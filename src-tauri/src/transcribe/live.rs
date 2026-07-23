//! Best-effort LIVE transcription: a read-only background loop that runs while a recording
//! is in progress. Every few seconds it snapshots the tail of the mic buffer, transcribes
//! that window with Whisper, and emits a [`crate::events::EVENT_LIVE_CAPTION`] event.
//!
//! Design guarantees (so this can never destabilise the core record/transcribe flow):
//! - It only *reads* a cloneable [`crate::audio::recorder::SampleReader`]; it never
//!   drains or mutates the capture buffer.
//! - It self-terminates as soon as the recorder is gone (recording stopped/taken).
//! - Every error (model load, resample, transcribe) is logged and skipped — the recording
//!   and the authoritative final transcript produced at stop are unaffected.
//! - Live quality/latency depends on the chosen model (use a small model for snappy
//!   captions); a slow tick just means less frequent captions, never a broken recording.
//! - MIC-ONLY until Stop: this loop transcribes the LOCAL MIC tail alone. The system-audio
//!   (far-side / other participants) stream is captured separately and is batch-transcribed only
//!   in the post-Stop `pipeline.rs` dual-stream merge — so the live captions AND the live `@brain`
//!   context reflect what YOU say during the call; the other side is folded in only after Stop.

use std::path::PathBuf;

use tauri::{AppHandle, Emitter, Manager};

use crate::state::AppState;
use crate::transcribe::Transcriber;

// NOTE (T1.5, 2026-07-09 transcription plan): the historical fixed `TICK = 3000 ms` sleep is
// now THERMAL-GOVERNED — `crate::thermal::ThermalGovernor::effective_tick()` returns the same
// 3 s at Nominal and stretches to 6 s / 9 s under Fair / Serious+ thermal pressure. Every
// "tick"-counted window below (wake dedup, hangovers, backstop budgets) is counted in TICKS,
// so under pressure those windows stretch in WALL-CLOCK terms with the tick — intended: the
// whole live layer backs off together.
// NOTE (Brain v2 L4): the fixed `REACTIONS_SCAN_EVERY = 7` cadence was replaced by the
// content-driven novelty gatekeeper (`crate::transcribe::novelty::NoveltyState`) — the hard
// minimum interval lives there (`MIN_FIRE_INTERVAL_TICKS`, ~15 s).
/// How many trailing seconds of audio to transcribe each tick (overlapping window).
const WINDOW_SECS: usize = 14;

/// How many consecutive ticks the SAME spoken wake stays de-duplicated. Consecutive ~14s tails
/// OVERLAP heavily (a `TICK` of 3s into a `WINDOW_SECS` of 14s ⇒ the same vocative is visible for
/// ~4-5 ticks), so without dedup one spoken "Klaudku" would re-fire every tick. A window of 5 ticks
/// (≈ 15s ≈ one full tail) collapses the overlapping re-detections of ONE utterance into a single
/// dispatch, while a fresh "Klaudku" later — or the same command after the window lapses — DOES
/// fire again. Recall is preserved (a NEW ask always catches); only the duplicate echo is dropped.
const WAKE_DEDUP_TICKS: u32 = 5;

#[derive(serde::Serialize, Clone)]
struct LiveCaption {
    text: String,
}

/// DEDUP state for the in-meeting wake trigger (the #23 fix). [`detect_wake`] now fires ANYWHERE in
/// the rolling, OVERLAPPING ~14s tail, so the SAME spoken "Klaudku zrób research" would otherwise
/// re-fire on every tick it remains visible. This collapses the overlapping re-detections of ONE
/// utterance into a SINGLE dispatch: it remembers the last fired wake (its normalized command text)
/// and a tick countdown, skipping a detection that matches the remembered one while the countdown is
/// live. A DIFFERENT command, or the SAME command after the window lapses, fires again — so one
/// spoken wake = one dispatch, but a fresh ask later in the same recording still catches.
///
/// Pure + stateful — held by the live loop, advanced once per tick via [`Self::tick`] and consulted
/// per wake hit via [`Self::should_fire`]. No I/O. Headless-testable.
#[derive(Default)]
struct WakeDedup {
    /// The normalized command of the last FIRED wake, while it is still suppressing repeats.
    last: Option<String>,
    /// Ticks remaining before `last` stops suppressing a matching repeat (0 = no active suppression).
    cooldown: u32,
}

impl WakeDedup {
    /// Advance one live tick: age out an expired suppression window. Call once per loop iteration,
    /// BEFORE the wake check, so the countdown reflects ticks elapsed since the last fire.
    fn tick(&mut self) {
        if self.cooldown > 0 {
            self.cooldown -= 1;
            if self.cooldown == 0 {
                self.last = None;
            }
        }
    }

    /// Decide whether a wake hit with normalized command `cmd` should FIRE (dispatch+emit) on this
    /// tick. Fires when it is a NEW/different command, or the suppression window for the same command
    /// has lapsed. On a fire it arms the suppression window ([`WAKE_DEDUP_TICKS`]); on a suppressed
    /// repeat it leaves the window untouched (so the cooldown counts from the FIRST fire, not the
    /// last echo — the overlap of one utterance can't extend the window indefinitely).
    fn should_fire(&mut self, cmd: &str) -> bool {
        let key = normalize_command(cmd);
        if self.cooldown > 0 && self.last.as_deref() == Some(key.as_str()) {
            return false; // overlapping re-detection of the same just-fired wake → skip.
        }
        self.last = Some(key);
        self.cooldown = WAKE_DEDUP_TICKS;
        true
    }

    /// Whether a wake-suppression window is currently ACTIVE (a wake just fired within the last
    /// [`WAKE_DEDUP_TICKS`] ticks). The VAD tick gate BYPASSES itself while this is true: right
    /// after a wake the user is mid-flow with the assistant, and a mis-gated tick there would be
    /// user-visible in a way a silent-lull skip never is.
    fn is_suppressing(&self) -> bool {
        self.cooldown > 0
    }
}

/// FLOOR on how many trailing seconds the VAD gate scans per tick. Absolute source-frame cursors,
/// rather than wall time, account for thermal/deferred ticks and slow model work without gaps.
const VAD_DELTA_SECS: usize = 3;

/// Overlap on top of the exact unseen source-frame count. It tolerates an in-flight callback and
/// gives VAD context at the boundary; continuity is still proven from absolute snapshot bounds.
const VAD_SCAN_HEADROOM_SECS: usize = 2;

/// Native-rate frames to inspect this tick. The first scan covers the entire rolling window so
/// speech captured while Whisper/VAD loaded is not skipped. Later scans cover every frame after
/// the last committed absolute cursor plus bounded overlap.
fn vad_scan_frame_count(
    last_vad_end_frame: Option<usize>,
    captured_end_frame: usize,
    source_rate: u32,
) -> usize {
    let rate = source_rate as usize;
    if rate == 0 {
        return 0;
    }
    let full_window = WINDOW_SECS.saturating_mul(rate);
    let Some(last_end) = last_vad_end_frame else {
        return full_window;
    };
    let unseen = captured_end_frame.saturating_sub(last_end);
    unseen
        .saturating_add(VAD_SCAN_HEADROOM_SECS.saturating_mul(rate))
        .clamp(VAD_DELTA_SECS.saturating_mul(rate), full_window)
}

/// A delta snapshot may advance the cursor only when it overlaps the prior committed end. A trim
/// that moved its start beyond the cursor is a detected gap and must fail open to full ASR.
fn vad_snapshot_covers_cursor(
    last_vad_end_frame: Option<usize>,
    snapshot_start_frame: usize,
    snapshot_end_frame: usize,
) -> bool {
    match last_vad_end_frame {
        None => true,
        Some(cursor) => snapshot_start_frame <= cursor && cursor <= snapshot_end_frame,
    }
}

/// How many extra ticks the gate keeps decoding after the LAST speech-positive tick. Bridges
/// short mid-sentence pauses (a 2-tick hangover ≈ 6 s at the nominal tick) so a caption never
/// cuts off on a breath; only a genuine lull stops the decode.
const VAD_HANGOVER_TICKS: u8 = 2;

/// T1.4 — the Silero VAD TICK GATE for the live loop: the PURE per-tick decision whether this
/// tick's whisper decode should run. Today the loop decodes the rolling window UNCONDITIONALLY
/// — even a fully silent meeting burns a full Metal encode every 3 s. The gate skips the decode
/// on silent ticks; everything downstream (captions, live buffer, bullets, reactions, wake)
/// consumes decoded TEXT, and silence produces none, so a skipped tick is behavior-identical to
/// a decoded-empty tick — minus the GPU burn.
///
/// Inputs per tick:
/// - `speech`: `Some(true/false)` = the CPU-only Silero verdict over the newest ~3 s delta;
///   `None` = VAD unavailable this tick (model absent / load or inference failed) ⇒ FAIL-OPEN,
///   decode (gate disabled = today's behavior — the gate may only ever REMOVE redundant work,
///   never a caption someone needed).
/// - `bypass`: manual voice-capture armed / wake-suppression window active ⇒ always decode
///   (those flows consume every tick's transcript directly).
///
/// Pure + stateful (hangover), no I/O — headless-testable ([`live_vad_gate_decision_matrix`]).
#[derive(Debug, Clone, Copy, Default)]
struct LiveVadGate {
    /// Remaining no-speech ticks that still decode after the last speech-positive tick.
    hangover: u8,
}

impl LiveVadGate {
    /// Decide whether THIS tick decodes. Speech re-arms the hangover; silence spends it;
    /// silence with the hangover spent ⇒ skip. Bypass/VAD-unavailable ⇒ decode (fail-open),
    /// leaving the hangover untouched.
    fn should_decode(&mut self, speech: Option<bool>, bypass: bool) -> bool {
        if bypass {
            return true;
        }
        match speech {
            None => true, // VAD unavailable ⇒ fail-open: today's decode-every-tick behavior.
            Some(true) => {
                self.hangover = VAD_HANGOVER_TICKS;
                true
            }
            Some(false) => {
                if self.hangover > 0 {
                    self.hangover -= 1;
                    true
                } else {
                    false
                }
            }
        }
    }
}

/// Coarse, non-PII MODEL-SIZE LABEL for the `live_perf` telemetry, derived from the ggml model
/// FILENAME only (never the path — §8): `ggml-small.bin` → `small`, `ggml-large-v3-q5_0.bin` →
/// `large-v3-q5_0`. A file that doesn't follow the ggml naming (an explicit
/// `whisper_model_path` pointing at an arbitrary user file) yields `custom` — its name is
/// deliberately NOT logged.
fn model_size_label(model_path: &std::path::Path) -> String {
    let stem = model_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    match stem.strip_prefix("ggml-") {
        Some(rest) if !rest.is_empty() => rest.to_string(),
        _ => "custom".to_string(),
    }
}

/// Normalize a wake command for dedup comparison: lowercase, collapse whitespace, drop surrounding
/// punctuation. So the SAME utterance transcribed with slightly different trailing punctuation /
/// spacing / casing across overlapping tails still compares equal and is de-duplicated.
fn normalize_command(cmd: &str) -> String {
    cmd.to_lowercase()
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Spawn the live-caption loop for the current recording. Returns immediately; the loop
/// runs on its own OS thread and ends on its own when recording stops.
pub(crate) fn spawn(
    app: AppHandle,
    meeting_id: String,
    model_path: PathBuf,
    lang: Option<String>,
    model_token: crate::perf::RecordingSessionToken,
    manual_clip_source: crate::audio::source::ManualClipSource,
) {
    let _ = std::thread::Builder::new()
        .name("murmur-live-captions".into())
        .spawn(move || {
            run(
                app,
                meeting_id,
                model_path,
                lang,
                model_token,
                manual_clip_source,
            )
        });
}

/// Wake/manual capture still consumes Whisper directly, so Parakeet cannot be selected without
/// creating a second resident ASR runtime. Keep the resolution pure and explicit until those paths
/// move behind the `LiveAsr` trait too.
fn effective_live_asr_engine(_requested: &str) -> &'static str {
    crate::transcribe::live_asr::ENGINE_WHISPER
}

fn run(
    app: AppHandle,
    meeting_id: String,
    model_path: PathBuf,
    lang: Option<String>,
    model_token: crate::perf::RecordingSessionToken,
    manual_clip_source: crate::audio::source::ManualClipSource,
) {
    // T1.5 — QoS: the caption tick is background inference; tag this thread UTILITY so macOS
    // schedules it onto efficiency cores under contention. Best-effort C call, never fatal.
    crate::thermal::set_utility_qos();
    // TP-F1 — advertise the live-loop as RUNNING only after its resident Whisper model loaded.
    // Begin must refuse during model load: arming earlier could leave a capture with no consumer if
    // load failed. The guard clears the flag on every later exit, including panic.
    struct LiveRunningGuard(AppHandle);
    impl Drop for LiveRunningGuard {
        fn drop(&mut self) {
            self.0
                .state::<AppState>()
                .live_running
                .store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }
    // Fresh transcript per recording: clear any leftover from a previous meeting so the in-meeting
    // assistant can never answer about a stale recording.
    if let Ok(mut lt) = app.state::<AppState>().live_transcript.lock() {
        lt.clear();
    }
    // Brain v2 L4: fresh bullets per recording too (belt-and-braces beside the
    // `start_recording`/`stop_recording` clears) — stale running notes must never seed a new
    // meeting's substrate or prompt inject.
    {
        let state = app.state::<AppState>();
        crate::transcribe::bullets::clear_ram(&state.live_bullets, &state.live_bullets_tracker);
    }
    // This lease represents the resident Whisper model, not merely one forward pass.
    // Holding it until the thread exits prevents a Brain sidecar/e5 generation from becoming a
    // second resident model between ticks, and makes Stop wait for the Transcriber to actually drop.
    let _live_model_residency = match crate::perf::acquire_recording_model_generation(
        &model_token,
        crate::perf::ResidentModelKind::Whisper,
    ) {
        Ok(lease) => lease,
        Err(e) => {
            tracing::debug!(target: "live", error = %e, "live model load deferred by recording lifecycle");
            return;
        }
    };
    let transcriber = match Transcriber::load(&model_path) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(target: "live", error = %e, "live captions disabled: model load failed");
            return;
        }
    };
    app.state::<AppState>()
        .live_running
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let _live_running_guard = LiveRunningGuard(app.clone());
    // Wake/manual capture still requires this Whisper handle. Loading Parakeet as the caption engine
    // here would therefore make TWO resident ASR runtimes under one lifecycle lease. Until those
    // optional paths are engine-agnostic, resolve every configured engine to Whisper for the whole
    // recording — one model, one resident lease, no hidden co-residency.
    let requested_engine = app
        .state::<AppState>()
        .config
        .lock()
        .map(|c| c.live_asr_engine.clone())
        .unwrap_or_else(|_| crate::transcribe::live_asr::ENGINE_WHISPER.to_string());
    let engine = effective_live_asr_engine(&requested_engine);
    if requested_engine != engine {
        tracing::info!(target: "live", requested = %requested_engine, "live ASR uses Whisper to preserve single-model residency");
    }
    let transcriber = std::sync::Arc::new(transcriber);
    let asr = crate::transcribe::live_asr::build_live_asr(engine, transcriber.clone());
    // The caption telemetry label follows the ACTUAL live engine, not the configured whisper size.
    let asr_label = asr.engine_label();

    // T1.4 — the Silero VAD tick gate (config `live_vad_gate`, default ON): load the tiny
    // (~885 kB) Silero model ONCE per recording, CPU-ONLY on purpose — a SECOND ggml Metal
    // context alongside the whisper context makes ggml's scheduler `ggml_abort` the process
    // (see `transcribe::vad::VadSegmenter::load`). Flag off / model absent / load failure ⇒
    // gate disabled (today's decode-every-tick behavior), logged ONCE here.
    let vad_gate_enabled = app
        .state::<AppState>()
        .config
        .lock()
        .map(|c| c.live_vad_gate)
        .unwrap_or(true);
    let mut vad = if vad_gate_enabled {
        match crate::transcribe::model::models_dir() {
            Ok(dir) => {
                let path = dir.join(crate::transcribe::model::VAD_MODEL_FILE);
                if path.is_file() {
                    match crate::transcribe::vad::VadSegmenter::load(&path) {
                        Ok(v) => Some(v),
                        Err(e) => {
                            tracing::warn!(target: "live", error = %e, "live VAD gate off (model load failed); decoding every tick");
                            None
                        }
                    }
                } else {
                    tracing::info!(target: "live", "live VAD gate off (VAD model not downloaded); decoding every tick");
                    None
                }
            }
            Err(e) => {
                tracing::debug!(target: "live", error = %e, "live VAD gate off (models dir unresolved); decoding every tick");
                None
            }
        }
    } else {
        None
    };
    let mut vad_gate = LiveVadGate::default();
    // Absolute native-rate end of the last successfully VAD-scanned or fully decoded snapshot.
    // `None` deliberately forces the FIRST post-model-load scan over the whole available 14 s.
    // Skipped/failed ticks never advance it, and every later delta must overlap it before a silent
    // verdict may suppress ASR.
    let mut last_vad_end_frame: Option<usize> = None;

    // T1.5 — thermal governor (tick-stretch + reactions pause + caption suspend under load) and
    // the one-tick user-turn decode defer. Recording + the post-Stop batch are NEVER touched.
    let mut governor = crate::thermal::ThermalGovernor::default();
    let mut turn_defer = crate::thermal::TurnDefer::default();

    // DEDUP state for the wake trigger (#23): detect_wake now fires ANYWHERE in the overlapping tail,
    // so without this the same spoken wake re-fires every tick it stays visible. Lives across ticks.
    let mut wake_dedup = WakeDedup::default();

    // PROACTIVE brain P1 state — FRESH per recording (its cooldown, session dedup, and scan
    // offset must never leak across recordings). See `crate::proactive` for the D1-D4 contract.
    let mut proactive = crate::proactive::ProactiveState::default();

    // REALTIME REACTIONS state — the light-engine contradiction scan runs OFF this tick thread (a
    // 5–10 s extraction inline would stall captions), skip-if-busy so ASR stays the priority
    // tenant (spec §4.2, review perf #7 / code-truth #11). Brain v2 L4: the fixed
    // every-7-ticks cadence is replaced by the NOVELTY GATEKEEPER below.
    let reactions_busy = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Brain v2 L4 state — FRESH per recording:
    // - `novelty` decides WHEN the reactions/bullets worker is worth spawning (new speech / a
    //   question / a known-entity mention / a lull, under a hard ~15 s floor);
    // - `entity_cache` is the visible-entity (id, name) list refreshed every ~60 ticks, shared
    //   between the gatekeeper's entity-hit trigger and the worker's scan (which previously
    //   re-fetched it per scan);
    // - whisper cards + proactive hints QUEUE (`pending` + the worker→tick `mpsc`) and emit at a
    //   conversational BOUNDARY (`boundary`), with drain-on-stop after the loop. Event names +
    //   payloads are UNCHANGED — only the emit TIMING moved.
    let mut novelty = crate::transcribe::novelty::NoveltyState::default();
    let mut entity_cache = crate::transcribe::novelty::RefreshableEntityCache::default();
    let mut boundary = crate::transcribe::novelty::BoundaryGate::default();
    let (surface_tx, surface_rx) = std::sync::mpsc::channel::<QueuedSurface>();
    let mut pending: Vec<QueuedSurface> = Vec::new();
    let mut last_caption = String::new();

    loop {
        // T1.5 — the sleep is thermal-governed: 3 s nominal (the historical TICK), stretching to
        // 6 s / 9 s as the Mac heats up. Wait on the recording-session Condvar instead of sleeping:
        // Live timeout means run the tick, while Draining/abort wakes immediately so Stop never waits
        // for the resident Whisper lease until the end of a thermal interval.
        if !model_token.wait_for_live_tick(governor.effective_tick()) {
            break;
        }
        // One guarded FFI read per completed tick; degrade-to-Nominal.
        governor.observe(crate::thermal::read_thermal_level());
        // Age out an expired wake-suppression window once per tick (before this tick's wake check).
        wake_dedup.tick();

        // PROACTIVE HINTS (zero-egress): advance the per-recording throttle every tick; on every
        // K-th tick — only while `proactive_hints_enabled` is ON — the deterministic matcher scans
        // the NEW live-tail delta against the gated local substrates (entities / commitments /
        // facts / FTS) and surfaces AT MOST one recall card. Fully local: no provider, no consent,
        // no egress. Best-effort — it can never disrupt the caption or the recording. Brain v2 L4:
        // the hint is QUEUED and emitted at the next conversational boundary (its own scan logic —
        // throttle, dedup, scoring — is untouched; only the emit moved).
        if let Some(hint) = crate::proactive::scan_tick(&app, &mut proactive) {
            pending.push(QueuedSurface::Hint(hint));
        }

        // NOVELTY GATEKEEPER (Brain v2 L4): the visible-entity cache ticks (a gated
        // `list_entities_visible` read every ~60 ticks over the FRESH unlocked set — staleness
        // affects trigger sensitivity only, never content; every downstream fact/content read
        // re-gates itself), then the gatekeeper folds this tick's live-buffer delta into its
        // pending triggers and decides whether the worker fires NOW.
        let entities: Vec<(String, String)> = {
            let state = app.state::<AppState>();
            entity_cache
                .on_tick(|| {
                    let unlocked = state
                        .unlocked_folders
                        .lock()
                        .map(|u| u.clone())
                        .unwrap_or_default();
                    state
                        .db
                        .list_entities_visible(&unlocked)
                        .map(|v| v.into_iter().map(|n| (n.id, n.name)).collect())
                        .unwrap_or_else(|e| {
                            tracing::debug!(target: "live", error = %e, "entity cache refresh failed; keeping empty");
                            Vec::new()
                        })
                })
                .to_vec()
        };
        let live_buf = app
            .state::<AppState>()
            .live_transcript
            .lock()
            .map(|b| b.clone())
            .unwrap_or_default();
        let novelty_tick = novelty.on_tick(&live_buf, &entities);

        // REALTIME REACTIONS + LIVE BULLETS (Brain Live / Brain v2 L4): when the gatekeeper fires
        // and no scan is in flight, run the worker on its OWN thread (never this tick thread):
        // FIRST the incremental bullets update (`bullets_tick` — flag- and stub-gated, it is the
        // substrate the scan reads), THEN the light-engine contradiction scan. Cards are SENT back
        // over the mpsc queue and emitted at a boundary — or, in shadow mode, counted. Skip-if-
        // busy: a slow scan drops this trigger; the recording never waits on the LLM.
        // T1.5: under `Serious`+ thermal pressure the reactions/bullets worker is PAUSED
        // (checked BEFORE the busy-flag swap so a paused tick can't wedge the flag).
        if novelty_tick.fire
            && !governor.reactions_paused()
            // Deliberate priority policy: the resident live Whisper owns the ONE model lane.
            // Reactions/bullets degrade for this tick instead of spawning a doomed Brain worker.
            && crate::perf::recording_model_lane_is_free(&model_token)
            && !reactions_busy.swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            let app2 = app.clone();
            let busy2 = std::sync::Arc::clone(&reactions_busy);
            let tx2 = surface_tx.clone();
            let entities2 = entities.clone();
            std::thread::spawn(move || {
                // T1.5 — QoS: the reactions worker is background inference too → UTILITY.
                crate::thermal::set_utility_qos();
                // RAII: reset the busy flag on EVERY exit — including a panic inside reactions_scan.
                // Without this, one panic would wedge the flag `true` and silently kill ALL further
                // reaction scans this recording (deep-review: worker-thread-panic stuck-busy).
                struct BusyReset(std::sync::Arc<std::sync::atomic::AtomicBool>);
                impl Drop for BusyReset {
                    fn drop(&mut self) {
                        self.0.store(false, std::sync::atomic::Ordering::Release);
                    }
                }
                let _reset = BusyReset(busy2);
                // L4: bullets BEFORE the scan — the scan's window reads the just-updated bullets.
                crate::transcribe::bullets::bullets_tick(&app2);
                let now = chrono::Utc::now().to_rfc3339();
                let scan = crate::brain_reactions::reactions_scan(&app2, &now, &entities2);
                let n = scan.cards.len() as u64;
                if scan.emit {
                    for card in scan.cards {
                        // Boundary-timed surfacing: queue to the tick thread. The send fails only
                        // when the loop has ended (receiver dropped) — the card is then dropped
                        // (the recording is over; a late card about it has no surface).
                        let _ = tx2.send(QueuedSurface::Whisper(card));
                    }
                } else if n > 0 {
                    // Shadow mode: count would-have-fired for user-local calibration; emit nothing.
                    app2.state::<AppState>()
                        .reactions_shadow_count
                        .fetch_add(n, std::sync::atomic::Ordering::Relaxed);
                }
            });
        }

        // BOUNDARY-TIMED SURFACING (Brain v2 L4): fold in any worker-queued cards, then emit the
        // whole queue at a conversational boundary — a short lull, the (previous tick's) caption
        // ending a sentence, or the 30 s max-hold force-emit. `last_caption` lags one tick by
        // construction (this runs before this tick's transcription) — an accepted ~3 s skew on
        // the sentence-final signal, not on the lull.
        while let Ok(item) = surface_rx.try_recv() {
            pending.push(item);
        }
        if boundary.on_tick(
            !pending.is_empty(),
            novelty_tick.new_chars > 0,
            &last_caption,
        ) {
            for item in pending.drain(..) {
                emit_surface(&app, item);
            }
        }

        // Manual capture has its own absolute source-frame cap and is finalized independently of
        // the ordinary caption decode. This runs BEFORE every early `continue` below, so thermal
        // suspension, a short live tail, VAD, resample failure or live-ASR failure can never wedge
        // the FE in "listening". While still armed it performs no extra ASR; the exact post-click
        // range is transcribed once, only after the user stops or the hard source-frame cap lands.
        let manual_decision = step_manual_capture(
            &app,
            transcriber.as_ref(),
            lang.as_deref(),
            &manual_clip_source,
        );
        if let Some(decision) = manual_decision {
            let awaiting_generation = match &decision {
                ManualCaptureDecision::AwaitingDurable { generation } => Some(*generation),
                _ => None,
            };
            let terminal = !matches!(
                &decision,
                ManualCaptureDecision::KeepListening
                    | ManualCaptureDecision::AwaitingDurable { .. }
            );
            apply_manual_capture_decision(&app, &meeting_id, &model_token, decision);
            if terminal {
                continue;
            }
            if let Some(generation) = awaiting_generation {
                // The exact range is owned by the stable spool handle. Do not consult the recorder
                // slot (Stop may already have taken it), and do not impose a second arbitrary
                // timeout: `spool_finished` is the producer's terminal proof. Retry at a small,
                // bounded polling cadence until the prefix becomes durable or the spool exits.
                tracing::trace!(target: "voice", generation, "manual command waiting for certified spool prefix");
                std::thread::sleep(std::time::Duration::from_millis(25));
                continue;
            }
        }

        // Check recorder liveness without cloning audio. The optional caption throttles below must
        // run BEFORE the 14 s native-rate snapshot: at 384 kHz that snapshot is a 21.5 MiB atomic
        // copy (and may retry if a durable trim crosses it), which is pure waste on a tick we have
        // already decided not to decode. This cheap probe preserves prompt self-termination while
        // letting thermal/defer skips avoid the copy entirely.
        let recorder_status = {
            let state = app.state::<AppState>();
            let guard = match state.recorder.lock() {
                Ok(g) => g,
                Err(_) => break,
            };
            guard.as_ref().map(|recorder| {
                (
                    recorder.sample_reader(),
                    recorder.total_samples(),
                    recorder.source_sample_rate(),
                )
            })
        };
        let Some((sample_reader, captured_frames, source_rate)) = recorder_status else {
            break; // recorder taken → recording stopped
        };
        if source_rate == 0 || captured_frames < source_rate as usize {
            continue; // preserve the historical <1 s warm-up without cloning the short tail
        }

        // A just-fired wake is mid-flow and bypasses the optional caption throttles. Manual
        // capture no longer belongs here: its bounded one-shot finalizer ran above and needs no
        // repeated 14 s caption decode while the user is still speaking.
        let bypass = wake_dedup.is_suppressing();

        // T1.5 — CRITICAL thermal: suspend the caption decode entirely (the recording and the
        // post-Stop batch pipeline are untouched; the loop keeps ticking so recorder-gone
        // still ends it). Checked after the cheap recorder-presence probe but BEFORE the expensive
        // native-rate snapshot. Manual capture already progressed above; only the wake-suppression
        // flow bypasses this optional caption throttle.
        if governor.captions_suspended() && !bypass {
            tracing::debug!(target: "live", "critical thermal state; caption decode suspended this tick");
            continue;
        }

        // TURN-DEFER (T1.5): while a user-initiated assistant turn runs on the LOCAL GGUF
        // brain (the shared-Metal co-residency spike), skip ONE decode tick per turn window —
        // the same flag-read discipline as `brain_reactions::should_defer_scan`. Evaluated
        // ONLY on non-bypass ticks so a bypassed tick can't consume the turn window's single
        // defer without an actual skip (and the resolve isn't computed needlessly). Trade-off:
        // a turn window that both starts and ends entirely inside a bypass stretch leaves
        // `deferred_for_current` armed, costing the NEXT local turn its one defer — fail-safe
        // (fewer skips), never an extra skip.
        if !bypass {
            let user_turn = app
                .state::<AppState>()
                .user_turn_in_progress
                .load(std::sync::atomic::Ordering::Relaxed);
            let live_local = user_turn
                && app
                    .state::<AppState>()
                    .config
                    .lock()
                    .map(|c| {
                        crate::summarize::roles::resolve(crate::summarize::roles::Role::Live, &c)
                            .connection
                            == crate::summarize::roles::CONN_LOCAL
                    })
                    .unwrap_or(false);
            if turn_defer.should_skip(user_turn, live_local) {
                tracing::debug!(target: "live", "user turn on the local brain; deferring one decode tick");
                continue;
            }
        }

        // TWO-PHASE VAD TICK GATE (T1.4): first copy + resample only the exact unseen native-rate
        // span plus bounded overlap. The SampleReader was cloned under the recorder mutex above,
        // but every O(window) atomic copy happens OUTSIDE it so Stop is never blocked by a 14 s
        // snapshot. Absolute bounds prove continuity; contention or a trim gap fails open to full
        // ASR and never masquerades as silence.
        let mut completed_vad_scan_end = None;
        let speech: Option<bool> = match vad.as_mut() {
            Some(v) if !bypass => {
                let frames = vad_scan_frame_count(
                    last_vad_end_frame,
                    sample_reader.total_samples(),
                    source_rate,
                );
                match sample_reader.snapshot_tail(frames) {
                    Err(e) => {
                        // Exhausted trim-race retries are NOT silence. Preserve the cursor and fail
                        // open to the full snapshot below.
                        tracing::debug!(target: "live", error = %e, "live VAD tail snapshot contended; decoding");
                        None
                    }
                    Ok(snapshot) if snapshot.samples.is_empty() => {
                        // Defensive only (the liveness probe saw >=1 s). Empty input cannot prove
                        // silence, so preserve the cursor and decode.
                        None
                    }
                    Ok(snapshot)
                        if !vad_snapshot_covers_cursor(
                            last_vad_end_frame,
                            snapshot.start_frame,
                            snapshot.end_frame,
                        ) =>
                    {
                        tracing::debug!(
                            target: "live",
                            cursor = ?last_vad_end_frame,
                            snapshot_start = snapshot.start_frame,
                            snapshot_end = snapshot.end_frame,
                            "live VAD cursor gap detected; decoding"
                        );
                        None
                    }
                    Ok(snapshot) => {
                        let vad_samples_16k = match crate::audio::resample_to_16k(
                            &snapshot.samples,
                            source_rate,
                        ) {
                            Ok(samples) => samples,
                            Err(e) => {
                                // Preserve the prior cursor on failure: the next tick re-scans
                                // everything since the last successful VAD input.
                                tracing::debug!(target: "live", error = %e, "live VAD-span resample failed");
                                continue;
                            }
                        };
                        match v.speech_regions(&vad_samples_16k) {
                            Ok(regions) => {
                                completed_vad_scan_end = Some(snapshot.end_frame);
                                Some(!regions.is_empty())
                            }
                            Err(e) => {
                                tracing::debug!(target: "live", error = %e, "live VAD tick failed; decoding");
                                None
                            }
                        }
                    }
                }
            }
            _ => {
                // Bypass or no VAD loaded: decode unconditionally. The absolute cursor advances
                // only after the full snapshot resamples successfully below.
                None
            }
        };
        // Stage the hangover transition. If the admitted full-window resample fails, the old path
        // never reached VAD and therefore changed neither cursor nor hangover; committing only after
        // a successful full input preserves that retry behavior. A VAD-rejected silent tick needs no
        // full input, so its completed scan + hangover transition commit immediately.
        let mut next_vad_gate = vad_gate;
        if !next_vad_gate.should_decode(speech, bypass) {
            vad_gate = next_vad_gate;
            if let Some(scanned_end) = completed_vad_scan_end {
                last_vad_end_frame = Some(scanned_end);
            }
            continue;
        }

        // Gate admitted a caption decode. Check the affine recording phase before materializing the
        // full tail; Stop transitions it out of Live before waiting for this worker to quiesce.
        if model_token.validated_for_live_work().is_err() {
            break;
        }
        let snapshot = match sample_reader
            .snapshot_tail(WINDOW_SECS.saturating_mul(source_rate as usize))
        {
            Ok(snapshot) => snapshot,
            Err(e) => {
                tracing::debug!(target: "live", error = %e, "live full-tail snapshot contended");
                continue; // preserve VAD cursor/hangover and retry next tick
            }
        };
        if source_rate == 0 || snapshot.samples.len() < source_rate as usize {
            continue; // <1s captured so far — nothing worth transcribing yet
        }
        let samples_16k = match crate::audio::resample_to_16k(&snapshot.samples, source_rate) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!(target: "live", error = %e, "live resample tick failed");
                continue;
            }
        };
        vad_gate = next_vad_gate;
        last_vad_end_frame = Some(snapshot.end_frame);

        // Stop can arrive during the O(window) resample. Recheck immediately before native ASR so
        // no stale Whisper decode delays the draining barrier.
        if model_token.validated_for_live_work().is_err() {
            break;
        }

        // LIVE captions use the Fast profile via the `LiveAsr` seam — whisper's greedy/best_of:1
        // profile OR the CPU-only parakeet engine (config `live_asr_engine`), NOT the batch
        // beam-search path. Captions tick every few seconds on overlapping windows, so latency must
        // dominate. The authoritative high-quality transcript is produced once at Stop (pipeline.rs)
        // — ALWAYS by whisper (parakeet is live-only), so the seam here never affects note quality.
        let decode_started = std::time::Instant::now();
        let decoded = asr.transcribe_live(&samples_16k, lang.as_deref());
        // T0.1 — per-tick decode telemetry (target `live_perf`): durations / window length /
        // coarse engine label ONLY — never content, never paths (§8). The in-app instrument
        // behind `scripts/measure-live-power.sh`. `model` = the ACTUAL live engine (whisper/
        // parakeet); `whisper_size` = the loaded whisper batch size (unchanged coarse label).
        tracing::info!(
            target: "live_perf",
            decode_ms = decode_started.elapsed().as_millis() as u64,
            window_s = samples_16k.len() as f64 / 16_000.0,
            model = %asr_label,
            whisper_size = %model_size_label(&model_path),
            ok = decoded.is_ok(),
            "live decode tick"
        );
        match decoded {
            Ok(t) => {
                let text = t
                    .segments
                    .iter()
                    .map(|s| s.text.trim())
                    .collect::<Vec<_>>()
                    .join(" ")
                    .trim()
                    .to_string();

                // Accumulate the rolling caption into the live transcript (best-effort, read-only) so
                // the in-meeting assistant can answer questions about the recording IN PROGRESS.
                // Deduped (overlapping tails) + size-bounded inside `accumulate_live_caption`.
                accumulate_live_caption(app.state::<AppState>().inner(), &text);

                if !text.is_empty() {
                    // Brain v2 L4: remember the latest caption for the NEXT tick's boundary check
                    // (sentence-final punctuation = a good moment to surface queued cards).
                    last_caption.clone_from(&text);
                    // In-meeting voice trigger (Phase A: DETECT + SURFACE only — NO action dispatch;
                    // that needs the local brain in a later phase). Best-effort + panic-free: the
                    // detection is a pure function and the emit error is ignored, so a wake miss/hit
                    // can NEVER disrupt the caption or the authoritative record/transcribe flow.
                    // DEDUP (#23): the rolling tail OVERLAPS, so a single spoken "Klaudku …" is
                    // visible for several ticks and `detect_wake` (now firing anywhere) would re-hit
                    // it each tick. `should_fire` collapses those overlapping re-detections of the
                    // SAME command into ONE dispatch, while a DIFFERENT command — or the same one
                    // after the suppression window lapses — still fires. Skipped repeats produce
                    // neither a dispatch nor an `EVENT_WAKE_DETECTED`, but the caption still emits.
                    if let Some(payload) =
                        wake_event_for(&text).filter(|p| wake_dedup.should_fire(&p.command))
                    {
                        // PII rule (§8): NEVER log the spoken command text — only the
                        // non-PII wake token and a coarse intent KIND (variant
                        // discriminant, no content). The full payload goes to the FE
                        // event, not the log.
                        let intent_kind = match &payload.intent {
                            crate::audio::wake::VoiceIntent::Research { .. } => "research",
                            crate::audio::wake::VoiceIntent::SlackSearch { .. } => "slack_search",
                            crate::audio::wake::VoiceIntent::Recall { .. } => "recall",
                            crate::audio::wake::VoiceIntent::CreateReminder { .. } => {
                                "create_reminder"
                            }
                            crate::audio::wake::VoiceIntent::NoteAside { .. } => "note_aside",
                            crate::audio::wake::VoiceIntent::Unknown { .. } => "unknown",
                        };
                        // Phase E (Flow B): when the opt-in `realtime_reactions` toggle is ON,
                        // DISPATCH the parsed action over the gated vault + consent-gated brain on a
                        // DETACHED thread (the brain call can take seconds — it must NEVER block the
                        // transcription tick or the caption). When OFF (default) we only SURFACE the
                        // hit, exactly as before. A dispatch panic is contained to its own thread.
                        let dispatch = {
                            let state = app.state::<AppState>();
                            state
                                .config
                                .lock()
                                .map(|c| should_dispatch(&c))
                                .unwrap_or(false)
                        };
                        tracing::info!(
                            target: "voice",
                            matched = %payload.matched_phrase,
                            intent = intent_kind,
                            dispatch,
                            "wake word detected in live caption"
                        );
                        if dispatch {
                            // No FE thread here (wake-word turn) → the turn generates its own id.
                            // No FE meeting_id (wake twin) → scope falls back to the live recording.
                            spawn_assistant_turn_with_token(
                                app.clone(),
                                payload.command.clone(),
                                None,
                                None,
                                Some(model_token.clone()),
                            );
                        }
                        let _ = app.emit(crate::events::EVENT_WAKE_DETECTED, payload);
                    }
                    let _ = app.emit(crate::events::EVENT_LIVE_CAPTION, LiveCaption { text });
                }
            }
            Err(e) => tracing::debug!(target: "live", error = %e, "live transcribe tick failed"),
        }
    }

    // DRAIN-ON-STOP (Brain v2 L4): the recording ended (recorder gone / poisoned lock) — emit
    // everything still queued so a boundary-held card is never silently lost with the recording.
    // A worker STILL in flight past this point sends to a dropped receiver and its cards are
    // dropped (the recording is over; there is no live surface left for them).
    while let Ok(item) = surface_rx.try_recv() {
        pending.push(item);
    }
    for item in pending.drain(..) {
        emit_surface(&app, item);
    }
}

/// One queued live surface (Brain v2 L4 boundary-timed surfacing): a Realtime-Reactions whisper
/// card or a proactive recall hint, held on the tick thread until a conversational boundary.
/// Event names + payloads are the UNCHANGED existing contracts — only the emit timing moved.
enum QueuedSurface {
    Whisper(crate::brain_reactions::WhisperCard),
    Hint(crate::events::ProactiveHintPayload),
}

/// Emit one queued surface on its ORIGINAL event channel (payloads unchanged — FE untouched).
/// Best-effort; PII rule (§8): the hint log carries kind + score only, never title/content.
fn emit_surface(app: &AppHandle, item: QueuedSurface) {
    match item {
        QueuedSurface::Whisper(card) => {
            let _ = app.emit(crate::events::EVENT_WHISPER_CARD, card);
        }
        QueuedSurface::Hint(hint) => {
            tracing::info!(
                target: "proactive",
                kind = %hint.kind,
                score = hint.score,
                "proactive hint emitted"
            );
            let _ = app.emit(crate::events::EVENT_PROACTIVE_HINT, hint);
        }
    }
}

/// The single decision point for whether a wake hit DISPATCHES an in-meeting action (Flow B) vs
/// only being surfaced (Phase A). Pure, so the off-vs-on behaviour is headless-testable. Dispatch
/// happens ONLY when the opt-in `realtime_reactions` toggle is ON — OFF (the default) is exactly
/// today's behaviour: wake detected + surfaced via `EVENT_WAKE_DETECTED`, no dispatch.
fn should_dispatch(config: &crate::settings::AppConfig) -> bool {
    config.realtime_reactions
}

// ── Phase 5 tier system-prompt SUFFIXES — SINGLE-SOURCED in `crate::prompts` (Brain v2 L3) ──────
// Moved verbatim; re-exported here so this module (and any historical `live::TIER*_SUFFIX` path)
// keeps compiling. Each is appended to the shared `assistant_system_prompt`; the actual tool
// boundary stays STRUCTURAL (`AssistantScope`), and the `__ESCALATE__` token is drift-guarded
// against `crate::agent::ESCALATE_SENTINEL` in `prompts::tests`.
pub(crate) use crate::prompts::{TIER1_SUFFIX, TIER2_SUFFIX, TIER3_SUFFIX};

/// Resolve the turn's THREAD id: the FE-supplied id when present (an @brain thread), else a fresh
/// UUID v4 (the voice/wake path and the single-shot text composer send none), so EVERY persisted
/// assistant exchange carries a thread identity going forward. Blank ids count as absent — an
/// empty thread id must never be persisted. The id is OPAQUE — no PII (safe to log/emit).
pub(crate) fn ensure_thread_id(explicit: Option<String>) -> String {
    match explicit {
        Some(t) if !t.trim().is_empty() => t,
        _ => uuid::Uuid::new_v4().to_string(),
    }
}

/// Resolve the SCOPE meeting for an assistant turn — the id the brain grounds "this meeting" in
/// (both `GatedToolExecutor.meeting_id` and `gated_live_context`). Precedence (Phase 6 generalizes
/// the Phase-4 fix): an EXPLICIT FE-sent `meeting_id` (a bound past/anchored @brain thread) WINS
/// over the FOCUS pointer (`state.focus_meeting` — the meeting the user is looking at), which in
/// turn WINS over `state.current_meeting` (the recording pointer, `Some` only while recording). So:
/// a bound thread scopes to ITS meeting even while a different one records; an idle user viewing a
/// past meeting scopes to THAT meeting (the exact idle wrong-meeting root); and a voice/wake twin
/// that sends none AND has no focus still falls back to the live recording — so recording keeps
/// working. A blank/whitespace id at ANY level counts as ABSENT. The empty string means "no scope"
/// (no FE id, no focus, not recording) — the fail-closed default `gated_live_context` treats as
/// not-visible. PURE + headless-testable; no PII (opaque ids only).
pub(crate) fn resolve_scope_meeting(
    fe_meeting_id: Option<&str>,
    focus_meeting: Option<&str>,
    current_meeting: Option<&str>,
) -> String {
    let norm = |s: Option<&str>| -> Option<String> {
        s.map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    norm(fe_meeting_id)
        .or_else(|| norm(focus_meeting))
        .or_else(|| norm(current_meeting))
        .unwrap_or_default()
}

/// Spawn ONE assistant turn on a DETACHED OS thread — the SINGLE entry to the in-meeting brain for the
/// wake path, the manual button, AND the text composer (all three funnel here with a `command` string).
/// The brain + tools can take seconds, so it MUST run off-thread; the whole body is best-effort +
/// panic-free and runs on its own thread, so even an unexpected panic is contained and can never
/// disrupt recording or the caption.
///
/// Brain v2 P0.3 — IN-FLIGHT DEDUP + USER-TURN PRIORITY:
/// - At most ONE turn per scope key (the FE-sent `meeting_id`, "" for the voice/wake path) runs at a
///   time. A second spawn while one is in flight is DROPPED ([`try_begin_turn`]) — the overlapping-
///   wake / double-click pile-up guard, so duplicate turns never stack generations on Metal. The
///   already-running turn still resolves the FE's pending card via `EVENT_VOICE_ACTION_RESULT`.
/// - While the turn runs, `AppState::user_turn_in_progress` is `true` so the background
///   Realtime-Reactions scan defers. Both the per-key decrement and the flag reset are guaranteed on
///   EVERY exit path (including a panic inside the turn, and a failed thread spawn) by the RAII
///   [`TurnGuard`] moved into the worker closure.
pub fn spawn_assistant_turn(
    app: AppHandle,
    command: String,
    thread_id: Option<String>,
    meeting_id: Option<String>,
) {
    spawn_assistant_turn_with_token(app, command, thread_id, meeting_id, None);
}

fn spawn_assistant_turn_with_token(
    app: AppHandle,
    command: String,
    thread_id: Option<String>,
    meeting_id: Option<String>,
    recording_token: Option<crate::perf::RecordingSessionToken>,
) {
    // Avoid allocating a known-doomed worker: live Whisper deliberately owns residency for the
    // whole recording. An on-device Brain turn can run only if final ActiveRecording integration
    // supplied the exact token AND the lane is actually free.
    if crate::perf::recording_has_priority() {
        let local_live = app
            .state::<AppState>()
            .config
            .lock()
            .map(|config| {
                matches!(
                    crate::summarize::roles::resolve(crate::summarize::roles::Role::Live, &config,)
                        .connection
                        .as_str(),
                    crate::summarize::roles::CONN_LOCAL | crate::summarize::roles::CONN_AFM
                )
            })
            .unwrap_or(true);
        if local_live
            && !recording_token
                .as_ref()
                .is_some_and(crate::perf::recording_model_lane_is_free)
        {
            tracing::debug!(target: "voice", "local live Brain worker not spawned while Whisper owns residency");
            return;
        }
    }
    // Scope key = the FE-sent meeting id (matching how the turn scopes itself); the voice/wake twin
    // sends none → the shared "" key. Opaque id only — no PII.
    let key = meeting_id
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    {
        let state = app.state::<AppState>();
        if !try_begin_turn(&state.in_flight_turns, &key) {
            // NO PII: no command text, no id content — just the dedup decision.
            tracing::debug!(target: "voice", "turn dedup: turn already in flight; dropping");
            return;
        }
    }
    // The guard is created NOW and moved into the closure: if the spawn itself fails, the closure
    // (and the guard inside it) is dropped → the counter is decremented and the flag cleared, so a
    // failed spawn can never wedge the dedup registry.
    let guard = TurnGuard {
        app: app.clone(),
        key,
    };
    let _ = std::thread::Builder::new()
        .name("murmur-assistant-turn".into())
        .spawn(move || {
            let _guard = guard; // held for the WHOLE turn; Drop runs on every exit incl. panic.
            _guard
                .app
                .state::<AppState>()
                .user_turn_in_progress
                .store(true, std::sync::atomic::Ordering::Relaxed);
            run_assistant_turn(&app, command, thread_id, meeting_id, recording_token)
        });
}

/// Brain v2 P0.3 — the PURE in-flight registry decision: begin a turn for `key` unless one is
/// already in flight. Returns `true` (and increments the counter) when the caller may proceed;
/// `false` when a turn for the same key is already running (the caller drops the duplicate). A
/// poisoned lock is recovered via `into_inner` — the map is a plain counter registry, always valid.
/// Factored off `AppHandle` so the dedup contract is headless-testable.
pub(crate) fn try_begin_turn(
    registry: &std::sync::Mutex<std::collections::HashMap<String, u32>>,
    key: &str,
) -> bool {
    let mut map = match registry.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let count = map.entry(key.to_string()).or_insert(0);
    if *count > 0 {
        return false;
    }
    *count += 1;
    true
}

/// Brain v2 P0.3 — end a turn for `key`: decrement its in-flight count (saturating) and drop the
/// entry at zero so the registry never grows unboundedly across meetings. Counterpart of
/// [`try_begin_turn`]; called from [`TurnGuard::drop`] (and directly by tests).
pub(crate) fn end_turn(
    registry: &std::sync::Mutex<std::collections::HashMap<String, u32>>,
    key: &str,
) {
    let mut map = match registry.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    if let Some(count) = map.get_mut(key) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            map.remove(key);
        }
    }
}

/// RAII guard for ONE assistant turn (Brain v2 P0.3): on drop — normal return, early return, panic
/// inside the turn, or a failed thread spawn — it clears the user-turn priority flag and decrements
/// the per-key in-flight counter. Mirrors the panic-safe `BusyReset` pattern of the reactions worker
/// (see `run`), so a wedged flag/counter can never silently kill all further turns or scans.
struct TurnGuard {
    app: AppHandle,
    key: String,
}
impl Drop for TurnGuard {
    fn drop(&mut self) {
        let state = self.app.state::<AppState>();
        state
            .user_turn_in_progress
            .store(false, std::sync::atomic::Ordering::Relaxed);
        end_turn(&state.in_flight_turns, &self.key);
    }
}

/// The CARD path (wake / manual button / single-shot text composer): run the shared query core, then
/// EMIT `EVENT_VOICE_ACTION_RESULT` so the assistant card resolves the pending row. The per-tool trace
/// streams via `EVENT_ASSISTANT_TOOL`. A missing `thread_id` (voice/wake) is backend-generated here,
/// so the persisted row + every emitted payload of this turn carry the SAME thread identity.
/// `meeting_id` is the OPTIONAL FE-supplied scope meeting (Phase 4): when present it wins over
/// `state.current_meeting`; the voice/wake twin sends none, so the live recording still scopes.
fn run_assistant_turn(
    app: &AppHandle,
    command: String,
    thread_id: Option<String>,
    meeting_id: Option<String>,
    recording_token: Option<crate::perf::RecordingSessionToken>,
) {
    let thread_id = ensure_thread_id(thread_id);
    let result = run_assistant_query(
        app,
        &command,
        &command,
        crate::events::EVENT_ASSISTANT_TOOL,
        &thread_id,
        None,
        meeting_id.as_deref(),
        None, // the voice/wake/card twin has no FE source picker → whole-vault, unchanged.
        recording_token,
    );
    let _ = app.emit(crate::events::EVENT_VOICE_ACTION_RESULT, result);
}

/// THE shared query core — the brain's executive entry, reused by the card turn AND the chat panel.
/// 1. Re-snapshot config + the LIVE `unlocked` set + the current meeting id — FRESH per turn (C6).
/// 2. Resolve `command` (the user's LATEST message) to an intent (keyword → brain → fallback). This is
///    NO LONGER a write router — the agentic loop's tool-use DECIDES whether to answer or write (the
///    user's explicit "no hardcoded regex" philosophy). The resolved `intent` is used ONLY as the
///    deterministic FLOOR's read fan-out + its surfaced `intent_kind` (so the no-consent/local path is
///    unchanged and there is no read-answer regression).
/// 3. EVERY request (voice / text / @brain) runs the model-driven AGENTIC LOOP (now `allow_writes`)
///    over `loop_user` (= `command` for the card; = the whole conversation for the chat). The agent
///    picks `save_note` / `create_reminder` (gated) or answers. On non-convergence / `Unavailable` (no
///    cloud consent) / the local-or-stub backend it FALLS THROUGH to the deterministic floor — the
///    floor no longer owns write routing; it is the informational safety net (ZERO read regression).
/// 4. PERSIST the interaction (gated, purged-on-seal) + RETURN the result; the per-tool trace streams
///    to `tool_event` (card vs chat). The CALLER delivers the result (emit for the card, return for chat).
///
/// `thread_id` is the RESOLVED thread identity of this turn (callers run [`ensure_thread_id`] first)
/// — it is threaded onto every trace payload, the returned result, and the persisted row, so
/// simultaneous threads never cross-attribute. `anchor_text` is the note text an @brain thread was
/// anchored to (persisted with the row; `None` for voice/unanchored turns).
///
/// `fe_meeting_id` (Phase 4) is the EXPLICIT scope meeting the FE binds this thread to. It is
/// resolved as `fe_meeting_id.or(state.focus_meeting).or(state.current_meeting)` via
/// [`resolve_scope_meeting`] (Phase 6): an explicit FE id WINS (so a past/anchored @brain thread
/// scopes to ITS meeting even while a DIFFERENT meeting records, and "o czym to spotkanie" on an
/// idle thread no longer defaults to a vault-wide arbitrary meeting); when the FE sends none, the
/// FOCUS pointer (the meeting the user is viewing) applies — the backend safety-net for the
/// voice/wake fallback path; and only when there is neither does the live-recording pointer apply.
/// The RESOLVED id is used for the executor scope, `gated_live_context`, AND the persisted
/// thread binding — so a thread durably answers about its OWN meeting.
#[allow(clippy::too_many_arguments)] // cohesive @brain entry: fe pointers + thread/anchor + pinned sources.
pub(crate) fn run_assistant_query(
    app: &AppHandle,
    command: &str,
    loop_user: &str,
    tool_event: &'static str,
    thread_id: &str,
    anchor_text: Option<&str>,
    fe_meeting_id: Option<&str>,
    explicit_sources: Option<&[crate::storage::models::SourceRef]>,
    recording_token: Option<crate::perf::RecordingSessionToken>,
) -> crate::voice_action::VoiceActionResult {
    let state = app.state::<AppState>();
    let config = match state.config.lock() {
        Ok(c) => c.clone(),
        Err(_) => {
            return crate::voice_action::VoiceActionResult::nothing_heard()
                .with_command(command)
                .with_thread_id(thread_id)
        }
    };
    // Resolve the scope before any content read. A recording turn borrows priority only through the
    // exact `ActiveRecording` stored in AppState; a supplied wake token is merely corroborating
    // evidence and a mismatched/stale token fails closed.
    let focus_meeting = state.focus_meeting.lock().ok().and_then(|f| f.clone());
    let current_meeting = state
        .current_meeting
        .lock()
        .ok()
        .and_then(|m| m.map(|id| id.to_string()));
    let meeting_id = resolve_scope_meeting(
        fe_meeting_id,
        focus_meeting.as_deref(),
        current_meeting.as_deref(),
    );
    // Bind every gated source read and the eventual cached answer/persistence to one lock
    // generation. The turn may block in a provider/tool loop for minutes; a relock during that
    // interval must discard the answer and must not recreate a purged interaction row.
    let visibility = crate::commands::capture_content_visibility_snapshot(state.inner());
    // After Stop removes ActiveRecording, the ordinary recorder-bound token lookup must fail
    // closed. The one exception is the bounded manual-command handoff: its AppState slot proves
    // this exact meeting + session still owns the plaintext command, and the coordinator proves
    // the supplied token is now Postprocess. No other caller can borrow this exception merely by
    // passing an id/token pair.
    let postprocess_manual_token = recording_token.as_ref().and_then(|supplied| {
        let validated = supplied.validated_for_postprocess().ok()?;
        let owned = state
            .pending_manual_command
            .lock()
            .map(|slot| {
                slot.as_ref().is_some_and(|pending| {
                    pending.meeting_id == meeting_id
                        && pending.recording_token.same_session_as(&validated)
                })
            })
            .unwrap_or(false);
        owned.then_some(validated)
    });
    let recording_token = match state.live_model_token_for_meeting(&meeting_id) {
        Ok(Some(active)) => match recording_token {
            Some(supplied) if !supplied.same_session_as(&active) => {
                return crate::voice_action::VoiceActionResult::nothing_heard()
                    .with_command(command)
                    .with_thread_id(thread_id)
            }
            _ => Some(active),
        },
        Ok(None) if recording_token.is_none() => None,
        Ok(None) | Err(_) => match postprocess_manual_token {
            Some(token) => Some(token),
            None => {
                return crate::voice_action::VoiceActionResult::nothing_heard()
                    .with_command(command)
                    .with_thread_id(thread_id)
            }
        },
    };

    // A local on-device live turn is the ONE explicitly user-initiated Brain exception during
    // recording. It still carries the exact session identity; each concrete inference segment is
    // admitted independently so DB reads, tools and persistence never hold residency. If live
    // Whisper owns the lane, degrade before resolving the reasoner or reading content — never load
    // a second Metal/GGUF model beside it.
    let live_target =
        crate::summarize::roles::resolve(crate::summarize::roles::Role::Live, &config);
    let local_model_turn = matches!(
        live_target.connection.as_str(),
        crate::summarize::roles::CONN_LOCAL | crate::summarize::roles::CONN_AFM
    );
    if local_model_turn
        && crate::perf::recording_has_priority()
        && !recording_token
            .as_ref()
            .is_some_and(crate::perf::recording_model_lane_is_free)
    {
        return crate::voice_action::VoiceActionResult::nothing_heard()
            .with_command(command)
            .with_thread_id(thread_id);
    }
    // C6: re-snapshot the LIVE unlocked set for THIS turn (never trust a snapshot taken before it).
    let unlocked = match state.unlocked_folders.lock() {
        Ok(u) => u.clone(),
        Err(_) => {
            return crate::voice_action::VoiceActionResult::nothing_heard()
                .with_command(command)
                .with_thread_id(thread_id)
        }
    };
    // note↔meeting-links PR-2 (PARTIAL) — SOURCE-SCOPED context: when the FE pins explicit sources,
    // build their GATED pinned corpus (+ capped, gated link-expansion) and APPEND it to the loop's
    // conversation as clearly-labelled additional context, so the cloud agentic cascade reasons over
    // the pinned notes/meetings. Every leg is `unlocked`-gated (a sealed source/neighbour contributes
    // NOTHING). `None`/empty ⇒ `loop_user` is byte-identical. A build error degrades to no injection
    // (never fails the turn). The tool executor's candidate constraint + the deterministic floor legs
    // are a documented follow-up (see the command doc).
    let augmented_user: String = match explicit_sources.filter(|s| !s.is_empty()) {
        Some(sources) => {
            let ask_conn = crate::summarize::roles::provider_target(
                crate::summarize::roles::Role::Ask,
                &config,
            )
            .connection;
            match crate::summarize::vault_context::build_vault_context_pinned_visible(
                &state.db, sources, &ask_conn, &unlocked,
            ) {
                Ok((pinned, _)) if !pinned.trim().is_empty() => format!(
                    "{loop_user}\n\n=== PINNED NOTES & MEETINGS (the user scoped this question to these; ground your answer in them) ===\n{pinned}"
                ),
                _ => loop_user.to_string(),
            }
        }
        None => loop_user.to_string(),
    };
    let loop_user: &str = &augmented_user;
    // Resolve an intent for the FLOOR only (its read fan-out + surfaced kind) — it NO LONGER routes
    // writes. The agentic loop (with writes) is the single executive path; the agent DECIDES.
    // The reasoner is re-resolved for THIS turn (never a startup snapshot), so a consent /
    // provider / backend change since the last turn is already in effect. LIVE role — the whole
    // in-meeting assistant (wake/voice turns AND typed @brain threads funnel through this core).
    let reasoner = match recording_token.clone() {
        Some(token) => state
            .reasoner
            .current_for_recording(crate::summarize::roles::Role::Live, token),
        None => state
            .reasoner
            .current_for(crate::summarize::roles::Role::Live),
    };
    let intent = resolve_command_intent(&*reasoner, command);
    let result = run_informational(
        app,
        state.inner(),
        &config,
        &unlocked,
        &meeting_id,
        command,
        loop_user,
        &intent,
        tool_event,
        thread_id,
        recording_token,
    )
    .with_command(command)
    .with_thread_id(thread_id);

    // PII rule: log only the coarse intent kind + status + the OPAQUE thread id, never the
    // command/summary/anchor text.
    tracing::info!(
        target: "voice",
        intent = %result.intent_kind,
        status = %result.status,
        thread_id,
        "assistant query dispatched"
    );
    let _lifecycle = crate::commands::lifecycle_guard(state.inner());
    if crate::commands::require_current_content_visibility_snapshot_under_lifecycle(
        state.inner(),
        visibility,
    )
    .is_err()
    {
        return crate::voice_action::VoiceActionResult::nothing_heard()
            .with_command(command)
            .with_thread_id(thread_id);
    }
    persist_interaction(
        state.inner(),
        &meeting_id,
        command,
        &result,
        thread_id,
        anchor_text,
    );
    result
}

/// Answer a request. On the CLOUD brain, run the model-driven agentic loop over the gated, WRITE-CAPABLE
/// tool executor (the agent DECIDES answer-vs-write); on convergence (`Ok(Some)`) map the outcome to a
/// `VoiceActionResult`. On `Ok(None)` (no convergence), any `Err` (e.g. `Unavailable` = no cloud
/// consent), or the LOCAL/stub backend, FALL THROUGH to the deterministic `handle_voice_action` floor —
/// the verified informational safety net (gated fan-out + cited synthesis + `needs_consent`).
///
/// The floor is INFORMATIONAL ONLY: it no longer owns write routing, so a `CreateReminder`/`NoteAside`
/// intent is DEMOTED to a `Research` over the literal command before flooring. Otherwise a no-consent /
/// local user saying "note that …" would trigger a deterministic write that bypassed the agent's
/// decision — exactly the hardcoded routing this change removes. So a write only ever happens when the
/// AGENT chose it; when the agent can't run, the user gets a deterministic informational answer instead.
#[allow(clippy::too_many_arguments)] // cohesive dispatch surface: gated state + loop-user + trace event.
fn run_informational(
    app: &AppHandle,
    state: &AppState,
    config: &crate::settings::AppConfig,
    unlocked: &std::collections::HashSet<String>,
    meeting_id: &str,
    command: &str,
    loop_user: &str,
    intent: &crate::audio::wake::VoiceIntent,
    tool_event: &'static str,
    thread_id: &str,
    recording_token: Option<crate::perf::RecordingSessionToken>,
) -> crate::voice_action::VoiceActionResult {
    // The agentic loop runs only on CLOUD-connection targets — local-GGUF multi-step tool-call
    // reliability is unproven (Q4 + the Bielik 32K-overflow lesson), so the local + stub targets
    // use the deterministic floor: honest, fast, and a strict no-regression vs today.
    // Resolve the reasoner for THIS request from the LIVE config — a consent/provider/backend
    // change since the branch snapshot above dispatches correctly on the next turn either way
    // (the consent gate itself re-checks fresh config on every provider call inside).
    // The eligibility gate keys on the LIVE role's resolved target: with role keys absent,
    // `!is_reasoner_only()` is EXACTLY the legacy `brain_backend == Cloud` predicate.
    let live_target = crate::summarize::roles::resolve(crate::summarize::roles::Role::Live, config);
    let reasoner = match recording_token.clone() {
        Some(token) => state
            .reasoner
            .current_for_recording(crate::summarize::roles::Role::Live, token),
        None => state
            .reasoner
            .current_for(crate::summarize::roles::Role::Live),
    };
    let live_is_reasoner_only = live_target.is_reasoner_only();
    // Brain v2 L3 — SHADOW ROUTER (observation only, NOT dispatch): log the route the explicit
    // `crate::router` decision table WOULD take next to what the legacy gate below actually
    // chooses, so router↔legacy parity can be validated on real usage BEFORE any cutover.
    // CONTENT-FREE: coarse class + decision labels only — never the command text.
    {
        let decision = crate::router::route(&crate::router::RouterInput {
            role: crate::summarize::roles::Role::Live,
            config,
            query_class: crate::router::classify_query(command),
            heavy_available: crate::router::class_model_available(
                config,
                crate::reason::ModelClass::Heavy,
            ),
            light_available: crate::router::class_model_available(
                config,
                crate::reason::ModelClass::Light,
            ),
        });
        // The legacy path below has exactly two outcomes: the cloud cascade or the deterministic
        // floor. Divergence on `local` targets is EXPECTED (the router plans local tiers the
        // legacy path floors) — that gap is what this log measures.
        let legacy = if live_is_reasoner_only {
            "floor"
        } else {
            "cloud_agentic"
        };
        tracing::debug!(
            target: "router",
            shadow = decision.label(),
            legacy,
            agree = decision.label() == legacy,
            "shadow route (legacy path decides)"
        );
    }
    if !live_is_reasoner_only {
        // Phase 5 — the CURRENT-FIRST BRAIN CASCADE. Try each tier in order (current meeting →
        // vault → connectors), each with a STRUCTURALLY-scoped executor (`AssistantScope`) that
        // advertises only that tier's tools. The tier's prompt instructs the model to reply with the
        // `__ESCALATE__` sentinel when the question is not answerable at that tier; the ladder detects
        // it and steps up. This is "deterministic escalation, model-driven retrieval": which tools are
        // reachable is code-enforced, retrieval within a tier stays model-driven.
        if let Some(result) = run_cascade(
            app,
            state,
            config,
            unlocked,
            meeting_id,
            command,
            loop_user,
            intent,
            tool_event,
            thread_id,
            &*reasoner,
            recording_token.clone(),
        ) {
            return result;
        }
        // No tier converged (out of steps / no answer at any tier) → fall through to the floor.
    }
    // FLOOR — deterministic, gated, cited, needs_consent-aware, INFORMATIONAL ONLY. `floor_intent_for`
    // demotes a write intent to a Research so the floor NEVER performs a hardcoded write (a write
    // happens only when the AGENT chooses it; the floor is the read safety net).
    //
    // RECORDING AWARENESS on the FLOOR (back-ported from the cascade): the SAME recorder-lock flag the
    // cascade computes (`run_cascade`) — a bool, NOT a content read — so the floor knows a meeting is
    // being recorded RIGHT NOW even when the live buffer is still empty (meeting just started).
    let recording_in_progress = state.recorder.lock().map(|g| g.is_some()).unwrap_or(false);

    // ── TIER 1 — DETERMINISTIC CURRENT-FIRST (the structural fix) ────────────────────────────────
    // When the user is CLEARLY asking about THIS meeting ("o czym jest to spotkanie", "summarize this
    // recording", "co tu ustaliliśmy"), answer from the CURRENT meeting IN ISOLATION — NO vault
    // fan-out, NO web leg, NO calendar. This is what stops "o czym to spotkanie" (topic ≈ "spotkanie")
    // from web-searching the WORD "meeting" and drowning the weak local model in generic definition
    // pages. Deterministic, NOT model-driven: the weak floor model can't be trusted to route.
    //
    // The current content comes from `tier1_current_content` — the SAME visibility gate as the
    // fan-out floor, but ALSO reading a VIEWED PAST meeting's own gated transcript+note (the gap:
    // `floor_current_context` reads only the live buffer, so a viewed saved meeting was empty →
    // wrongly fell through to fan-out). A sealed-not-unlocked meeting yields "" (fail-closed) → the
    // honest "no content" branch, NEVER a leak and NEVER a fan-out.
    if crate::voice_action::is_about_current_meeting(command) {
        let current_content =
            tier1_current_content(&state.db, &state.live_transcript, meeting_id, unlocked);
        let title = current_meeting_title(&state.db, meeting_id, unlocked);
        // `intent_kind` for the card: keep the demoted read discriminant (research/recall stay as-is;
        // a write intent is surfaced as research so the card style matches the informational floor).
        let floor_intent = floor_intent_for(intent, command);
        let kind = crate::voice_action::intent_kind_str(&floor_intent);
        return crate::voice_action::answer_current_meeting_isolated(
            kind,
            command,
            &current_content,
            recording_in_progress,
            title.as_deref(),
            &*reasoner,
        );
    }

    // ── TIER 2/3 — the EXISTING fan-out floor (UNCHANGED) ────────────────────────────────────────
    // NOT a current-meeting question → the vault + web + calendar fan-out, exactly as before, so
    // cross-note ("co ustaliliśmy z Weroniką" → vault) and world ("jaka pogoda" → web) questions still
    // work. Because the current-first branch returned early above, the web leg now ONLY ever fires for
    // genuinely non-current questions — precisely the fix.
    //
    // Fetch THIS meeting's context through the SAME GATED reader the cascade uses
    // (`gated_live_context` → fail-closed on `meeting_is_visible`: a sealed-not-visible meeting yields
    // empty), and hand it to the floor as the PRIMARY, clearly-labeled grounding. No new read path, no
    // new egress class (it rides the same RedactingProvider + consent gate inside `handle_voice_action`).
    let (live, typed_notes) =
        gated_live_context(&state.db, &state.live_transcript, meeting_id, unlocked);
    // Brain v2 L4: when running bullets exist (gated IDENTICALLY to the live buffer), the floor's
    // current-meeting block becomes the tighter bullets+verbatim composition; with bullets empty
    // this is byte-identical to before (compose is the identity then).
    let bullets = gated_live_bullets(&state.db, &state.live_bullets, meeting_id, unlocked);
    let live = compose_live_inject(&live, &bullets);
    let current_meeting_context = floor_current_context(&live, &typed_notes);
    let floor_intent = floor_intent_for(intent, command);
    crate::voice_action::handle_voice_action_with_recording_token(
        &floor_intent,
        &*reasoner,
        &state.db,
        unlocked,
        config,
        meeting_id,
        command,
        &current_meeting_context,
        recording_in_progress,
        Some(app),
        recording_token.as_ref(),
    )
}

/// Assemble the CURRENT meeting's gated context (its live-transcript tail + the user's typed notes)
/// into one compact, labeled block for the deterministic floor's PRIMARY grounding. Inputs come from
/// [`gated_live_context`] (already visibility-gated — a sealed-not-visible meeting yields two empty
/// strings), so this only ever formats VISIBLE content. Both parts are truncated to the same recent
/// tail the live system prompt uses ([`LIVE_TRANSCRIPT_INJECT_CHARS`]) to keep the local reasoner's
/// window bounded. Returns "" when there is nothing to inject (idle / not-yet-captured / not visible),
/// so the floor stays byte-identical to before for a no-context turn.
fn floor_current_context(live: &str, typed_notes: &str) -> String {
    let mut out = String::new();
    let live = live.trim();
    if !live.is_empty() {
        out.push_str("Live transcript (so far):\n");
        out.push_str(&tail_chars(live, LIVE_TRANSCRIPT_INJECT_CHARS));
    }
    let typed = typed_notes.trim();
    if !typed.is_empty() {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str("Your typed notes:\n");
        out.push_str(&tail_chars(typed, LIVE_TRANSCRIPT_INJECT_CHARS));
    }
    out
}

/// TIER-1 CURRENT-MEETING CONTENT for the DETERMINISTIC FLOOR (reasoner-only / local backend). Unlike
/// [`floor_current_context`] (which reads ONLY the live RAM buffer), this ALSO reads a VIEWED PAST
/// meeting's own gated content — the gap that made "o czym to spotkanie" on a viewed saved meeting
/// fall through to the vault+web fan-out (its live buffer is empty for a non-recording meeting).
///
/// Two GATED sources, in order:
///   1. LIVE (recording in progress OR the live buffer/typed-notes are non-empty for a VISIBLE
///      meeting): the `floor_current_context` block (live tail + typed notes), already
///      visibility-gated by `gated_live_context` (a sealed-not-visible meeting yields "").
///   2. PAST (a viewed saved meeting): the meeting's OWN transcript (`get_segments`) + note
///      (`get_note_if_visible`), read ONLY when `meeting_is_visible` — a sealed-not-unlocked meeting
///      is INVISIBLE, so BOTH the visibility check and `get_note_if_visible` fail closed and this
///      returns "". This mirrors the `chat_meeting` gated read shape but stays inside `live.rs` over
///      `db` (no call into `commands.rs`).
///
/// Returns the assembled current-meeting content (possibly ""). NEVER reads ungated content: the
/// visibility gate is checked before ANY segment/note read.
fn tier1_current_content(
    db: &crate::storage::Db,
    live_transcript: &std::sync::Mutex<String>,
    meeting_id: &str,
    unlocked: &std::collections::HashSet<String>,
) -> String {
    // 1) LIVE buffer + typed notes (gated). Non-empty for a visible in-progress / focused meeting.
    let (live, typed_notes) = gated_live_context(db, live_transcript, meeting_id, unlocked);
    let live_block = floor_current_context(&live, &typed_notes);
    if !live_block.trim().is_empty() {
        return live_block;
    }

    // 2) PAST viewed meeting — read its OWN gated content. GATE FIRST: a sealed-not-unlocked meeting
    // is invisible → return "" (no read). This is the same fail-closed gate `chat_meeting` uses,
    // implemented here over `db` so we never touch `commands.rs`.
    if meeting_id.is_empty() || !db.meeting_is_visible(meeting_id, unlocked).unwrap_or(false) {
        return String::new();
    }

    let mut out = String::new();
    // Transcript (gated by the visibility check above; a sealed meeting's segments are blanked at
    // rest anyway). Rendered like `chat_meeting`'s transcript, bounded to the recent tail.
    if let Ok(segments) = db.get_segments(meeting_id) {
        let transcript = segments
            .iter()
            .filter(|s| !s.text.trim().is_empty())
            .map(|s| format!("[{:.0}s] {}", s.start_s, s.text.trim()))
            .collect::<Vec<_>>()
            .join("\n");
        let transcript = transcript.trim();
        if !transcript.is_empty() {
            out.push_str("Transcript:\n");
            out.push_str(&tail_chars(transcript, LIVE_TRANSCRIPT_INJECT_CHARS));
        }
    }
    // Note (VISIBILITY-GATED read: a sealed-not-unlocked meeting yields None).
    if let Ok(Some(note)) = db.get_note_if_visible(meeting_id, unlocked) {
        let md = note.markdown.trim();
        if !md.is_empty() {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str("Note:\n");
            out.push_str(&tail_chars(md, LIVE_TRANSCRIPT_INJECT_CHARS));
        }
    }
    out
}

/// The current meeting's OWN gated title (for the Tier-1 isolated answer's `[[Title]]` citation).
/// GATE (fail-closed): resolved ONLY for a VISIBLE meeting — a sealed-not-unlocked meeting is
/// invisible and yields `None` (no title leak). Mirrors [`tier_extra_citations`]'s gated resolution.
fn current_meeting_title(
    db: &crate::storage::Db,
    meeting_id: &str,
    unlocked: &std::collections::HashSet<String>,
) -> Option<String> {
    if meeting_id.is_empty() || !db.meeting_is_visible(meeting_id, unlocked).unwrap_or(false) {
        return None;
    }
    match db.get_meeting(meeting_id) {
        Ok(Some(m)) => m
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string),
        _ => None,
    }
}

/// Per-tier step budgets (Phase 5) — kept SMALL so the cascade stays live-safe (a live turn can run
/// up to all three tiers back-to-back). Tier 1 answers from injected content (no retrieval tool), so
/// it converges in one model turn — a budget of 2 leaves room for a `propose_note` + answer. Tier 2
/// (vault retrieval) gets 4; Tier 3 (connectors) gets 3.
const TIER1_MAX_STEPS: usize = 2;
const TIER2_MAX_STEPS: usize = 4;
const TIER3_MAX_STEPS: usize = 3;

/// Run the CURRENT-FIRST BRAIN CASCADE (Phase 5): Tier 1 (current meeting in isolation) → Tier 2
/// (vault) → Tier 3 (connectors), each with a STRUCTURALLY-scoped executor. Returns
/// `Some(VoiceActionResult)` when a tier CONVERGED to a real (non-escalation) answer — the tier badge
/// is set DETERMINISTICALLY to that tier. Returns `None` when no tier answered (each either escalated
/// to the next, or ran out of steps / errored) → the caller floors to the deterministic path.
///
/// ESCALATION is deterministic: the tier prompt asks the model to reply EXACTLY the
/// [`crate::agent::ESCALATE_SENTINEL`] when it cannot answer at that tier; [`crate::agent::is_escalation`]
/// detects it (whole-answer match, never a substring) and the ladder steps up. A NON-convergence
/// (`Ok(None)`) or an `Err` at a tier is handled WITHIN the ladder (try the next tier if there is one,
/// else `None`) — it is NOT conflated with escalation.
#[allow(clippy::too_many_arguments)]
fn run_cascade(
    app: &AppHandle,
    state: &AppState,
    config: &crate::settings::AppConfig,
    unlocked: &std::collections::HashSet<String>,
    meeting_id: &str,
    command: &str,
    loop_user: &str,
    intent: &crate::audio::wake::VoiceIntent,
    tool_event: &'static str,
    thread_id: &str,
    reasoner: &dyn crate::reason::LocalReasoner,
    recording_token: Option<crate::perf::RecordingSessionToken>,
) -> Option<crate::voice_action::VoiceActionResult> {
    // Shared per-turn injected context (gated). Tier 1 leans on the live buffer + typed notes for the
    // CURRENT meeting (read through `gated_live_context`, fail-closed on the LIVE unlocked set); the
    // memory brief + recording flag ride along exactly as before. NO new egress class — every segment
    // goes through the same RedactingProvider + cloud-consent gate. L2.2: the brief is relevance-
    // filtered against the user's CURRENT command (`command`, never the whole rendered conversation).
    let (live, typed_notes) =
        gated_live_context(&state.db, &state.live_transcript, meeting_id, unlocked);
    // Brain v2 L4: the live-question inject becomes `2k bullets + 2k verbatim tail` when running
    // bullets exist — gated IDENTICALLY to the live buffer (`gated_live_bullets` fail-closes on
    // the same `meeting_is_visible` check), byte-identical to the legacy 6k tail when they don't.
    let bullets = gated_live_bullets(&state.db, &state.live_bullets, meeting_id, unlocked);
    let live = compose_live_inject(&live, &bullets);
    let memory_brief =
        gated_user_memory_brief(&state.db, unlocked, config.user_memory_enabled, command);
    let recording_in_progress = state.recorder.lock().map(|g| g.is_some()).unwrap_or(false);
    let base_system =
        assistant_system_prompt(&live, &typed_notes, &memory_brief, recording_in_progress);

    // The ladder, in order. Each entry: (scope, per-tier prompt suffix, step budget, badge, whether
    // it may escalate). Tier 3 is TERMINAL — it never escalates (there is no higher tier).
    use crate::tools::AssistantScope;
    use crate::voice_action::AnsweredFrom;
    let tiers: [(AssistantScope, &str, usize, AnsweredFrom, bool); 3] = [
        (
            AssistantScope::CurrentMeeting,
            TIER1_SUFFIX,
            TIER1_MAX_STEPS,
            AnsweredFrom::CurrentMeeting,
            true,
        ),
        (
            AssistantScope::Vault,
            TIER2_SUFFIX,
            TIER2_MAX_STEPS,
            AnsweredFrom::Vault,
            true,
        ),
        (
            AssistantScope::Connectors,
            TIER3_SUFFIX,
            TIER3_MAX_STEPS,
            AnsweredFrom::Connectors,
            false,
        ),
    ];

    // L3 escalation ledger — the connection the NEXT tier will run on. The whole cascade is gated
    // by the caller to run ONLY on a non-reasoner-only (provider) Live target and every tier shares
    // that ONE resolved reasoner, so today the next tier is always provider-served; the
    // `is_reasoner_only` guard below keeps a future local-tier cascade from writing bogus rows
    // (a local→local escalation sends nothing off-device and is NOT egress).
    let live_target = crate::summarize::roles::resolve(crate::summarize::roles::Role::Live, config);

    for (tier_idx, (scope, suffix, max_steps, badge, may_escalate)) in tiers.into_iter().enumerate()
    {
        let executor = crate::tools::GatedToolExecutor {
            db: &state.db,
            // The LIVE set behind its Mutex — re-read per tool call (C6), so a mid-loop relock is gated
            // out immediately at every tier.
            unlocked: &state.unlocked_folders,
            config,
            meeting_id,
            app: Some(app),
            recording_token: recording_token.clone(),
            // PROPOSE-then-ACCEPT model (Rev 2): the in-meeting agent is READ-ONLY at every tier — it
            // GENERATES content (incl. note drafts) but NEVER auto-writes; the user commits via "Add to
            // notes". The dispatch still routes EVERY request through the loop (no hardcoded classifier).
            allow_writes: false,
            note_drafts: true,
            // Phase 5: the STRUCTURAL escalation boundary — this tier's executor advertises only its
            // tools; `run()` refuses any other. A weak model CANNOT reach a higher tier's tools.
            scope,
            // Seal-on-write handles (residual W1): this surface is read-only (`allow_writes:
            // false`), but the executor carries the live seam so a future write-enabled turn can
            // never silently skip the manual-notes seal.
            seal: Some(crate::tools::SealAccess {
                master_kek: &state.master_kek,
                lifecycle: &state.lifecycle,
            }),
            proposed_note: std::sync::Mutex::new(None),
        };
        let sink = ToolEventSink {
            app: app.clone(),
            event: tool_event,
            thread_id: thread_id.to_string(),
        };
        let system = format!("{base_system}\n\n{suffix}");
        match crate::agent::run_agentic_loop(
            reasoner,
            &system,
            loop_user,
            &executor,
            max_steps,
            Some(&sink as &dyn crate::agent::DeltaSink),
            // P0.3: the LIVE preset — a 1024-token cap (+ the GGUF 30 s wall-clock timeout) so a
            // runaway on-device decode can't saturate Metal mid-recording. No-op on stub/cloud.
            // L3: rides the `loop_transcript_compaction` flag (default ON) + the default-off
            // `brain_heavy_grammar_enabled` tiny-schema constraint (a no-op on today's cloud-only
            // cascade reasoners; correct if a local tier ever runs here).
            crate::reason::GenOptions::live_answer()
                .with_transcript_compaction(config.loop_transcript_compaction)
                .with_grammar_constraint(config.brain_heavy_grammar_enabled),
        ) {
            Ok(Some(outcome)) => {
                // ESCALATION vs a REAL answer. `is_escalation` matches the WHOLE answer (never a
                // substring) so a genuine answer that mentions the token does not mis-escalate.
                if crate::agent::is_escalation(&outcome.answer) {
                    if may_escalate {
                        tracing::debug!(target: "voice", "tier escalating to the next tier");
                        // Brain v2 L3 — ESCALATION IS A LEDGERED EVENT: a content-free
                        // `egress_log` row (call_kind "escalation", tierN→tierN+1, NO query text)
                        // makes the step-up visible in the privacy receipt. The record/skip
                        // DECISION is the pure, test-bound `should_ledger_escalation` (the
                        // reasoner-only guard lives there); only the `active_sink().record(...)`
                        // side-effect below stays code-read-verified (it needs the running app's
                        // startup-wired sink — headless tests get the NoopEgressSink).
                        if let Some(entry) = should_ledger_escalation(
                            live_target.is_reasoner_only(),
                            &live_target.connection,
                            tier_idx as u8 + 1,
                            tier_idx as u8 + 2,
                        ) {
                            crate::summarize::egress_log::active_sink().record(entry);
                        }
                        continue; // try the next tier
                    }
                    // Terminal tier said escalate but there is no higher tier → treat as "no answer
                    // here" and fall to the deterministic floor (never surface the sentinel).
                    tracing::debug!(target: "voice", "terminal tier requested escalation; flooring");
                    return None;
                }
                // REAL answer at this tier — set the badge DETERMINISTICALLY from the tier that
                // converged (never string-sniffed). Add the tier-appropriate extra citations.
                let extra = tier_extra_citations(&state.db, meeting_id, badge, unlocked);
                let proposed = executor.proposed_note.lock().ok().and_then(|g| g.clone());
                return Some(
                    crate::voice_action::VoiceActionResult::from_agent(
                        intent, outcome, badge, extra,
                    )
                    .with_proposed_note(proposed),
                );
            }
            // Non-convergence at a tier is NOT escalation — try the next tier if there is one, else
            // let the caller floor. This distinguishes "out of steps here" from "escalate".
            Ok(None) => {
                tracing::debug!(target: "voice", "tier did not converge; trying the next tier / floor");
                continue;
            }
            // An error (e.g. Unavailable = no cloud consent) is fatal to the whole cascade — the same
            // provider is used at every tier, so a re-attempt would fail identically. Floor.
            Err(e) => {
                tracing::debug!(target: "voice", error = %e, "cascade tier unavailable/failed; flooring");
                return None;
            }
        }
    }
    None
}

/// Brain v2 L3 — the escalation-ledger DECISION, factored PURE so the guard is test-bound
/// (adversarial finding 2026-07-10: removing the reasoner-only guard survived the full suite —
/// nothing bound it). Returns the content-free `escalation_entry` to record when the Live target
/// is an egress-bearing (provider) connection; `None` when the target is reasoner-only
/// (local/AFM/off) — a local→local escalation sends NOTHING off-device and must never write an
/// egress row. `run_cascade` records whatever this returns via `active_sink()` (the AppHandle-era
/// side-effect that stays code-read-verified; THIS function owns the decision).
fn should_ledger_escalation(
    live_target_reasoner_only: bool,
    connection: &str,
    from_tier: u8,
    to_tier: u8,
) -> Option<crate::summarize::egress_log::EgressEntry> {
    if live_target_reasoner_only {
        return None;
    }
    Some(crate::summarize::egress_log::escalation_entry(
        connection, from_tier, to_tier,
    ))
}

/// The tier-specific extra citations the loop's `gathered`-scraped citations miss. Tier 1's answer is
/// grounded in PROMPT-INJECTED current-meeting content (which produces no `[[Title]]` in `gathered`),
/// so we resolve the CURRENT meeting's OWN title to a `[[Title]]` and prepend it — GATED: only when
/// the meeting is VISIBLE to the live `unlocked` set (never a title for a sealed-not-unlocked
/// meeting), and only when it has a non-empty title. Tier 2/3 rely on the loop's own gated citations
/// + (Tier 3) the connector loud-lines the agent's answer already carries, so they add nothing here.
fn tier_extra_citations(
    db: &crate::storage::Db,
    meeting_id: &str,
    badge: crate::voice_action::AnsweredFrom,
    unlocked: &std::collections::HashSet<String>,
) -> Vec<String> {
    if badge != crate::voice_action::AnsweredFrom::CurrentMeeting || meeting_id.is_empty() {
        return Vec::new();
    }
    // GATE (fail-closed): resolve the title ONLY for a VISIBLE meeting — a sealed-not-unlocked meeting
    // is invisible and contributes no citation (its live buffer was already masked to "" by
    // `gated_live_context`, so Tier 1 could only have answered "just started" anyway).
    if !db.meeting_is_visible(meeting_id, unlocked).unwrap_or(false) {
        return Vec::new();
    }
    match db.get_meeting(meeting_id) {
        Ok(Some(m)) => match m.title.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            Some(title) => vec![format!("[[{title}]]")],
            None => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// The intent the INFORMATIONAL FLOOR runs with. Read intents (Research / Recall / SlackSearch /
/// Unknown) pass through UNCHANGED — zero read-answer regression. A WRITE intent
/// (`CreateReminder` / `NoteAside`) is DEMOTED to a `Research` over the literal command, so a
/// no-consent / local user whose phrase happened to parse as a write gets a deterministic
/// informational answer instead of a hardcoded write the AGENT never chose. This is the load-bearing
/// "the floor no longer owns write routing" rule, made pure + headless-testable.
fn floor_intent_for(
    intent: &crate::audio::wake::VoiceIntent,
    command: &str,
) -> crate::audio::wake::VoiceIntent {
    match intent {
        crate::audio::wake::VoiceIntent::CreateReminder { .. }
        | crate::audio::wake::VoiceIntent::NoteAside { .. } => {
            crate::audio::wake::VoiceIntent::Research {
                topic: command.trim().to_string(),
            }
        }
        other => other.clone(),
    }
}

/// The live tool-trace sink: emits a per-tool-call event (`EVENT_ASSISTANT_TOOL` for the card,
/// `EVENT_CHAT_TOOL` for the chat panel, `EVENT_ASK_TOOL` for the Ask page — chosen by `event`) so
/// the FE can render the "Searching notes… ✓" chips. NO PII — tool NAME + a coarse result-size
/// count + the turn's OPAQUE thread id. `pub(crate)` so `ask_vault` (commands.rs) reuses the SAME
/// payload contract instead of duplicating it.
pub(crate) struct ToolEventSink {
    pub(crate) app: AppHandle,
    pub(crate) event: &'static str,
    /// The turn's thread identity — stamped on every chip so simultaneous threads never
    /// cross-attribute their traces (the documented v1 gap this closes).
    pub(crate) thread_id: String,
}
impl crate::agent::DeltaSink for ToolEventSink {
    fn tool_running(&self, tool: &str) {
        let _ = self.app.emit(
            self.event,
            tool_trace_payload(&self.thread_id, tool, "running", true, None),
        );
    }
    fn tool_done(&self, tool: &str, ok: bool, result_chars: usize) {
        let _ = self.app.emit(
            self.event,
            tool_trace_payload(&self.thread_id, tool, "done", ok, Some(result_chars as u32)),
        );
    }
}

/// Build one tool-trace payload — pure (no AppHandle), so the payload contract is headless-testable:
/// tool NAME + state + coarse count + the turn's opaque `thread_id`. NO PII (never the tool args,
/// results, or any content).
fn tool_trace_payload(
    thread_id: &str,
    tool: &str,
    state: &str,
    ok: bool,
    count: Option<u32>,
) -> crate::events::AssistantToolPayload {
    crate::events::AssistantToolPayload {
        tool: tool.to_string(),
        state: state.to_string(),
        ok,
        count,
        thread_id: Some(thread_id.to_string()),
    }
}

/// Persist one dispatched voice interaction against the CURRENT recording meeting — best-effort +
/// PANIC-FREE so a persist failure can NEVER disrupt recording/dispatch. Skips when there is no
/// active recording (`meeting_id` empty). `command` is the user's own dictated words (the heard
/// command for the manual path, the intent's literal words for the wake path). The `summary`/
/// `citations`/`status`/`intent_kind` come straight off the dispatch result. Derived convenience
/// data: it is PURGED on seal (it mirrors sealed content), never surfaced for a sealed meeting.
fn persist_interaction(
    state: &AppState,
    meeting_id: &str,
    command: &str,
    result: &crate::voice_action::VoiceActionResult,
    thread_id: &str,
    anchor_text: Option<&str>,
) {
    if meeting_id.is_empty() {
        return; // no active recording → nothing to attach the interaction to.
    }
    let created_at = chrono::Utc::now().to_rfc3339();
    match state.db.insert_assistant_interaction(
        meeting_id,
        command,
        &result.summary,
        &result.citations,
        &result.status,
        Some(result.intent_kind.as_str()),
        Some(thread_id),
        anchor_text,
        &created_at,
    ) {
        Ok(_) => {}
        // PII rule: log only that a persist failed + the coarse status — never the command/answer.
        Err(e) => tracing::debug!(
            target: "voice",
            error = %e,
            status = %result.status,
            "persisting assistant interaction failed; continuing"
        ),
    }
}

/// What the live loop should do with the MANUAL voice-command capture on this tick.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ManualCaptureDecision {
    /// The capture is DONE (the user clicked stop, OR the backstop cap was reached) AND the
    /// accumulated post-click utterance is a real (non-empty, meaningful) command → resolve +
    /// dispatch it (keyword fast-path, else brain interpret, else keyword fallback) and clear the
    /// capture. Carries the FULL accumulated utterance, not a per-tick fragment.
    Dispatch { generation: u64, command: String },
    /// Still listening (the user has not stopped and the backstop cap is not yet reached) → keep the
    /// capture armed, ACCUMULATING the growing post-click window, and wait for the next tick / the
    /// user's stop click. Does NOT dispatch on hearing speech (CLICK-TO-STOP).
    KeepListening,
    /// The capture has an exact terminal range, but the durable spool has not certified every frame
    /// yet. The generation stays armed and the live thread retries without consulting AppState's
    /// recorder slot, which Stop may already have removed.
    AwaitingDurable { generation: u64 },
    /// The capture ended (user stop OR backstop) with NOTHING meaningful heard (the user never spoke,
    /// or only silence/filler) → clear the capture and surface a graceful "nothing_heard". NEVER
    /// dispatches an empty command.
    NothingHeard,
    /// The exact range could not become durable because the spool terminated before certifying it.
    /// Surface an honest unavailable result; never misreport this as silence.
    Unavailable,
}

fn apply_manual_capture_decision(
    app: &AppHandle,
    meeting_id: &str,
    model_token: &crate::perf::RecordingSessionToken,
    decision: ManualCaptureDecision,
) {
    match decision {
        ManualCaptureDecision::Dispatch {
            generation,
            command,
        } => {
            let _ = app.emit(
                crate::events::EVENT_VOICE_COMMAND_LISTENING,
                crate::events::VoiceCommandListeningPayload { active: false },
            );
            let pending = crate::state::PendingManualCommand {
                meeting_id: meeting_id.to_string(),
                capture_generation: generation,
                command,
                recording_token: model_token.clone(),
            };
            let state = app.state::<AppState>();
            // Atomically move ownership from the capture slot to the pending slot. Begin uses the
            // same capture -> pending order (under recorder), so it can observe neither a gap nor
            // overwrite the just-finalized generation between transcription and publication.
            let published = match state.voice_command_capture.lock() {
                Ok(mut capture) => match state.pending_manual_command.lock() {
                    Ok(mut slot)
                        if capture
                            .as_ref()
                            .is_some_and(|current| current.generation == generation)
                            && slot.is_none() =>
                    {
                        *slot = Some(pending.clone());
                        *capture = None;
                        true
                    }
                    _ => false,
                },
                Err(_) => false,
            };
            if !published {
                tracing::warn!(target: "voice", "manual command handoff slot was occupied; preserving the existing owned command");
                if let Ok(mut capture) = state.voice_command_capture.lock() {
                    if capture
                        .as_ref()
                        .is_some_and(|current| current.generation == generation)
                    {
                        *capture = None;
                    }
                }
                let _ = app.emit(
                    crate::events::EVENT_VOICE_ACTION_RESULT,
                    crate::voice_action::VoiceActionResult::capture_unavailable(),
                );
                return;
            }
            // Local Live work intentionally waits for Stop because resident Whisper owns the model
            // lane. Cloud-capable work may be accepted now. In either case the slot remains owned
            // until the accepted worker completes, so Draining can never silently drop it.
            if let Err(error) = try_spawn_pending_manual_turn(app, pending, model_token.clone()) {
                tracing::debug!(target: "voice", error = %error, "manual command retained for postprocess dispatch");
            }
        }
        ManualCaptureDecision::NothingHeard => {
            let _ = app.emit(
                crate::events::EVENT_VOICE_COMMAND_LISTENING,
                crate::events::VoiceCommandListeningPayload { active: false },
            );
            let _ = app.emit(
                crate::events::EVENT_VOICE_ACTION_RESULT,
                crate::voice_action::VoiceActionResult::nothing_heard(),
            );
        }
        ManualCaptureDecision::Unavailable => {
            let _ = app.emit(
                crate::events::EVENT_VOICE_COMMAND_LISTENING,
                crate::events::VoiceCommandListeningPayload { active: false },
            );
            let _ = app.emit(
                crate::events::EVENT_VOICE_ACTION_RESULT,
                crate::voice_action::VoiceActionResult::capture_unavailable(),
            );
        }
        ManualCaptureDecision::KeepListening | ManualCaptureDecision::AwaitingDurable { .. } => {}
    }
}

/// Admit one finalized manual command as an owned assistant worker. The coordinator lease is
/// acquired synchronously before the thread exists, atomically with Live/Draining/Postprocess
/// transitions. Therefore either the worker is included in Stop's quiescence wait, or admission
/// fails and the bounded pending slot remains for Stop to dispatch in Postprocess.
fn try_spawn_pending_manual_turn(
    app: &AppHandle,
    pending: crate::state::PendingManualCommand,
    token: crate::perf::RecordingSessionToken,
) -> crate::error::Result<Option<tokio::sync::oneshot::Receiver<()>>> {
    if !pending.recording_token.same_session_as(&token) {
        return Err(crate::error::AppError::Unavailable(
            "manual command belongs to a stale recording session".into(),
        ));
    }

    let local_live = app
        .state::<AppState>()
        .config
        .lock()
        .map(|config| {
            matches!(
                crate::summarize::roles::resolve(crate::summarize::roles::Role::Live, &config,)
                    .connection
                    .as_str(),
                crate::summarize::roles::CONN_LOCAL | crate::summarize::roles::CONN_AFM
            )
        })
        .unwrap_or(true);
    if local_live && !crate::perf::recording_model_lane_is_free(&token) {
        return Ok(None);
    }

    // This lifecycle lease deliberately covers prompt preparation as well as the eventual local
    // model / connector-specific lease. Draining must see ownership before it can race this worker.
    let lifecycle_lease = crate::perf::acquire_recording_work_lease(&token)?;
    let key = pending.meeting_id.clone();
    if !try_begin_turn(&app.state::<AppState>().in_flight_turns, &key) {
        return Ok(None);
    }
    let guard = TurnGuard {
        app: app.clone(),
        key,
    };
    let worker_app = app.clone();
    let worker_pending = pending.clone();
    let (done_tx, done_rx) = tokio::sync::oneshot::channel();
    let (start_tx, start_rx) = std::sync::mpsc::sync_channel(0);
    let spawned = std::thread::Builder::new()
        .name("murmur-manual-assistant-turn".into())
        .spawn(move || {
            let _lifecycle_lease = lifecycle_lease;
            let _guard = guard;
            if start_rx.recv().is_err() {
                let _ = done_tx.send(());
                return;
            }
            _guard
                .app
                .state::<AppState>()
                .user_turn_in_progress
                .store(true, std::sync::atomic::Ordering::Relaxed);
            run_assistant_turn(
                &worker_app,
                worker_pending.command.clone(),
                None,
                Some(worker_pending.meeting_id.clone()),
                Some(token),
            );
            if let Ok(mut slot) = worker_app.state::<AppState>().pending_manual_command.lock() {
                if slot.as_ref().is_some_and(|current| {
                    current.capture_generation == worker_pending.capture_generation
                        && current.meeting_id == worker_pending.meeting_id
                        && current
                            .recording_token
                            .same_session_as(&worker_pending.recording_token)
                }) {
                    *slot = None;
                }
            }
            let _ = done_tx.send(());
        });
    if spawned.is_err() {
        // The failed spawn drops the closure, which drops both guards and the completion sender.
        // The command itself remains in AppState for Postprocess ownership.
        return Ok(None);
    }
    let _ = app.emit(
        crate::events::EVENT_VOICE_COMMAND_PROCESSING,
        crate::events::VoiceCommandProcessingPayload { active: true },
    );
    let _ = start_tx.send(());
    Ok(Some(done_rx))
}

/// Stop-side half of the manual-command ownership protocol. Call only after Live quiescence and
/// `transition_to_postprocess`: it validates both opaque meeting identity and recording-session
/// identity, starts the worker under the exact Postprocess token, and awaits that owned worker so
/// the batch pipeline cannot steal the single model lane first.
pub(crate) async fn dispatch_pending_manual_after_stop(
    app: &AppHandle,
    meeting_id: &str,
    postprocess_token: crate::perf::RecordingSessionToken,
) {
    let pending = app
        .state::<AppState>()
        .pending_manual_command
        .lock()
        .ok()
        .and_then(|slot| slot.clone());
    let Some(pending) = pending else {
        // Live is already proven quiescent at this call site. Any capture still present therefore
        // lost its only consumer before it could publish a pending command (model-load failure,
        // panic, or an early live-loop exit). Resolve it honestly before Stop disarms its RAII
        // cleanup; otherwise the stale single-entry capture would block every future Begin.
        if app
            .state::<AppState>()
            .voice_command_capture
            .lock()
            .map(|capture| capture.is_some())
            .unwrap_or(true)
        {
            fail_pending_manual_for_meeting(app, meeting_id);
        }
        return;
    };
    let identity_matches = pending_matches_stop(&pending, meeting_id, &postprocess_token);
    if !identity_matches {
        tracing::warn!(target: "voice", "stale manual command handoff rejected at Stop");
        clear_pending_manual_command(app, &pending);
        let _ = app.emit(
            crate::events::EVENT_VOICE_ACTION_RESULT,
            crate::voice_action::VoiceActionResult::capture_unavailable(),
        );
        return;
    }
    match try_spawn_pending_manual_turn(app, pending.clone(), postprocess_token) {
        Ok(Some(done)) => {
            if done.await.is_err() || pending_is_owned(app, &pending) {
                clear_pending_manual_command(app, &pending);
                let _ = app.emit(
                    crate::events::EVENT_VOICE_ACTION_RESULT,
                    crate::voice_action::VoiceActionResult::capture_unavailable(),
                );
            }
        }
        Ok(None) | Err(_) => {
            clear_pending_manual_command(app, &pending);
            let _ = app.emit(
                crate::events::EVENT_VOICE_ACTION_RESULT,
                crate::voice_action::VoiceActionResult::capture_unavailable(),
            );
        }
    }
}

/// Resolve an owned command honestly when Stop itself cannot reach Postprocess. This is called by
/// a Stop RAII guard on every error/panic path, preventing an old session token from occupying the
/// bounded slot forever and blocking the next recording's Begin.
pub(crate) fn fail_pending_manual_for_meeting(app: &AppHandle, meeting_id: &str) {
    // Clear an ended-but-not-yet-published capture first. The live step compare-before-publishes,
    // so a concurrent transcription that finishes after this failure observes the missing
    // generation and cannot resurrect a stale command into the slot.
    let cleared_capture = app
        .state::<AppState>()
        .voice_command_capture
        .lock()
        .map(|mut capture| capture.take().is_some())
        .unwrap_or(false);
    let pending = app
        .state::<AppState>()
        .pending_manual_command
        .lock()
        .ok()
        .and_then(|slot| slot.clone());
    let pending = pending.filter(|pending| pending.meeting_id == meeting_id);
    if let Some(pending) = pending.as_ref() {
        clear_pending_manual_command(app, pending);
    }
    if cleared_capture || pending.is_some() {
        let _ = app.emit(
            crate::events::EVENT_VOICE_COMMAND_LISTENING,
            crate::events::VoiceCommandListeningPayload { active: false },
        );
        let _ = app.emit(
            crate::events::EVENT_VOICE_ACTION_RESULT,
            crate::voice_action::VoiceActionResult::capture_unavailable(),
        );
    }
}

fn pending_matches_stop(
    pending: &crate::state::PendingManualCommand,
    meeting_id: &str,
    postprocess_token: &crate::perf::RecordingSessionToken,
) -> bool {
    pending.meeting_id == meeting_id && pending.recording_token.same_session_as(postprocess_token)
}

fn clear_pending_manual_command(app: &AppHandle, pending: &crate::state::PendingManualCommand) {
    if let Ok(mut slot) = app.state::<AppState>().pending_manual_command.lock() {
        if slot.as_ref().is_some_and(|current| {
            current.capture_generation == pending.capture_generation
                && current.meeting_id == pending.meeting_id
                && current
                    .recording_token
                    .same_session_as(&pending.recording_token)
        }) {
            *slot = None;
        }
    }
}

fn pending_is_owned(app: &AppHandle, pending: &crate::state::PendingManualCommand) -> bool {
    app.state::<AppState>()
        .pending_manual_command
        .lock()
        .map(|slot| {
            slot.as_ref().is_some_and(|current| {
                current.capture_generation == pending.capture_generation
                    && current.meeting_id == pending.meeting_id
                    && current
                        .recording_token
                        .same_session_as(&pending.recording_token)
            })
        })
        .unwrap_or(false)
}

/// Advance the MANUAL voice-command capture independently of ordinary captions. While it remains
/// inside its absolute source-frame budget this is a cheap state check and returns
/// `KeepListening`; it does not repeatedly transcribe a growing native-rate Vec. On user Stop or
/// the hard cap, it copies the exact clamped post-click range, resamples it once, transcribes once,
/// and compare-before-clears only the same capture generation.
fn step_manual_capture(
    app: &AppHandle,
    transcriber: &Transcriber,
    lang: Option<&str>,
    clip_source: &crate::audio::source::ManualClipSource,
) -> Option<ManualCaptureDecision> {
    let state = app.state::<AppState>();
    let current = { (*state.voice_command_capture.lock().ok()?)? };

    let captured_end = {
        let recorder = state.recorder.lock().ok()?;
        recorder.as_ref().map(|recorder| recorder.total_samples())
    };
    let source_cap_reached = match (captured_end, current.max_end_sample) {
        (Some(end), Some(max_end)) => end >= max_end,
        (None, _) => true,
        _ => false,
    };
    if !current.ended && !source_cap_reached {
        return Some(ManualCaptureDecision::KeepListening);
    }

    let clip_lang = resolve_clip_lang(lang, &state);
    let clip_lang_ref = clip_lang.as_deref();
    let command = match (
        current.start_sample,
        captured_end.or(current.max_end_sample),
    ) {
        (Some(start), Some(captured_end)) => {
            let end = current
                .max_end_sample
                .unwrap_or(captured_end)
                .min(captured_end);
            match clip_source.read_16k(start, end) {
                Ok(crate::audio::source::ManualClipRead::Pending) => {
                    return Some(ManualCaptureDecision::AwaitingDurable {
                        generation: current.generation,
                    })
                }
                Ok(crate::audio::source::ManualClipRead::Ready(samples)) => {
                    transcribe_manual_samples(transcriber, clip_lang_ref, &samples)
                }
                Err(error) => {
                    tracing::warn!(target: "voice", error = %error, "durable manual clip could not be read");
                    let mut guard = state.voice_command_capture.lock().ok()?;
                    if guard.as_ref().map(|capture| capture.generation) != Some(current.generation)
                    {
                        return None;
                    }
                    *guard = None;
                    return Some(ManualCaptureDecision::Unavailable);
                }
            }
        }
        _ => String::new(),
    };

    let terminal = crate::state::CaptureState {
        ended: true,
        ..current
    };
    let (decision, next) = decide_manual_capture(terminal, &command);
    let mut guard = state.voice_command_capture.lock().ok()?;
    if guard.as_ref().map(|capture| capture.generation) != Some(current.generation) {
        // A fresh arm replaced this one while the bounded final transcription was running. Keep
        // the new generation and suppress every stale event/dispatch from the old result.
        return None;
    }
    if matches!(&decision, ManualCaptureDecision::Dispatch { .. }) {
        // Publication into `pending_manual_command` is the ownership transfer. Keep the terminal
        // capture visible until apply performs capture -> pending atomically, so Begin cannot arm a
        // new generation in the gap between transcription and handoff.
        *guard = Some(current);
    } else {
        *guard = next;
    }
    Some(decision)
}

/// Transcribe the already-clamped, streamed-to-16k durable clip once.
fn transcribe_manual_samples(
    transcriber: &Transcriber,
    lang: Option<&str>,
    samples_16k: &[f32],
) -> String {
    if samples_16k.len() < 16_000 * 2 / 5 {
        return String::new();
    }
    match transcriber.transcribe(samples_16k, lang) {
        Ok(t) => t
            .segments
            .iter()
            .map(|s| s.text.trim())
            .collect::<Vec<_>>()
            .join(" ")
            .trim()
            .to_string(),
        Err(e) => {
            tracing::debug!(target: "live", error = %e, "manual-capture transcribe failed");
            String::new()
        }
    }
}

/// Resolve the forced language for a manual-capture command clip from the live-loop `lang` (which is
/// `config.language` at spawn time) and the CURRENT config (re-read from `state` in case the user
/// changed it mid-recording). Side-effecting only in that it reads the config lock; the decision is
/// the pure [`resolve_clip_lang_core`].
fn resolve_clip_lang(loop_lang: Option<&str>, state: &AppState) -> Option<String> {
    let cfg_lang = state.config.lock().ok().and_then(|c| c.language.clone());
    resolve_clip_lang_core(loop_lang, cfg_lang.as_deref())
}

/// PURE, headless-testable core: pick the forced clip language. Prefer the freshest configured
/// language (the user may have set/changed it after the recording started), else the loop's spawn-
/// time language. A blank/whitespace value (`Some("")`) counts as UNSET → `None` (auto-detect). The
/// returned `Some(lang)` is what gets passed to Whisper for the short clip so it does NOT free-
/// auto-detect a 3s utterance into the wrong Slavic language.
fn resolve_clip_lang_core(loop_lang: Option<&str>, config_lang: Option<&str>) -> Option<String> {
    let norm = |s: Option<&str>| -> Option<String> {
        s.map(str::trim)
            .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("auto"))
            .map(str::to_string)
    };
    norm(config_lang).or_else(|| norm(loop_lang))
}

/// Standalone filler/backchannel utterances Whisper emits for a near-silent / noise-only clip, in
/// EN + PL. Normalized (lowercased, diacritics stripped, inner non-alphanumerics removed) before the
/// lookup, so "Uh," / "Eee…" / "Yyy" all collapse onto a member. A command consisting ONLY of these
/// (after splitting on whitespace) is NOT a real command.
const FILLERS: &[&str] = &[
    // English
    "uh", "uhh", "uhm", "um", "umm", "er", "err", "erm", "hmm", "hm", "hmmm", "ah", "aha", "ahh",
    "oh", "ohh", "eh", "ehh", "mm", "mmm", "mhm", "huh", "yeah", "ok", "okay",
    // Polish fillers / backchannels
    "eee", "ee", "yyy", "yy", "yhy", "ehe",
];

/// Whether a normalized token is a pure filler/backchannel (after stripping inner punctuation).
fn is_filler_token(tok: &str) -> bool {
    let t: String = tok
        .to_lowercase()
        .chars()
        .map(|c| match c {
            'ą' => 'a',
            'ć' => 'c',
            'ę' => 'e',
            'ł' => 'l',
            'ń' => 'n',
            'ó' => 'o',
            'ś' => 's',
            'ź' | 'ż' => 'z',
            other => other,
        })
        .filter(|c| c.is_alphanumeric())
        .collect();
    t.is_empty() || FILLERS.contains(&t.as_str())
}

/// Whether the heard `command` is a MEANINGFUL command worth dispatching, vs garbage/filler. PURE +
/// headless-testable. A command is garbage when, after trimming, EITHER:
/// - it has fewer than 2 tokens that are not pure filler, OR
/// - its non-filler alphanumeric content is shorter than ~8 chars (a single short word like "Uh").
///
/// So "Uh," / "eee" / "." / "a" / "ok" → NOT meaningful (keep listening); but real commands like
/// "zrób research o pogodzie" / "co wiemy o atlasie" → meaningful (dispatch). Conservative on the
/// short side so we never DROP a real one-word command that carries enough signal (≥8 non-filler
/// chars, e.g. "pogoda?" is 6 → borderline; we require it ride with a verb in practice — the manual
/// path's whole utterance is the command, which is virtually always ≥2 words).
fn is_meaningful_command(command: &str) -> bool {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let non_filler: Vec<&str> = tokens
        .iter()
        .copied()
        .filter(|t| !is_filler_token(t))
        .collect();
    // Count of meaningful (non-filler) tokens.
    let meaningful_tokens = non_filler.len();
    // Total non-filler alphanumeric chars (diacritic-insensitive count via char count of the kept
    // alphanumerics — we don't need the normalized form, just the length signal).
    let non_filler_chars: usize = non_filler
        .iter()
        .flat_map(|t| t.chars())
        .filter(|c| c.is_alphanumeric())
        .count();
    meaningful_tokens >= 2 || non_filler_chars >= 8
}

/// PURE, headless-testable core of the manual-capture step (CLICK-TO-STOP). Given the armed
/// [`CaptureState`] and the FULL `command` accumulated SINCE the click (the growing post-click
/// window's transcript), decide the outcome and the NEXT capture state (`None` = cleared on a
/// terminal outcome, `Some(decremented)` while still listening).
///
/// The capture ENDS only when the user clicked stop (`capture.ended == true`) OR the backstop cap is
/// reached (`budget` hits 0) — NOT on merely hearing speech. Until then we KEEP LISTENING so the user
/// can finish their whole question without being cut off mid-sentence. Rules (no wake word — the WHOLE
/// post-click utterance is the command; NEVER dispatch empty):
/// 1. The capture is DONE (`ended` set by the user's stop click, OR `budget` exhausted = backstop):
///    - accumulated command is meaningful → `Dispatch{command}` with the FULL utterance, clear.
///    - else (silent / pure filler) → `NothingHeard`, clear (graceful, never an empty dispatch).
/// 2. Still going (not ended, backstop not reached) → spend one tick of the backstop budget and
///    `KeepListening` (keep accumulating). On a non-`ended` capture this is the common path even when
///    real speech is heard — we wait for the user's stop click, only the backstop forces an end.
///
/// No I/O, no FFI, no egress — just the meaningfulness check + arithmetic.
fn decide_manual_capture(
    capture: crate::state::CaptureState,
    command: &str,
) -> (ManualCaptureDecision, Option<crate::state::CaptureState>) {
    let command = command.trim();
    // Spend one tick of the backstop budget; 0 means the safety cap is now reached this tick.
    let remaining = capture.budget.saturating_sub(1);
    let backstop_reached = remaining == 0;

    // The capture is DONE when the user clicked stop OR the backstop cap is reached.
    if capture.ended || backstop_reached {
        // GARBAGE FILTER: a too-short / pure-filler accumulation ("Uh,", "eee", "hmm", a lone char or
        // punctuation) is NOT a real command — Whisper emits these for a silent/noise clip. Treat it
        // as nothing heard rather than dispatching a Research on "Uh,". The user gets a clean
        // "nothing_heard", never a garbage card.
        if !command.is_empty() && is_meaningful_command(command) {
            (
                ManualCaptureDecision::Dispatch {
                    generation: capture.generation,
                    command: command.to_string(),
                },
                None,
            )
        } else {
            (ManualCaptureDecision::NothingHeard, None)
        }
    } else {
        // Still listening — accumulate the growing window, decrement the backstop, wait for the
        // user's stop click. We do NOT dispatch on hearing speech (CLICK-TO-STOP).
        (
            ManualCaptureDecision::KeepListening,
            Some(crate::state::CaptureState {
                budget: remaining,
                ..capture
            }),
        )
    }
}

/// Resolve a NON-EMPTY heard `command` to a [`crate::audio::wake::VoiceIntent`] using the
/// keyword fast-path, then the BRAIN, then a keyword fallback. PURE w.r.t. the reasoner (the only
/// effect is the reasoner call, which the caller runs off-thread):
/// 1. `parse_voice_intent` — deterministic PL+EN keyword parser. A RECOGNIZED intent wins immediately.
/// 2. Else ask the brain ([`crate::voice_action::interpret_with_brain`]) to map any phrasing/order to
///    a known action — so "poszukaj mi info o X", "zrób research o wakacjach", etc. all work.
/// 3. Else (brain unavailable / no-consent / no mapping) fall back to **Research over the literal
///    command** — a sensible default for a non-empty command, never an empty/Unknown dispatch.
fn resolve_command_intent(
    reasoner: &dyn crate::reason::LocalReasoner,
    command: &str,
) -> crate::audio::wake::VoiceIntent {
    let keyword = crate::audio::wake::parse_voice_intent(command);
    if !matches!(keyword, crate::audio::wake::VoiceIntent::Unknown { .. }) {
        return keyword; // keyword fast-path understood it.
    }
    // Keyword said Unknown but the command is non-empty → ask the brain (best-effort).
    if let Some(intent) = crate::voice_action::interpret_with_brain(reasoner, command) {
        return intent;
    }
    // Brain unavailable / no mapping → Research over the literal command is a fine default.
    crate::audio::wake::VoiceIntent::Research {
        topic: command.trim().to_string(),
    }
}

/// Pure, headless-testable core of the wake wiring: detect a wake utterance at the head of a
/// transcript `tail` and, on a hit, build the typed [`crate::events::WakeDetectedPayload`] (matched
/// wake token + command tail + deterministically-parsed intent). Returns `None` when nothing fires.
///
/// No I/O, no FFI, no egress — just `detect_wake` + `parse_voice_intent`. The live mic loop above
/// calls this and emits the payload; the real-mic precision is the Mac step (`cargo test` is not
/// proof for acoustic behaviour — see `crate::audio::wake`).
fn wake_event_for(tail: &str) -> Option<crate::events::WakeDetectedPayload> {
    let hit = crate::audio::wake::detect_wake(tail)?;
    let intent = crate::audio::wake::parse_voice_intent(&hit.command);
    Some(crate::events::WakeDetectedPayload {
        matched_phrase: hit.matched_phrase,
        command: hit.command,
        intent,
    })
}

// ── Live-meeting awareness: accumulate the rolling captions into a running transcript and inject it
// into the in-meeting assistant's context, so it can answer "what is this meeting about?" about the
// recording IN PROGRESS (whose segments aren't persisted until Stop). ──────────────────────────────

/// Hard cap on the accumulated live-transcript buffer (chars) — bounds memory over a multi-hour
/// meeting; the oldest text is trimmed from the FRONT (keep the recent tail, which is what matters).
const MAX_LIVE_TRANSCRIPT_CHARS: usize = 16_000;
/// How much of the live transcript (most-recent tail) is injected into the assistant's prompt. The
/// tail is what's relevant + bounds the per-turn token cost across the agentic loop.
const LIVE_TRANSCRIPT_INJECT_CHARS: usize = 6_000;

/// Append a rolling live CAPTION to the accumulated live transcript, removing the OVERLAP between the
/// end of `accumulated` and the start of `caption` (live captions are overlapping tail windows, so a
/// naive append would trIplicate every phrase). Word-level overlap: find the largest `k` where the
/// last `k` words of `accumulated` equal the first `k` words of `caption`, and append only the words
/// after `k`. Approximate (Whisper varies run-to-run) but good enough for the assistant's gist
/// context. Pure + cheap (no extra transcription — reuses the caption the loop already produced).
fn merge_live_caption(accumulated: &str, caption: &str) -> String {
    let cap = caption.trim();
    if cap.is_empty() {
        return accumulated.to_string();
    }
    let acc = accumulated.trim_end();
    if acc.is_empty() {
        return cap.to_string();
    }
    let acc_words: Vec<&str> = acc.split_whitespace().collect();
    let cap_words: Vec<&str> = cap.split_whitespace().collect();
    let max_k = acc_words.len().min(cap_words.len());
    let mut overlap = 0;
    for k in (1..=max_k).rev() {
        let acc_tail = &acc_words[acc_words.len() - k..];
        let cap_head = &cap_words[..k];
        if acc_tail
            .iter()
            .zip(cap_head)
            .all(|(a, b)| caption_words_match(a, b))
        {
            overlap = k;
            break;
        }
    }
    let new_part = cap_words[overlap..].join(" ");
    if new_part.is_empty() {
        return acc.to_string();
    }
    format!("{acc} {new_part}")
}

/// Whether two caption words are the SAME word for overlap detection, tolerating the transcription
/// variance Whisper shows between overlapping tails: Unicode case ("Że" vs "że" — ASCII-only
/// folding misses Polish diacritics) and leading/trailing punctuation ("piątek." vs "piątek").
/// COMPARISON ONLY — the appended text stays byte-original. Two punctuation-only tokens (both
/// normalize to empty) count as a match: both are transcription noise, so skipping one is safe.
fn caption_words_match(a: &str, b: &str) -> bool {
    normalize_caption_word(a) == normalize_caption_word(b)
}

/// Normalize one word for the overlap compare: trim leading/trailing non-alphanumerics (Unicode-
/// aware, so diacritics survive) and Unicode-lowercase the rest.
fn normalize_caption_word(w: &str) -> String {
    w.trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase()
}

/// Merge a caption into the shared live-transcript buffer (read-modify-write under its Mutex) and
/// trim to [`MAX_LIVE_TRANSCRIPT_CHARS`] from the FRONT. Best-effort: a poisoned lock is ignored so a
/// caption tick can never disrupt the recording.
fn accumulate_live_caption(state: &AppState, caption: &str) {
    if caption.trim().is_empty() {
        return;
    }
    if let Ok(mut buf) = state.live_transcript.lock() {
        let merged = merge_live_caption(&buf, caption);
        *buf = if merged.chars().count() > MAX_LIVE_TRANSCRIPT_CHARS {
            tail_chars(&merged, MAX_LIVE_TRANSCRIPT_CHARS)
                .trim_start_matches('…')
                .to_string()
        } else {
            merged
        };
    }
}

/// The in-flight context injected into the assistant's system prompt: the LIVE transcript tail of
/// the recording in progress + the user's own typed notes for the CURRENT meeting. BOTH are gated
/// by `meeting_is_visible` on the LIVE per-turn `unlocked` set, fail-closed: no current meeting
/// (`meeting_id` empty), a sealed-and-not-session-unlocked meeting, or a gate error injects
/// NOTHING. The live buffer only ever holds the CURRENT recording (cleared at recording start and
/// at Stop), so gating on the current meeting's visibility covers a mid-recording folder relock
/// (screen-share auto-relock included) WITHOUT wiping the user's in-flight buffer — a session
/// re-unlock makes the context inject again. Best-effort: a read error degrades to no injection,
/// never a failure.
fn gated_live_context(
    db: &crate::storage::Db,
    live_transcript: &std::sync::Mutex<String>,
    meeting_id: &str,
    unlocked: &std::collections::HashSet<String>,
) -> (String, String) {
    let visible =
        !meeting_id.is_empty() && db.meeting_is_visible(meeting_id, unlocked).unwrap_or(false);
    if !visible {
        return (String::new(), String::new());
    }
    let live = live_transcript
        .lock()
        .map(|t| t.clone())
        .unwrap_or_default();
    let typed = db.get_manual_notes(meeting_id).unwrap_or_default();
    (live, typed)
}

/// Brain v2 L4 — the RUNNING BULLETS of the recording in progress, GATED for prompt injection
/// EXACTLY like [`gated_live_context`]: the RAM buffer only ever holds the CURRENT recording
/// (cleared at recording start + Stop), and it is injected ONLY when the scope meeting is VISIBLE
/// to the LIVE per-turn `unlocked` set — fail-closed: no scope meeting, a sealed-not-unlocked
/// meeting, or a gate error yields "" (no injection, and [`compose_live_inject`] then degrades to
/// the legacy behavior). Best-effort: a poisoned lock degrades to "".
fn gated_live_bullets(
    db: &crate::storage::Db,
    live_bullets: &std::sync::Mutex<String>,
    meeting_id: &str,
    unlocked: &std::collections::HashSet<String>,
) -> String {
    let visible =
        !meeting_id.is_empty() && db.meeting_is_visible(meeting_id, unlocked).unwrap_or(false);
    if !visible {
        return String::new();
    }
    live_bullets.lock().map(|b| b.clone()).unwrap_or_default()
}

/// Chars of running bullets injected into the live-question prompt (Brain v2 L4).
const LIVE_BULLETS_INJECT_CHARS: usize = 2_000;
/// Chars of VERBATIM transcript tail injected ALONGSIDE the bullets (Brain v2 L4) — together a
/// tighter 4k budget than the legacy 6k raw tail, better for small models.
const LIVE_VERBATIM_INJECT_CHARS: usize = 2_000;

/// Brain v2 L4 — compose the live-question inject (PURE): with running `bullets` present the
/// injected "live transcript" becomes `≤2k bullets + ≤2k verbatim tail` (labeled sub-sections);
/// with bullets empty (feature off / stub / not visible / nothing noted yet) OR an empty live
/// buffer, the inject is EXACTLY the input `live` — byte-identical legacy behavior (the
/// `assistant_system_prompt` 6k tail + its empty-buffer branches are untouched). Both inputs are
/// already gated by the caller ([`gated_live_context`] / [`gated_live_bullets`]); the composed
/// block rides the SAME redaction firewall + consent gate as the rest of the prompt.
fn compose_live_inject(live: &str, bullets: &str) -> String {
    let b = bullets.trim();
    let l = live.trim();
    if b.is_empty() || l.is_empty() {
        return live.to_string();
    }
    format!(
        "RUNNING NOTES (auto-generated from this meeting so far):\n{}\n\nMOST RECENT TRANSCRIPT (verbatim):\n{}",
        tail_chars(b, LIVE_BULLETS_INJECT_CHARS),
        tail_chars(l, LIVE_VERBATIM_INJECT_CHARS)
    )
}

/// Phase 3 CROSS-MEETING USER MEMORY: synthesize the injectable memory brief from the CURRENTLY-
/// VISIBLE user facts (design spec C3/D3). The visibility gate lives in `list_user_facts_visible`
/// (source-meeting `visibility_clause`), so a sealed-and-not-session-unlocked meeting's user facts
/// are NEVER read here and NEVER injected — the brief is regenerated from the remaining visible
/// sources on every turn. Best-effort: a read error degrades to an EMPTY brief (no injection), never
/// a failure. The brief egresses only inside the already-redacted, consent-gated system prompt.
///
/// `enabled` is the config `user_memory_enabled` master gate: when FALSE the brief is EMPTY (no read,
/// no injection) — so turning memory off suppresses injection into the @brain loop too, identically
/// to Ask / per-meeting chat.
///
/// Brain v2 L2.2: `query` is the user's CURRENT command/question — the brief is RELEVANCE-FILTERED
/// (BM25 top-k over the SAME visible set via `build_memory_brief`); an empty query or zero hits
/// falls back to the full-list brief, byte-identical to the pre-L2.2 behavior.
fn gated_user_memory_brief(
    db: &crate::storage::Db,
    unlocked: &std::collections::HashSet<String>,
    enabled: bool,
    query: &str,
) -> String {
    if !enabled {
        return String::new();
    }
    crate::user_memory::build_memory_brief(db, query, unlocked)
}

/// Clear the accumulated live-transcript buffer. Called when a recording STOPS
/// (`commands::stop_recording`) so a stale tail can never be injected into assistant prompts after
/// Stop — nor keep egressing once the just-recorded folder is sealed — and by the lock-surface
/// hygiene below. PII rule (§8): logs only the cleared char COUNT, never the content. Best-effort:
/// a poisoned lock is ignored (the buffer is re-cleared at the next recording start anyway).
pub(crate) fn clear_live_transcript(live: &std::sync::Mutex<String>) {
    if let Ok(mut buf) = live.lock() {
        if !buf.is_empty() {
            tracing::debug!(
                target: "live",
                chars = buf.chars().count(),
                "cleared live-transcript buffer"
            );
            buf.clear();
        }
    }
}

/// Belt-and-braces RAM hygiene for the lock surface (`lock_folder` / `relock_all`): clear the
/// buffer ONLY when no recording is active (post clear-on-Stop it is normally already empty —
/// this is cheap idempotent hygiene). NEVER clears mid-recording: the user's in-flight buffer
/// stays, and egress correctness there is owned by the `gated_live_context` visibility gate.
pub(crate) fn clear_live_transcript_if_idle(live: &std::sync::Mutex<String>, is_recording: bool) {
    if !is_recording {
        clear_live_transcript(live);
    }
}

/// The most-recent `n` chars of `s` (on a char boundary), prefixed with `…` when truncated.
fn tail_chars(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= n {
        s.to_string()
    } else {
        let tail: String = chars[chars.len() - n..].iter().collect();
        format!("…{tail}")
    }
}

/// Build the in-meeting assistant's system prompt. When the LIVE transcript of the current recording
/// is present, inject its recent tail (≤ [`LIVE_TRANSCRIPT_INJECT_CHARS`]) so the brain can answer
/// questions about the meeting IN PROGRESS; otherwise the base prompt is unchanged (no regression when
/// not recording / before any caption). When the user has typed their OWN notes for this meeting
/// (`typed_notes`), inject them as their own bounded section (their emphasis — weight these),
/// truncated to the recent tail like the transcript. EMPTY `typed_notes` ⇒ the prompt is BYTE-IDENTICAL
/// to the pre-feature output (the section is simply absent). Both the injected transcript AND the typed
/// notes egress through the SAME redaction firewall as every other prompt
/// (`RedactingProvider::complete` scrubs `system` + `user`) — they are NOT a new egress class.
fn assistant_system_prompt(
    live_transcript: &str,
    typed_notes: &str,
    memory_brief: &str,
    recording_in_progress: bool,
) -> String {
    let base = "You are an in-meeting assistant. Answer the user's request CONCISELY (2-4 \
                sentences). Do not invent facts; if you cannot find the answer, say so plainly. \
                Decide what the user wants: for a plain QUESTION or conversation, just ANSWER it. \
                ONLY when the user asks you to MAKE / SAVE / DRAFT / WRITE a note (e.g. \"make me a \
                note about the decisions\", \"save that we ship Friday\"), call the propose_note tool \
                with the note content enriched from the meeting context — that drafts a note for the \
                user to review and accept; do NOT call propose_note for ordinary questions.";
    // Shared recording-awareness wording (Brain v2 L3, single-sourced): the three load-bearing
    // phrases — `prompts::{RECORDING_NOW_PHRASE, MEETING_JUST_STARTED_PHRASE,
    // NO_SUBSTITUTE_OTHER_MEETINGS_PHRASE}` — are INTERPOLATED into this prose (and into the
    // deterministic FLOOR's prose in `voice_action::rag_answer`) from ONE definition, so the two
    // prompts can never drift. The interpolation is byte-identical to the former literal prose —
    // the prompt-pinning tests below prove it.
    use crate::prompts::{
        MEETING_JUST_STARTED_PHRASE, NO_SUBSTITUTE_OTHER_MEETINGS_PHRASE, RECORDING_NOW_PHRASE,
    };
    let t = live_transcript.trim();
    let mut prompt = if t.is_empty() {
        if recording_in_progress {
            // A recording is LIVE but nothing has been transcribed yet (it just started, the user
            // hasn't spoken, or captions lag). "This meeting / this conversation / ta rozmowa" means
            // the recording IN PROGRESS — never other saved meetings. Do NOT let the model search the
            // vault and describe unrelated saved notes as if they were the current meeting (the
            // "brain talks about other recordings" bug): with no live content, the honest answer is
            // that the meeting just started.
            format!(
                "{base} A meeting is being {RECORDING_NOW_PHRASE}. When the user asks about \"this \
                 meeting\", \"this conversation\", \"ta rozmowa\" or similar, they mean the recording \
                 IN PROGRESS — NOT any other saved meeting. Nothing has been transcribed from it yet \
                 (it just started or the user has not spoken much). If you cannot answer from the \
                 current meeting, say plainly that the {MEETING_JUST_STARTED_PHRASE} and little has been \
                 captured so far — {NO_SUBSTITUTE_OTHER_MEETINGS_PHRASE} and describe them \
                 as if they were this one. Use your gated vault tools ONLY when the user EXPLICITLY \
                 asks about their saved notes/past meetings."
            )
        } else {
            format!(
                "{base} Ground answers in tool results from the user's own gated vault (and web / \
                 calendar when you use them)."
            )
        }
    } else {
        let tail = tail_chars(t, LIVE_TRANSCRIPT_INJECT_CHARS);
        // HONESTY: the buffer is mic-stream-only and carries no speaker labels — the prompt must
        // not invite "who said what" answers (any attribution from it would be hallucinated).
        format!(
            "{base} A meeting is being {RECORDING_NOW_PHRASE} — use the LIVE TRANSCRIPT below to answer \
             questions about THIS current meeting (its topic and what has been said so far), and your \
             gated tools for anything in the user's saved notes/vault. When the user asks what this \
             meeting/conversation is about, answer FROM THE LIVE TRANSCRIPT — do NOT substitute other \
             saved meetings as if they were this one.\n\nLIVE TRANSCRIPT — an \
             UNATTRIBUTED, possibly-partial rolling capture of the recent portion of the meeting from \
             the user's microphone side; it may be garbled and it does NOT indicate who said what, so \
             never attribute a statement to a specific speaker:\n{tail}"
        )
    };
    // The user's OWN typed notes for this meeting — their explicit emphasis, so the brain should
    // weight them. Appended as a distinct bounded section; absent when empty (prompt unchanged).
    let notes = typed_notes.trim();
    if !notes.is_empty() {
        let notes_tail = tail_chars(notes, LIVE_TRANSCRIPT_INJECT_CHARS);
        prompt.push_str(&format!(
            "\n\nUSER'S OWN TYPED NOTES for this meeting (their emphasis — weight these):\n{notes_tail}"
        ));
    }
    // Phase 3 CROSS-MEETING USER MEMORY: the synthesized brief of what the brain durably knows about
    // the USER across all meetings (preferences, ongoing work, commitments), regenerated from
    // currently-VISIBLE user facts only. Appended as a distinct bounded section; ABSENT when empty
    // (so the prompt is BYTE-IDENTICAL to the pre-feature output when there is no memory). It rides
    // the SAME redaction firewall + cloud-consent gate as the rest of the prompt — NOT a new egress
    // class.
    let brief = memory_brief.trim();
    if !brief.is_empty() {
        prompt.push_str(&format!(
            "\n\nWHAT YOU KNOW ABOUT THE USER (durable memory across meetings — use it to ground and \
             personalize your answers; it may be stale, so defer to anything the user says now):\n{brief}"
        ));
    }
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Performance-order regression: cheap skips precede even the bounded VAD copy, and the VAD
    /// verdict precedes the full native-rate rolling-window clone. At the supported 384 kHz ceiling
    /// the full clone is 21.5 MiB per attempt, so moving it above the verdict restores the bug.
    #[test]
    fn thermal_and_turn_defer_precede_live_tail_snapshot() {
        let source = include_str!("live.rs");
        let run_start = source.find("\nfn run(").expect("live run function");
        let run_end = source[run_start..]
            .find("\n/// One queued live surface")
            .map(|offset| run_start + offset)
            .expect("end of live run function");
        let run = &source[run_start..run_end];
        let reader_clone = run
            .find("recorder.sample_reader()")
            .expect("reader cloned under short recorder lock");
        let vad_snapshot = run
            .find("match sample_reader.snapshot_tail(frames)")
            .expect("bounded native-rate VAD snapshot");
        let vad_gate = run
            .find("if !next_vad_gate.should_decode(speech, bypass)")
            .expect("VAD decode decision");
        let full_snapshot = run
            .find("let snapshot = match sample_reader")
            .expect("full native-rate live snapshot");
        let thermal = run
            .find("if governor.captions_suspended() && !bypass")
            .expect("thermal caption gate");
        let turn_defer = run
            .find("if turn_defer.should_skip(user_turn, live_local)")
            .expect("user-turn defer gate");

        assert!(
            thermal < vad_snapshot,
            "thermal skip must avoid every tail clone"
        );
        assert!(
            turn_defer < vad_snapshot,
            "user-turn defer must avoid every tail clone"
        );
        assert!(
            reader_clone < vad_snapshot && vad_snapshot < vad_gate && vad_gate < full_snapshot,
            "full 14 s snapshot must remain behind the bounded VAD verdict"
        );
        let final_phase_check = run[full_snapshot..]
            .find("if model_token.validated_for_live_work().is_err()")
            .map(|offset| full_snapshot + offset)
            .expect("post-snapshot live-phase check");
        let asr = run
            .find("asr.transcribe_live(&samples_16k")
            .expect("native live ASR call");
        assert!(
            full_snapshot < final_phase_check && final_phase_check < asr,
            "Stop must invalidate the session before stale native ASR can start"
        );
    }

    #[test]
    fn every_live_asr_setting_resolves_to_exactly_one_whisper_runtime() {
        for requested in ["whisper", "parakeet", "unknown", ""] {
            assert_eq!(
                effective_live_asr_engine(requested),
                crate::transcribe::live_asr::ENGINE_WHISPER
            );
        }
    }
    use crate::audio::wake::VoiceIntent;

    // ── T1.4: the Silero VAD tick gate (pure decision matrix, stub-VAD) ──────────────────────────

    /// The full gate matrix over stubbed VAD verdicts: speech decodes + re-arms the hangover;
    /// silence spends the hangover then SKIPS; bypass (manual capture / wake window) always
    /// decodes; VAD-unavailable (`None`) fails OPEN (today's decode-every-tick behavior).
    #[test]
    fn live_vad_gate_decision_matrix() {
        // Speech ⇒ decode, hangover armed to 2.
        let mut g = LiveVadGate::default();
        assert!(g.should_decode(Some(true), false), "speech decodes");
        // Two silent ticks ride the hangover, the THIRD is skipped.
        assert!(
            g.should_decode(Some(false), false),
            "hangover tick 1 decodes"
        );
        assert!(
            g.should_decode(Some(false), false),
            "hangover tick 2 decodes"
        );
        assert!(
            !g.should_decode(Some(false), false),
            "silence with hangover spent SKIPS the decode"
        );
        assert!(
            !g.should_decode(Some(false), false),
            "continued silence keeps skipping"
        );
        // Speech returns ⇒ decode + hangover re-armed.
        assert!(
            g.should_decode(Some(true), false),
            "speech resumes decoding"
        );
        assert!(g.should_decode(Some(false), false), "hangover re-armed");

        // A FRESH gate (hangover 0) skips pure silence immediately…
        let mut fresh = LiveVadGate::default();
        assert!(
            !fresh.should_decode(Some(false), false),
            "a silent recording start is not decoded"
        );
        // …but BYPASS always decodes, without touching the hangover state…
        assert!(
            fresh.should_decode(Some(false), true),
            "bypass always decodes"
        );
        assert!(
            !fresh.should_decode(Some(false), false),
            "bypass did not arm a hangover"
        );
        // …and VAD-unavailable fails OPEN.
        assert!(
            fresh.should_decode(None, false),
            "no VAD verdict ⇒ gate disabled ⇒ decode (fail-open)"
        );
    }

    /// The first scan covers the entire post-load window. Later scans use exact unseen source
    /// frames plus overlap, independent of decode latency or thermal tick stretching.
    #[test]
    fn vad_scan_frames_cover_first_backlog_and_every_unseen_frame() {
        let rate = 48_000;
        let full = WINDOW_SECS * rate as usize;
        assert_eq!(
            vad_scan_frame_count(None, 30 * rate as usize, rate),
            full,
            "first post-model-load scan must inspect the whole resident window"
        );
        assert_eq!(
            vad_scan_frame_count(Some(10 * rate as usize), 13 * rate as usize, rate),
            5 * rate as usize,
            "3 s unseen plus 2 s overlap"
        );
        assert_eq!(
            vad_scan_frame_count(Some(10 * rate as usize), 19 * rate as usize, rate),
            11 * rate as usize,
            "a thermally stretched 9 s delta remains fully covered"
        );
        assert_eq!(
            vad_scan_frame_count(Some(0), 60 * rate as usize, rate),
            full,
            "a long skip is bounded by the resident rolling window"
        );
        assert_eq!(vad_scan_frame_count(Some(10), 10, 0), 0);
    }

    #[test]
    fn vad_cursor_requires_absolute_overlap_before_silence_can_skip_asr() {
        assert!(vad_snapshot_covers_cursor(None, 500, 900));
        assert!(vad_snapshot_covers_cursor(Some(700), 500, 900));
        assert!(vad_snapshot_covers_cursor(Some(500), 500, 900));
        assert!(vad_snapshot_covers_cursor(Some(900), 500, 900));
        assert!(
            !vad_snapshot_covers_cursor(Some(499), 500, 900),
            "a trimmed-away gap must fail open"
        );
        assert!(
            !vad_snapshot_covers_cursor(Some(901), 500, 900),
            "a regressed source must not advance the cursor"
        );
    }

    /// The wake-suppression window doubles as a VAD-gate bypass signal: active right after a
    /// fire, expired after `WAKE_DEDUP_TICKS` tick() calls.
    #[test]
    fn wake_dedup_suppression_window_reports_active_then_expires() {
        let mut d = WakeDedup::default();
        assert!(!d.is_suppressing(), "fresh dedup: no window");
        assert!(d.should_fire("zrób research o testach"));
        assert!(d.is_suppressing(), "window armed by the fire");
        for _ in 0..WAKE_DEDUP_TICKS {
            d.tick();
        }
        assert!(!d.is_suppressing(), "window expired after WAKE_DEDUP_TICKS");
    }

    // ── T0.1: the telemetry model label (non-PII: ggml size token or "custom") ───────────────────

    #[test]
    fn model_size_label_extracts_ggml_size_or_custom() {
        use std::path::Path;
        assert_eq!(model_size_label(Path::new("/m/ggml-small.bin")), "small");
        assert_eq!(
            model_size_label(Path::new("/m/ggml-large-v3-turbo-q8_0.bin")),
            "large-v3-turbo-q8_0"
        );
        assert_eq!(
            model_size_label(Path::new("/m/ggml-small.en.bin")),
            "small.en"
        );
        // A user-supplied arbitrary file name is NEVER echoed into logs.
        assert_eq!(
            model_size_label(Path::new("/Users/kim/my-meeting-model.bin")),
            "custom"
        );
        assert_eq!(model_size_label(Path::new("")), "custom");
    }

    // ── Brain v2 P0.3: in-flight assistant-turn dedup registry ────────────────────────────────────

    /// The dedup contract of `spawn_assistant_turn` (headless — the registry helpers off `AppHandle`):
    /// the FIRST begin for a key wins; a SECOND begin for the SAME key while in flight is refused;
    /// after the turn ends (the guard's decrement) the key begins again. RED before P0.3:
    /// `try_begin_turn` did not exist — overlapping wakes stacked concurrent generations.
    #[test]
    fn turn_dedup_refuses_second_in_flight_then_allows_after_end() {
        let registry = std::sync::Mutex::new(std::collections::HashMap::new());
        assert!(try_begin_turn(&registry, "m1"), "first begin must proceed");
        assert!(
            !try_begin_turn(&registry, "m1"),
            "a second turn for the same key while one is in flight must be dropped"
        );
        end_turn(&registry, "m1");
        assert!(
            try_begin_turn(&registry, "m1"),
            "after the in-flight turn ends the key must begin again"
        );
        end_turn(&registry, "m1");
        assert!(
            registry.lock().unwrap().is_empty(),
            "a fully-ended key is removed — the registry never grows across meetings"
        );
    }

    /// Dedup is PER KEY: a turn scoped to a different meeting (or the unscoped "" voice key) is NOT
    /// blocked by another meeting's in-flight turn. And ending a never-begun / already-ended key is a
    /// harmless no-op (never an underflow/panic).
    #[test]
    fn turn_dedup_is_per_key_and_end_is_saturating() {
        let registry = std::sync::Mutex::new(std::collections::HashMap::new());
        assert!(try_begin_turn(&registry, "m1"));
        assert!(
            try_begin_turn(&registry, "m2"),
            "a different meeting's turn must not be blocked"
        );
        assert!(
            try_begin_turn(&registry, ""),
            "the unscoped voice/wake key is independent too"
        );
        // Ending an unknown key / double-ending never panics or underflows.
        end_turn(&registry, "never-began");
        end_turn(&registry, "m1");
        end_turn(&registry, "m1"); // double end — saturating no-op
        assert!(try_begin_turn(&registry, "m1"), "m1 is free again");
    }

    // ── PR D thread identity: backend UUID fallback + thread-stamped trace payloads ───────────────

    /// The voice/wake path (and a thread-less text turn) has no FE-supplied thread id → the backend
    /// GENERATES a UUID v4, so every persisted exchange carries a thread identity going forward; an
    /// explicit @brain thread id passes through untouched, and a blank one counts as absent (an
    /// empty thread id is never persisted). RED before PR D: `ensure_thread_id` did not exist —
    /// voice rows persisted with no thread identity at all.
    #[test]
    fn ensure_thread_id_generates_uuid_when_absent() {
        let generated = ensure_thread_id(None);
        assert!(
            uuid::Uuid::parse_str(&generated).is_ok(),
            "a missing thread id must become a real UUID"
        );
        assert_eq!(
            ensure_thread_id(Some("t-7".into())),
            "t-7",
            "explicit id passes through"
        );
        assert!(
            uuid::Uuid::parse_str(&ensure_thread_id(Some("   ".into()))).is_ok(),
            "a blank id counts as absent and is regenerated"
        );
    }

    /// Every tool-trace chip payload carries the turn's thread id (camelCase `threadId` over IPC),
    /// so simultaneous threads attribute their chips without cross-bleed — the documented v1 gap.
    /// The payload stays non-PII: tool NAME + state + coarse count + an opaque UUID only. RED
    /// before PR D: `AssistantToolPayload` had no thread field (chips were unattributable).
    #[test]
    fn tool_trace_payload_carries_thread_id() {
        let running = tool_trace_payload("t-9", "search_meetings", "running", true, None);
        assert_eq!(running.thread_id.as_deref(), Some("t-9"));
        assert_eq!(running.state, "running");
        assert!(running.count.is_none());

        let done = tool_trace_payload("t-9", "search_meetings", "done", true, Some(3));
        assert_eq!(done.thread_id.as_deref(), Some("t-9"));
        assert_eq!(done.count, Some(3));
        // The FE reads camelCase — the serialized event must expose `threadId`.
        let json = serde_json::to_value(&done).unwrap();
        assert_eq!(json.get("threadId").and_then(|v| v.as_str()), Some("t-9"));
        assert_eq!(
            json.get("tool").and_then(|v| v.as_str()),
            Some("search_meetings")
        );
    }

    #[test]
    fn wake_event_for_builds_payload_with_parsed_intent_on_hit() {
        let p =
            wake_event_for("klodku zrób research o konkurencji").expect("vocative wake must fire");
        assert_eq!(p.matched_phrase, "klodku");
        assert_eq!(p.command, "zrób research o konkurencji");
        assert_eq!(
            p.intent,
            VoiceIntent::Research {
                topic: "konkurencji".into()
            }
        );
    }

    // ── #23 DEDUP: one spoken wake = one dispatch; a fresh wake later DOES fire ───────────────────

    #[test]
    fn wake_dedup_collapses_overlapping_repeats_of_the_same_wake() {
        // The #23 echo: the same "Klaudku zrób research" stays visible across several overlapping
        // ~14s tails. The FIRST tick fires; the next ticks (within the window) are SKIPPED.
        let mut d = WakeDedup::default();
        assert!(
            d.should_fire("zrób research o konkurencji"),
            "first detection must fire"
        );
        for _ in 0..WAKE_DEDUP_TICKS - 1 {
            d.tick();
            assert!(
                !d.should_fire("zrób research o konkurencji"),
                "an overlapping re-detection of the SAME command within the window must be skipped"
            );
        }
    }

    #[test]
    fn wake_dedup_normalizes_punctuation_and_case_across_tails() {
        // The same utterance re-transcribed with different trailing punctuation / casing across
        // overlapping tails still compares equal and is de-duplicated.
        let mut d = WakeDedup::default();
        assert!(d.should_fire("zrób research o konkurencji"));
        d.tick();
        assert!(
            !d.should_fire("Zrób research o konkurencji."),
            "case/punctuation-only differences must still dedup as the same command"
        );
    }

    #[test]
    fn wake_dedup_allows_a_different_command_immediately() {
        // RECALL: a DIFFERENT wake command in the same recording must fire right away, even while the
        // previous one's suppression window is still live (the user's "to ma zawsze łapać").
        let mut d = WakeDedup::default();
        assert!(
            d.should_fire("zrób research o konkurencji"),
            "first command fires"
        );
        d.tick();
        assert!(
            d.should_fire("co wiemy o atlasie"),
            "a fresh, different command must fire even within the prior window"
        );
    }

    #[test]
    fn wake_dedup_allows_the_same_command_again_after_the_window_lapses() {
        // The user genuinely asks the SAME thing again, later. Once the suppression window has fully
        // aged out, the same command fires again — dedup suppresses the echo, not a real re-ask.
        let mut d = WakeDedup::default();
        assert!(
            d.should_fire("zrób research o konkurencji"),
            "first ask fires"
        );
        for _ in 0..WAKE_DEDUP_TICKS {
            d.tick();
        }
        assert!(
            d.should_fire("zrób research o konkurencji"),
            "after the window lapses the same command must be allowed to fire again"
        );
    }

    #[test]
    fn wake_dedup_window_counts_from_the_first_fire_not_the_last_echo() {
        // A suppressed echo must NOT re-arm the window, else a long overlap would suppress forever.
        let mut d = WakeDedup::default();
        assert!(d.should_fire("zrób research"), "first fire arms the window");
        // Echo once mid-window: suppressed, and must not extend the cooldown.
        d.tick();
        assert!(!d.should_fire("zrób research"), "echo suppressed");
        // Age out the REMAINDER of the window counted from the FIRST fire.
        for _ in 0..WAKE_DEDUP_TICKS - 1 {
            d.tick();
        }
        assert!(
            d.should_fire("zrób research"),
            "the window must expire on schedule from the first fire, not be extended by echoes"
        );
    }

    #[test]
    fn wake_event_for_is_silent_on_ordinary_speech_and_empty() {
        assert!(wake_event_for("let's talk about the budget for friday").is_none());
        assert!(wake_event_for("cloud computing is the future").is_none());
        assert!(wake_event_for("").is_none());
    }

    #[test]
    fn realtime_reactions_off_does_not_dispatch_on_default_config() {
        // Default config has `realtime_reactions = false` ⇒ a wake hit is SURFACED, never dispatched.
        let cfg = crate::settings::AppConfig::default();
        assert!(
            !cfg.realtime_reactions,
            "the in-meeting assistant must be opt-in"
        );
        assert!(
            !should_dispatch(&cfg),
            "default (OFF) must not dispatch a voice action"
        );

        // The wake still FIRES regardless of the toggle — only the dispatch is gated by it, so OFF
        // is exactly today's behaviour (detect + surface).
        assert!(wake_event_for("klodku zrób research o konkurencji").is_some());
    }

    #[test]
    fn realtime_reactions_on_enables_dispatch() {
        let cfg = crate::settings::AppConfig {
            realtime_reactions: true,
            ..Default::default()
        };
        assert!(should_dispatch(&cfg), "ON must dispatch a voice action");
    }

    // ── MANUAL voice-command capture (the button trigger) ───────────────────
    use crate::reason::LocalReasoner;
    use crate::state::CaptureState;
    use serde_json::Value;

    fn armed(budget: u32) -> CaptureState {
        CaptureState {
            generation: 1,
            budget,
            start_sample: Some(0),
            max_end_sample: Some(60),
            ended: false,
        }
    }

    /// An armed capture the user has CLICKED STOP on (`end_voice_command` flipped `ended`).
    fn ended(budget: u32) -> CaptureState {
        CaptureState {
            generation: 1,
            budget,
            start_sample: Some(0),
            max_end_sample: Some(60),
            ended: true,
        }
    }

    #[test]
    fn manual_capture_still_listening_does_not_dispatch_on_hearing_speech() {
        // CLICK-TO-STOP: hearing a real, meaningful utterance while NOT ended (and under the backstop)
        // must KEEP LISTENING — the user controls when they're done, not a per-tick auto-dispatch.
        let cap = armed(20);
        let (decision, next) = decide_manual_capture(cap, "Więc tak, zrób web");
        assert_eq!(
            decision,
            ManualCaptureDecision::KeepListening,
            "a non-ended capture must keep accumulating, not cut the user off mid-question"
        );
        assert_eq!(
            next,
            Some(CaptureState {
                generation: 1,
                budget: 19,
                start_sample: Some(0),
                max_end_sample: Some(60),
                ended: false
            }),
            "a listening tick decrements the backstop and keeps the latched offset + ended flag"
        );
    }

    #[test]
    fn manual_capture_end_click_dispatches_full_accumulated_utterance_and_clears() {
        // The user clicked STOP (`ended`) with a full accumulated utterance → dispatch the WHOLE
        // trimmed command, clear the capture. This is the primary end of a CLICK-TO-STOP capture.
        let cap = ended(15);
        let (decision, next) =
            decide_manual_capture(cap, "  Więc tak, zrób web research o konkurencji  ");
        assert_eq!(
            decision,
            ManualCaptureDecision::Dispatch {
                generation: 1,
                command: "Więc tak, zrób web research o konkurencji".into()
            },
            "the user's stop click must dispatch the FULL accumulated (trimmed) utterance"
        );
        assert!(next.is_none(), "capture must be cleared after a dispatch");
    }

    #[test]
    fn manual_stop_pending_handoff_survives_draining_and_matches_postprocess_session() {
        // Headless lifecycle regression for the cross-Stop half of the flow. The file-backed
        // Pending -> durable transition itself is exercised by
        // `audio::source::manual_clip_handle_survives_owner_drop_and_transitions_pending_to_ready`;
        // here we prove the resulting real command is owned across recorder removal/Draining and
        // is accepted only by the same meeting + exact Postprocess session (never NothingHeard).
        let _serial = crate::perf::model_lifecycle_test_guard();
        crate::perf::reset_model_lifecycle_for_test();
        let mut owner = crate::perf::begin_recording_session().unwrap();
        owner.transition_to_live().unwrap();
        let live_token = owner.token().validated_for_live_work().unwrap();
        let (decision, next) =
            decide_manual_capture(ended(15), "zrób research o wydajności nagrywania");
        let ManualCaptureDecision::Dispatch {
            generation,
            command,
        } = decision
        else {
            panic!("durable exact command became NothingHeard instead of a pending dispatch");
        };
        assert!(next.is_none());
        let pending = crate::state::PendingManualCommand {
            meeting_id: "meeting-a".into(),
            capture_generation: generation,
            command: command.clone(),
            recording_token: live_token,
        };

        owner.transition_to_draining().unwrap();
        owner.transition_to_postprocess().unwrap();
        let post_token = owner.token().validated_for_postprocess().unwrap();
        assert!(pending_matches_stop(&pending, "meeting-a", &post_token));
        assert!(!pending_matches_stop(&pending, "meeting-b", &post_token));
        assert_eq!(pending.command, command);
        owner.finish().unwrap();
        crate::perf::reset_model_lifecycle_for_test();
    }

    #[test]
    fn manual_capture_backstop_cap_dispatches_a_real_utterance_as_a_backstop() {
        // No stop click, but the backstop cap is reached (budget hits 0 this tick) with a real
        // utterance accumulated → auto-stop + dispatch so the capture can't listen forever.
        let cap = armed(1); // this tick takes budget 1 → 0 = backstop reached
        let (decision, next) = decide_manual_capture(cap, "zrób research o konkurencji");
        assert_eq!(
            decision,
            ManualCaptureDecision::Dispatch {
                generation: 1,
                command: "zrób research o konkurencji".into()
            },
            "reaching the backstop cap with a real utterance must dispatch it (backstop)"
        );
        assert!(next.is_none(), "capture cleared at the backstop");
    }

    #[test]
    fn manual_capture_end_click_on_silent_capture_is_nothing_heard_not_empty_dispatch() {
        // The NO-EMPTY-DISPATCH guarantee: the user clicked stop but nothing was ever heard → graceful
        // NothingHeard, NEVER an empty Unknown dispatch.
        let (d, n) = decide_manual_capture(ended(15), "");
        assert_eq!(
            d,
            ManualCaptureDecision::NothingHeard,
            "an ended-but-silent capture must terminate as NothingHeard, never an empty dispatch"
        );
        assert!(n.is_none(), "capture must be cleared once it gives up");
    }

    #[test]
    fn manual_capture_backstop_with_nothing_heard_is_nothing_heard() {
        // No stop click, backstop reached, nothing ever heard → NothingHeard (graceful), never empty.
        let (d, n) = decide_manual_capture(armed(1), "   ");
        assert_eq!(
            d,
            ManualCaptureDecision::NothingHeard,
            "the backstop with a fully-silent capture must give up gracefully"
        );
        assert!(n.is_none(), "capture cleared at the backstop");
    }

    #[test]
    fn manual_capture_silent_tick_with_budget_left_keeps_listening_and_decrements() {
        // An EMPTY post-click window (no speech yet) but backstop remains → keep listening, budget −1.
        let cap = armed(3);
        let (decision, next) = decide_manual_capture(cap, "   ");
        assert_eq!(decision, ManualCaptureDecision::KeepListening);
        assert_eq!(
            next,
            Some(CaptureState {
                generation: 1,
                budget: 2,
                start_sample: Some(0),
                max_end_sample: Some(60),
                ended: false
            }),
            "a silent tick must decrement the backstop and keep the SAME latched offset + flag"
        );
    }

    #[test]
    fn nothing_heard_result_is_graceful_with_empty_command() {
        let r = crate::voice_action::VoiceActionResult::nothing_heard();
        assert_eq!(r.status, "nothing_heard");
        assert!(
            r.command.is_empty(),
            "nothing was heard ⇒ no command surfaced"
        );
        assert!(
            r.summary.contains("didn't hear"),
            "friendly nudge to click + speak again, not 'didn't catch an action'"
        );
    }

    #[test]
    fn manual_capture_default_backstop_is_generous_not_a_felt_cutoff() {
        // CLICK-TO-STOP: the default backstop must be GENEROUS (~20 ticks ≈ 60s at the 3s TICK) so a
        // normal-length question never feels cut off — the user's stop click is the primary end, this
        // is only the can't-listen-forever safety cap.
        assert!(
            (15..=30).contains(&CaptureState::DEFAULT_BUDGET),
            "the backstop cap must be generous (~60s) so it never feels like a cutoff mid-question"
        );
    }

    // ── resolve_command_intent: keyword fast-path → brain → fallback ─────────

    /// Maps "research" for any non-empty command via `structured`, so we can prove the brain path.
    struct BrainResearch;
    impl LocalReasoner for BrainResearch {
        fn id(&self) -> &str {
            "brain-research"
        }
        fn reason(&self, _s: &str, _u: &str) -> crate::error::Result<String> {
            Ok(String::new())
        }
        fn structured(&self, _s: &str, _u: &str, _schema: &Value) -> crate::error::Result<Value> {
            Ok(serde_json::json!({ "action": "research", "argument": "wakacjach" }))
        }
    }

    /// A reasoner that always errors — proves the keyword FALLBACK kicks in when the brain is down.
    struct DeadBrain;
    impl LocalReasoner for DeadBrain {
        fn id(&self) -> &str {
            "dead"
        }
        fn reason(&self, _s: &str, _u: &str) -> crate::error::Result<String> {
            Err(crate::error::AppError::Unavailable("no consent".into()))
        }
        fn structured(&self, _s: &str, _u: &str, _schema: &Value) -> crate::error::Result<Value> {
            Err(crate::error::AppError::Unavailable("no consent".into()))
        }
    }

    #[test]
    fn resolve_keyword_fast_path_wins_without_touching_the_brain() {
        // A keyword-recognized command never calls the brain (DeadBrain would error if it did).
        let intent = resolve_command_intent(&DeadBrain, "zrób research o konkurencji");
        assert_eq!(
            intent,
            VoiceIntent::Research {
                topic: "konkurencji".into()
            }
        );
    }

    #[test]
    fn resolve_unknown_command_uses_the_brain_mapping() {
        // Keyword Unknown ("poszukaj mi info o…") → brain maps it to Research over its argument.
        let intent = resolve_command_intent(&BrainResearch, "poszukaj mi info o wakacjach");
        assert_eq!(
            intent,
            VoiceIntent::Research {
                topic: "wakacjach".into()
            }
        );
    }

    #[test]
    fn resolve_unknown_command_falls_back_to_research_when_brain_unavailable() {
        // Keyword Unknown + brain unavailable (no consent) → Research over the LITERAL command, a
        // sensible non-empty default; never an empty/Unknown dispatch.
        let intent = resolve_command_intent(&DeadBrain, "find me the latest on widgets");
        assert_eq!(
            intent,
            VoiceIntent::Research {
                topic: "find me the latest on widgets".into()
            },
            "a non-empty command with no brain must default to Research over the literal text"
        );
    }

    // ── DISPATCH: the floor no longer owns WRITE routing (conversation-first design, 2026-06-30) ────
    // The agentic loop (with writes) is the SINGLE executive path — the agent DECIDES answer-vs-write,
    // NOT a hardcoded classifier branch. The intent is resolved ONLY for the informational floor, and
    // the floor is INFORMATIONAL ONLY: a parsed write intent is demoted to a Research so a no-consent /
    // local user never gets a hardcoded write the agent never chose.

    #[test]
    fn floor_demotes_write_intents_to_informational_research() {
        // A "save"-style phrase that the classifier parses as a WRITE (NoteAside) must NOT drive a
        // hardcoded write on the floor — it is demoted to a Research over the literal command. (The
        // actual answer-vs-act DECISION belongs to the agentic loop, not this floor.)
        let note = VoiceIntent::NoteAside {
            text: "send the deck to Anna".into(),
        };
        assert_eq!(
            floor_intent_for(&note, "note that I send the deck to Anna"),
            VoiceIntent::Research {
                topic: "note that I send the deck to Anna".into()
            },
            "the floor must NOT perform a hardcoded NoteAside write — it answers informationally"
        );

        let reminder = VoiceIntent::CreateReminder {
            text: "email Bob".into(),
            due: None,
        };
        assert_eq!(
            floor_intent_for(&reminder, "remind me to email Bob"),
            VoiceIntent::Research { topic: "remind me to email Bob".into() },
            "the floor must NOT perform a hardcoded CreateReminder write — it answers informationally"
        );
    }

    #[test]
    fn floor_passes_read_intents_through_unchanged_no_regression() {
        // Read intents are UNTOUCHED by the floor demotion ⇒ the no-consent/local informational answer
        // is exactly today's behavior (zero regression). Research/Recall/SlackSearch/Unknown all pass.
        for intent in [
            VoiceIntent::Research {
                topic: "atlas pricing".into(),
            },
            VoiceIntent::Recall {
                entity: "Anna".into(),
            },
            VoiceIntent::SlackSearch {
                query: "raport".into(),
            },
            VoiceIntent::Unknown {
                raw: "gibberish".into(),
            },
        ] {
            assert_eq!(
                floor_intent_for(&intent, "the literal command"),
                intent,
                "a read intent must reach the floor unchanged: {intent:?}"
            );
        }
    }

    // ── FIX 3: GARBAGE / FILLER filter — "Uh," / "eee" / single char must NOT dispatch ───────────

    #[test]
    fn is_meaningful_command_rejects_filler_and_too_short() {
        // Pure fillers / single short tokens / punctuation → NOT meaningful.
        for junk in [
            "Uh,", "uh", "eee", "Hmm", "yyy", "aha", "ok", ".", ",", "a", "—", "  uh  ", "eh",
        ] {
            assert!(
                !is_meaningful_command(junk),
                "garbage/filler {junk:?} must NOT be a meaningful command"
            );
        }
        // Real commands → meaningful.
        for good in [
            "zrób research o pogodzie",
            "co wiemy o atlasie",
            "jaka była pogoda",
            "do research on pricing",
            "przypomnij mi o spotkaniu",
        ] {
            assert!(
                is_meaningful_command(good),
                "real command {good:?} must be meaningful"
            );
        }
    }

    #[test]
    fn decide_manual_capture_end_click_on_garbage_only_is_nothing_heard() {
        // The user clicked stop but only garbage/filler ("eee") was accumulated → NothingHeard
        // (graceful), never a garbage Research dispatch.
        let (d, n) = decide_manual_capture(ended(10), "eee");
        assert_eq!(
            d,
            ManualCaptureDecision::NothingHeard,
            "an ended garbage-only capture ends as NothingHeard, never a garbage dispatch"
        );
        assert!(n.is_none(), "capture cleared once it gives up");
    }

    #[test]
    fn decide_manual_capture_end_click_dispatches_a_real_command() {
        // The user clicked stop with a genuine command → dispatch the (trimmed) command verbatim.
        let (d, n) = decide_manual_capture(ended(15), "  zrób research o pogodzie  ");
        assert_eq!(
            d,
            ManualCaptureDecision::Dispatch {
                generation: 1,
                command: "zrób research o pogodzie".into()
            },
            "an ended real command must dispatch"
        );
        assert!(n.is_none(), "capture cleared on dispatch");
    }

    // ── FIX 2: forced clip language threading (config.language reaches the clip) ──────────────────

    #[test]
    fn resolve_clip_lang_core_forces_configured_language() {
        // A configured language is forced for the short command clip (the param that reaches
        // `transcribe_since` → Whisper), so a 3s Polish clip is NOT free-auto-detected as Russian.
        assert_eq!(
            resolve_clip_lang_core(None, Some("pl")).as_deref(),
            Some("pl"),
            "config.language must force the clip language even when the loop lang was None"
        );
        // The freshest config language wins over the spawn-time loop language.
        assert_eq!(
            resolve_clip_lang_core(Some("en"), Some("pl")).as_deref(),
            Some("pl"),
            "the freshest configured language must win"
        );
        // Loop lang is used when config is unset.
        assert_eq!(
            resolve_clip_lang_core(Some("de"), None).as_deref(),
            Some("de")
        );
        // Blank / "auto" / both-unset → None (whisper auto-detects; user can set it in Settings).
        assert_eq!(resolve_clip_lang_core(None, Some("")), None);
        assert_eq!(resolve_clip_lang_core(None, Some("auto")), None);
        assert_eq!(resolve_clip_lang_core(Some("  "), Some("  ")), None);
        assert_eq!(resolve_clip_lang_core(None, None), None);
    }

    #[test]
    fn merge_live_caption_seeds_then_dedups_overlap() {
        // First caption seeds the buffer.
        let a = merge_live_caption("", "we need to ship the beta");
        assert_eq!(a, "we need to ship the beta");
        // Rolling captions OVERLAP (same tail re-transcribed) — the shared head must NOT be duplicated;
        // only the genuinely-new tail is appended.
        let b = merge_live_caption(&a, "ship the beta on Friday");
        assert_eq!(b, "we need to ship the beta on Friday");
        // A caption fully contained in the buffer adds nothing.
        let c = merge_live_caption(&b, "on Friday");
        assert_eq!(c, "we need to ship the beta on Friday");
    }

    #[test]
    fn merge_live_caption_handles_empty_and_no_overlap() {
        // Empty caption leaves the buffer untouched; empty buffer takes the caption verbatim.
        assert_eq!(merge_live_caption("hello world", "   "), "hello world");
        assert_eq!(merge_live_caption("", "fresh start"), "fresh start");
        // No shared boundary ⇒ space-joined (a gap in captions, not an overlap).
        assert_eq!(
            merge_live_caption("budget approved", "contract signed"),
            "budget approved contract signed"
        );
    }

    #[test]
    fn merge_live_caption_folds_unicode_case_for_overlap() {
        // PR-A #4a: Whisper re-transcribes the overlapping tail with varying capitalization —
        // "że" vs "Że". `eq_ignore_ascii_case` does NOT fold non-ASCII (Polish diacritics), so the
        // old compare missed the overlap and DUPLICATED the shared words. Unicode folding must
        // detect it; the appended text stays byte-original.
        let merged = merge_live_caption("mówił że projekt", "Że projekt ruszy w piątek");
        assert_eq!(
            merged, "mówił że projekt ruszy w piątek",
            "a Unicode-case-only difference must still merge without duplication"
        );
    }

    #[test]
    fn merge_live_caption_ignores_edge_punctuation_for_overlap() {
        // PR-A #4b: trailing-punctuation variance between overlapping tails ("piątek." vs
        // "piątek") must not break overlap detection. The ACCUMULATED text keeps its original
        // bytes (incl. the "."); only the comparison is normalized.
        let merged = merge_live_caption("spotkamy się w piątek.", "w piątek omówimy budżet");
        assert_eq!(
            merged, "spotkamy się w piątek. omówimy budżet",
            "punctuation-only variance must still dedup the shared overlap"
        );
    }

    /// PR-A #5 HONESTY: the live buffer is a mic-side, unattributed rolling capture — the prompt
    /// must SAY so, and must not claim the transcript can answer "who said what" (any speaker
    /// attribution from it would be hallucinated).
    #[test]
    fn assistant_system_prompt_is_honest_about_attribution() {
        let p = assistant_system_prompt("we shipped the beta", "", "", false);
        let lower = p.to_lowercase();
        assert!(
            lower.contains("unattributed"),
            "must state the transcript is unattributed: {p}"
        );
        assert!(
            lower.contains("microphone"),
            "must state the mic-side capture origin: {p}"
        );
        assert!(
            !p.contains("its topic, decisions, who said what"),
            "must not claim the transcript knows who said what: {p}"
        );
    }

    /// A recording is LIVE but nothing has been transcribed yet (0:31 in, captions still lagging).
    /// The prompt MUST scope "this meeting/conversation" to the recording in progress and MUST NOT
    /// invite a vault search that describes OTHER saved meetings as if they were this one — the
    /// user-reported "brain talks about my other recordings" bug. RED before this fix: with an
    /// empty live buffer the prompt said only "Ground answers in tool results from the user's own
    /// gated vault", so the agent semantic-searched the vault and summarized unrelated meetings.
    #[test]
    fn assistant_system_prompt_scopes_empty_buffer_to_the_live_recording() {
        let p = assistant_system_prompt("", "", "", /* recording_in_progress */ true);
        assert!(
            p.contains("recorded RIGHT NOW"),
            "must tell the model a meeting is being recorded now: {p}"
        );
        assert!(
            p.contains("meeting just started"),
            "must offer the honest 'meeting just started' answer when nothing is captured: {p}"
        );
        assert!(
            p.contains("do NOT search the vault for other saved meetings"),
            "must forbid substituting other saved meetings for the current one: {p}"
        );
        assert!(
            !p.contains("Ground answers in tool results from the user's own gated vault"),
            "the empty-recording branch must NOT invite a vault search: {p}"
        );
        // When NOT recording, the same empty buffer keeps the old vault-grounding behavior (Ask /
        // out-of-meeting card) — no regression.
        let idle = assistant_system_prompt("", "", "", /* recording_in_progress */ false);
        assert!(
            idle.contains("Ground answers in tool results from the user's own gated vault"),
            "out-of-meeting empty prompt keeps vault-grounding: {idle}"
        );
        assert!(
            !idle.contains("recorded RIGHT NOW"),
            "no live-recording claim when not recording: {idle}"
        );
    }

    // ── PR-A #2: the live-tail injection is GATED on meeting visibility (fail-closed) ─────────────

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn tmp_db(tag: &str) -> crate::storage::Db {
        let p = crate::storage::db::unique_temp_path(&format!("murmur-live-gate-{tag}"), "sqlite");
        let _ = std::fs::remove_file(&p);
        crate::storage::Db::open_with_key(&p, TEST_DEK).unwrap()
    }

    /// Seed one summarized meeting with a note, foldered into `folder_id` when given.
    fn seed_meeting(db: &crate::storage::Db, mid: &str, folder_id: Option<&str>) {
        db.insert_meeting(&crate::storage::Meeting {
            id: mid.to_string(),
            started_at: "2026-07-01T09:00:00Z".to_string(),
            ended_at: None,
            title: Some("Sync".to_string()),
            duration_s: 60,
            audio_path: None,
            status: crate::storage::MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&crate::storage::NoteRecord {
            meeting_id: mid.to_string(),
            provider_id: "claude_code".to_string(),
            markdown: "# note".to_string(),
            created_at: "2026-07-01T09:05:00Z".to_string(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_meeting_folder(mid, folder_id).unwrap();
    }

    #[test]
    fn compose_live_inject_is_identity_without_bullets_and_composes_with() {
        // Brain v2 L4 — no bullets (feature off / stub / nothing noted): the inject is EXACTLY the
        // legacy live string, so every downstream prompt is byte-identical.
        assert_eq!(compose_live_inject("live tail", ""), "live tail");
        assert_eq!(compose_live_inject("live tail", "  \n"), "live tail");
        // Empty live buffer: bullets alone must NOT flip the empty-buffer prompt branches.
        assert_eq!(compose_live_inject("", "- [a]: b"), "");
        // Both present: labeled bullets + verbatim tail, each bounded at its 2k budget.
        let long_live = "w ".repeat(3_000); // 6k chars
        let composed = compose_live_inject(&long_live, "- [deal]: pricing agreed");
        assert!(composed.starts_with(
            "RUNNING NOTES (auto-generated from this meeting so far):\n- [deal]: pricing agreed"
        ));
        let verbatim = composed
            .split("MOST RECENT TRANSCRIPT (verbatim):\n")
            .nth(1)
            .expect("verbatim section present");
        assert!(
            verbatim.chars().count() <= LIVE_VERBATIM_INJECT_CHARS + 1, // + the '…' marker
            "verbatim tail bounded at 2k, got {}",
            verbatim.chars().count()
        );
        // The composed block stays under the prompt's own 6k tail budget → never re-truncated.
        assert!(composed.chars().count() < LIVE_TRANSCRIPT_INJECT_CHARS);
        // And it flows into the live prompt as the injected transcript section.
        let prompt = assistant_system_prompt(&composed, "", "", true);
        assert!(prompt.contains("RUNNING NOTES"));
        assert!(prompt.contains("- [deal]: pricing agreed"));
    }

    /// Brain v2 L4 — the bullets prompt read is gated EXACTLY like the live buffer: a
    /// sealed-not-unlocked scope meeting injects NO bullets (fail-closed), a session unlock
    /// re-injects them, and the RAM buffer itself is never wiped by the gate.
    #[test]
    fn gated_live_bullets_masks_for_sealed_meeting_and_reinjects_on_unlock() {
        let db = tmp_db("bullets-gate");
        db.insert_folder(&crate::storage::Folder {
            id: "f1".into(),
            name: "Secret".into(),
            path: "Secret".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-01T00:00:00Z".into(),
        })
        .unwrap();
        seed_meeting(&db, "m1", Some("f1"));
        db.set_folder_locked("f1", true, Some(&b"wrapped"[..]))
            .unwrap();

        let bullets = std::sync::Mutex::new("- [deal]: pricing agreed".to_string());
        let unlocked = std::collections::HashSet::new();
        assert!(
            gated_live_bullets(&db, &bullets, "m1", &unlocked).is_empty(),
            "sealed-not-unlocked meeting must inject NO bullets"
        );
        assert!(
            gated_live_bullets(&db, &bullets, "", &unlocked).is_empty(),
            "no scope meeting ⇒ no injection (fail-closed default)"
        );
        // Session unlock → the SAME buffer injects again (the gate is the live set, not a wipe).
        let mut unlocked2 = std::collections::HashSet::new();
        unlocked2.insert("f1".to_string());
        assert_eq!(
            gated_live_bullets(&db, &bullets, "m1", &unlocked2),
            "- [deal]: pricing agreed"
        );
        assert_eq!(*bullets.lock().unwrap(), "- [deal]: pricing agreed");
    }

    #[test]
    fn gated_live_context_masks_live_tail_when_meeting_not_visible() {
        // The mid-recording-relock window: the current meeting's folder is sealed and NOT
        // session-unlocked → the live tail (still legitimately in RAM) must NOT be injected into
        // the assistant's prompt. Fail-closed, exactly like the typed notes.
        let db = tmp_db("masked");
        db.insert_folder(&crate::storage::Folder {
            id: "f1".into(),
            name: "Secret".into(),
            path: "Secret".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-01T00:00:00Z".into(),
        })
        .unwrap();
        seed_meeting(&db, "m1", Some("f1"));
        db.set_folder_locked("f1", true, Some(&b"wrapped"[..]))
            .unwrap();

        let live = std::sync::Mutex::new("sealed meeting tail still in RAM".to_string());
        let unlocked = std::collections::HashSet::new();
        let (tail, notes) = gated_live_context(&db, &live, "m1", &unlocked);
        assert!(
            tail.is_empty(),
            "sealed-not-unlocked meeting must inject NO live tail"
        );
        assert!(
            notes.is_empty(),
            "sealed-not-unlocked meeting must inject NO typed notes"
        );
        let prompt = assistant_system_prompt(&tail, &notes, "", false);
        assert!(
            !prompt.contains("LIVE TRANSCRIPT"),
            "no live section for a sealed meeting: {prompt}"
        );
        assert!(
            !prompt.contains("sealed meeting tail"),
            "the RAM buffer must not reach the prompt"
        );
        // The in-flight buffer itself is NOT wiped — a session re-unlock re-injects it.
        assert_eq!(*live.lock().unwrap(), "sealed meeting tail still in RAM");
    }

    /// (B/gate) RED-before-GREEN (2026-07-08): the deterministic floor's CURRENT-meeting grounding is
    /// built from `gated_live_context` → `floor_current_context`. A sealed-not-visible meeting yields
    /// EMPTY gated context, so the floor's current-meeting block is empty (nothing sealed leaks into
    /// the local-brain floor prompt). A VISIBLE meeting produces a labeled, non-empty block.
    #[test]
    fn floor_current_context_is_empty_for_sealed_meeting_and_labeled_for_visible() {
        let db = tmp_db("floor-ctx");
        // Sealed-not-unlocked meeting → gated context is empty → floor current block is empty.
        db.insert_folder(&crate::storage::Folder {
            id: "f1".into(),
            name: "Secret".into(),
            path: "Secret".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-01T00:00:00Z".into(),
        })
        .unwrap();
        seed_meeting(&db, "sealed", Some("f1"));
        db.set_manual_notes("sealed", "SECRET typed note").unwrap();
        db.set_folder_locked("f1", true, Some(&b"wrapped"[..]))
            .unwrap();
        let live = std::sync::Mutex::new("SECRET live tail".to_string());
        let unlocked = std::collections::HashSet::new();
        let (l, n) = gated_live_context(&db, &live, "sealed", &unlocked);
        let ctx = floor_current_context(&l, &n);
        assert!(
            ctx.is_empty(),
            "a sealed-not-visible meeting must contribute NO floor current-meeting grounding: {ctx}"
        );
        assert!(
            !ctx.contains("SECRET"),
            "sealed content must NOT leak into the floor grounding"
        );

        // A visible in-progress recording → non-empty, labeled block containing tail + typed notes.
        db.insert_meeting(&crate::storage::Meeting {
            id: "m-rec".to_string(),
            started_at: "2026-07-01T09:00:00Z".to_string(),
            ended_at: None,
            title: None,
            duration_s: 0,
            audio_path: None,
            status: crate::storage::MeetingStatus::Recording,
            folder_id: None,
        })
        .unwrap();
        db.set_manual_notes("m-rec", "ship Friday").unwrap();
        let live2 = std::sync::Mutex::new("we agreed to ship friday".to_string());
        let (l2, n2) = gated_live_context(&db, &live2, "m-rec", &unlocked);
        let ctx2 = floor_current_context(&l2, &n2);
        assert!(
            ctx2.contains("Live transcript"),
            "the visible meeting's live tail must be labeled in the floor grounding: {ctx2}"
        );
        assert!(
            ctx2.contains("we agreed to ship friday"),
            "the visible live tail must be present: {ctx2}"
        );
        assert!(
            ctx2.contains("ship Friday"),
            "the visible typed notes must be present: {ctx2}"
        );
    }

    #[test]
    fn gated_live_context_injects_for_visible_meeting_and_masks_without_one() {
        let db = tmp_db("visible");
        // An in-progress recording has no note rows yet ⇒ trivially visible: tail + notes inject.
        db.insert_meeting(&crate::storage::Meeting {
            id: "m-rec".to_string(),
            started_at: "2026-07-01T09:00:00Z".to_string(),
            ended_at: None,
            title: None,
            duration_s: 0,
            audio_path: None,
            status: crate::storage::MeetingStatus::Recording,
            folder_id: None,
        })
        .unwrap();
        db.set_manual_notes("m-rec", "ship Friday").unwrap();
        let live = std::sync::Mutex::new("we agreed to ship friday".to_string());
        let unlocked = std::collections::HashSet::new();
        let (tail, notes) = gated_live_context(&db, &live, "m-rec", &unlocked);
        assert_eq!(
            tail, "we agreed to ship friday",
            "a visible meeting injects the live tail"
        );
        assert_eq!(
            notes, "ship Friday",
            "a visible meeting injects the typed notes"
        );

        // NO current meeting (not recording) ⇒ fail-closed: nothing injects even if a stale
        // buffer somehow survived.
        let (tail, notes) = gated_live_context(&db, &live, "", &unlocked);
        assert!(
            tail.is_empty(),
            "no current meeting must inject NO live tail"
        );
        assert!(
            notes.is_empty(),
            "no current meeting must inject NO typed notes"
        );
    }

    #[test]
    fn gated_live_context_reinjects_after_session_unlock() {
        // A session unlock of the sealed folder makes the meeting visible again → the (unwiped)
        // in-flight buffer injects once more. Proves the gate is reversible, not a wipe.
        let db = tmp_db("reunlock");
        db.insert_folder(&crate::storage::Folder {
            id: "f1".into(),
            name: "Secret".into(),
            path: "Secret".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-01T00:00:00Z".into(),
        })
        .unwrap();
        seed_meeting(&db, "m1", Some("f1"));
        db.set_folder_locked("f1", true, Some(&b"wrapped"[..]))
            .unwrap();

        let live = std::sync::Mutex::new("tail".to_string());
        let mut unlocked = std::collections::HashSet::new();
        assert!(gated_live_context(&db, &live, "m1", &unlocked).0.is_empty());
        unlocked.insert("f1".to_string());
        assert_eq!(gated_live_context(&db, &live, "m1", &unlocked).0, "tail");
    }

    // ── Phase 4: explicit-meeting_id scope resolution (kills the wrong-meeting bug) ────────────────

    #[test]
    fn resolve_scope_meeting_fe_id_wins_over_current_meeting() {
        // The precedence rule: an EXPLICIT FE-sent meeting_id (a bound past/anchored @brain thread)
        // WINS over state.current_meeting (the recording pointer). This is the exact case the
        // wrong-meeting bug lived in — viewing a PAST meeting while a DIFFERENT meeting records must
        // scope to the viewed meeting, not the recording. RED on the pre-phase code, which read ONLY
        // current_meeting and ignored any FE id. (Phase 6: focus is None here.)
        assert_eq!(
            resolve_scope_meeting(Some("past-thread-meeting"), None, Some("recording-meeting")),
            "past-thread-meeting",
            "an explicit FE meeting_id must win over the recording pointer"
        );
    }

    #[test]
    fn resolve_scope_meeting_falls_back_to_recording_when_fe_none() {
        // The voice/wake twin (and a live thread that omits it) sends None → with no focus either,
        // the live recording pointer still scopes, so recording keeps working exactly as before.
        assert_eq!(
            resolve_scope_meeting(None, None, Some("recording-meeting")),
            "recording-meeting",
            "a None FE id + no focus falls back to the current recording meeting"
        );
    }

    #[test]
    fn resolve_scope_meeting_past_thread_when_idle_no_recording() {
        // The headline fix: idle (nothing recording ⇒ current_meeting None, no focus) + a bound
        // past-meeting thread ⇒ scope is THAT meeting, NOT empty. Empty was the whole bug: it fell
        // through to gated_live_context → ("","") → the "ground answers in the vault" branch → an
        // arbitrary saved meeting. With a bound id the scope is concrete.
        assert_eq!(
            resolve_scope_meeting(Some("m-past"), None, None),
            "m-past",
            "an idle bound past-meeting thread scopes to its own meeting, not empty"
        );
    }

    #[test]
    fn resolve_scope_meeting_blank_counts_as_absent_both_sides() {
        // Blank/whitespace at ANY level is ABSENT. A blank FE id falls to focus/recording; a blank
        // FE id + blank focus + blank recording yields "" (no scope — the fail-closed default,
        // which gated_live_context treats as not-visible).
        assert_eq!(
            resolve_scope_meeting(Some("  "), None, Some("m-rec")),
            "m-rec"
        );
        assert_eq!(resolve_scope_meeting(Some(""), None, None), "");
        assert_eq!(resolve_scope_meeting(None, None, Some("   ")), "");
        assert_eq!(resolve_scope_meeting(Some(" m-x "), None, None), "m-x");
    }

    #[test]
    fn resolve_scope_meeting_focus_wins_over_recording_when_fe_none() {
        // PHASE 6: the FOCUS pointer (the meeting the user is VIEWING) wins over the recording
        // pointer when the FE sends no explicit id — the idle wrong-meeting root. Viewing a past
        // meeting while a DIFFERENT meeting records must scope to the VIEWED (focus) meeting, not
        // the recording. RED on the Phase-4 two-arg resolver (no focus arg existed).
        assert_eq!(
            resolve_scope_meeting(None, Some("m-focused-past"), Some("m-recording")),
            "m-focused-past",
            "the focus pointer must win over the recording pointer when the FE sends none"
        );
    }

    #[test]
    fn resolve_scope_meeting_fe_id_wins_over_focus() {
        // PHASE 6: an explicit FE-bound thread id is the TOP of the precedence — it wins even over
        // the focus pointer (a thread durably answers about its OWN bound meeting regardless of what
        // the user happens to be looking at).
        assert_eq!(
            resolve_scope_meeting(Some("m-bound"), Some("m-focused"), Some("m-recording")),
            "m-bound",
            "an explicit FE meeting_id must win over both focus and recording"
        );
    }

    #[test]
    fn resolve_scope_meeting_focus_used_when_idle_no_recording() {
        // PHASE 6: focus makes the cascade Tier-1 scope deterministic even when NOTHING records
        // (current_meeting None) and the FE sent no id — the user is just viewing a meeting.
        assert_eq!(
            resolve_scope_meeting(None, Some("m-focused"), None),
            "m-focused",
            "focus scopes an idle view when there is no FE id and no recording"
        );
    }

    #[test]
    fn resolve_scope_meeting_blank_focus_falls_through() {
        // A blank/whitespace focus is ABSENT and falls through to the recording pointer.
        assert_eq!(
            resolve_scope_meeting(None, Some("   "), Some("m-rec")),
            "m-rec"
        );
        // A blank focus + no recording yields "".
        assert_eq!(resolve_scope_meeting(None, Some("  "), None), "");
        // A blank FE id + a real focus uses the focus.
        assert_eq!(
            resolve_scope_meeting(Some(" "), Some("m-focus"), Some("m-rec")),
            "m-focus"
        );
    }

    #[test]
    fn resolved_past_meeting_scope_grounds_gated_context_in_that_meeting() {
        // End-to-end for the resolution seam: a bound PAST (not-recording) meeting id, resolved with
        // NO recording pointer, drives gated_live_context to read THAT meeting's own gated context
        // (its typed notes) — not empty/arbitrary. Proves the FE id reaches the gate as the scope.
        let db = tmp_db("phase4-scope");
        // A visible past meeting (no folder → trivially visible) with its OWN typed notes.
        db.insert_meeting(&crate::storage::Meeting {
            id: "m-past".to_string(),
            started_at: "2026-07-01T09:00:00Z".to_string(),
            ended_at: Some("2026-07-01T10:00:00Z".to_string()),
            title: Some("Budget review".to_string()),
            duration_s: 3600,
            audio_path: None,
            status: crate::storage::MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.set_manual_notes("m-past", "cut the Q3 travel line")
            .unwrap();

        // Idle: no recording pointer, no focus. The FE binds the thread to m-past.
        let scope = resolve_scope_meeting(Some("m-past"), None, None);
        assert_eq!(scope, "m-past");

        let live = std::sync::Mutex::new(String::new()); // not recording → no live tail
        let unlocked = std::collections::HashSet::new();
        let (_tail, notes) = gated_live_context(&db, &live, &scope, &unlocked);
        assert_eq!(
            notes, "cut the Q3 travel line",
            "the resolved past-meeting scope must read THAT meeting's gated context, not empty/arbitrary"
        );

        // Contrast: the pre-phase behavior (ignore FE id + focus, use only the empty recording
        // pointer) would have scoped to "" → no context read → the vault-wide-arbitrary fallback.
        let empty_scope = resolve_scope_meeting(None, None, None);
        assert_eq!(empty_scope, "");
        let (_t, n) = gated_live_context(&db, &live, &empty_scope, &unlocked);
        assert!(
            n.is_empty(),
            "the pre-phase empty scope reads NO meeting context (the fallback that caused the bug)"
        );
    }

    #[test]
    fn resolved_scope_still_masks_a_sealed_not_visible_bound_meeting() {
        // The visibility gate must still hold for the RESOLVED scope: binding a thread to a
        // sealed-and-not-session-unlocked meeting must inject NOTHING (fail-closed) — Phase 4 threads
        // the id but never bypasses meeting_is_visible.
        let db = tmp_db("phase4-sealed");
        db.insert_folder(&crate::storage::Folder {
            id: "f1".into(),
            name: "Secret".into(),
            path: "Secret".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-01T00:00:00Z".into(),
        })
        .unwrap();
        seed_meeting(&db, "m1", Some("f1"));
        db.set_manual_notes("m1", "secret budget cuts").unwrap();
        db.set_folder_locked("f1", true, Some(&b"wrapped"[..]))
            .unwrap();

        // The FE binds the thread to the sealed meeting; the scope resolves to it — but the gate masks.
        let scope = resolve_scope_meeting(Some("m1"), None, None);
        assert_eq!(scope, "m1");
        let live = std::sync::Mutex::new("sealed tail in RAM".to_string());
        let unlocked = std::collections::HashSet::new();
        let (tail, notes) = gated_live_context(&db, &live, &scope, &unlocked);
        assert!(
            tail.is_empty() && notes.is_empty(),
            "a bound-but-sealed meeting must inject nothing (the visibility gate still holds)"
        );

        // A session unlock re-exposes it (the gate is reversible, not a wipe).
        let mut unlocked = std::collections::HashSet::new();
        unlocked.insert("f1".to_string());
        let (_t, notes) = gated_live_context(&db, &live, &scope, &unlocked);
        assert_eq!(
            notes, "secret budget cuts",
            "a session-unlocked bound meeting injects its own context"
        );
    }

    #[test]
    fn focused_past_meeting_scopes_to_itself_even_while_a_different_meeting_records() {
        // PHASE 6 — the exact idle/cross-meeting wrong-meeting root: the FE sends NO explicit id
        // (the voice/wake fallback path), a DIFFERENT meeting is recording, and the user is viewing
        // (focus) a PAST meeting. The scope must resolve to the FOCUSED past meeting and read ITS
        // gated context — NOT the recording, NOT an arbitrary vault meeting.
        let db = tmp_db("phase6-focus-scope");
        // A past, saved meeting the user is looking at, with its own typed notes.
        db.insert_meeting(&crate::storage::Meeting {
            id: "m-focused-past".to_string(),
            started_at: "2026-07-01T09:00:00Z".to_string(),
            ended_at: Some("2026-07-01T10:00:00Z".to_string()),
            title: Some("Roadmap review".to_string()),
            duration_s: 3600,
            audio_path: None,
            status: crate::storage::MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.set_manual_notes("m-focused-past", "ship the connector in Q3")
            .unwrap();

        // Resolution: no FE id, focus = the past meeting, recording = a DIFFERENT live meeting.
        let scope = resolve_scope_meeting(None, Some("m-focused-past"), Some("m-recording-now"));
        assert_eq!(
            scope, "m-focused-past",
            "focus must win over the recording pointer for the fallback path"
        );

        let live = std::sync::Mutex::new("live tail of the OTHER meeting".to_string());
        let unlocked = std::collections::HashSet::new();
        let (_tail, notes) = gated_live_context(&db, &live, &scope, &unlocked);
        assert_eq!(
            notes, "ship the connector in Q3",
            "the focused past meeting grounds context in ITSELF, not the recording"
        );
    }

    #[test]
    fn relock_re_masks_the_focused_meeting_content() {
        // PHASE 6 clear-on-relock discipline: focus is only an ID (no content), so a relock does not
        // need to clear the focus pointer — but any CONTENT read against the focused meeting MUST
        // re-mask when its folder relocks (the same visibility gate live_transcript rides). This
        // proves the focused meeting's content is masked after relock and reversible on re-unlock.
        let db = tmp_db("phase6-relock-mask");
        db.insert_folder(&crate::storage::Folder {
            id: "f-focus".into(),
            name: "Secret".into(),
            path: "Secret".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-01T00:00:00Z".into(),
        })
        .unwrap();
        seed_meeting(&db, "m-focus", Some("f-focus"));
        db.set_manual_notes("m-focus", "secret roadmap").unwrap();

        // The user is viewing (focus) this meeting; the folder is session-UNLOCKED → content shows.
        let scope = resolve_scope_meeting(None, Some("m-focus"), None);
        assert_eq!(scope, "m-focus");
        let live = std::sync::Mutex::new(String::new());
        let mut unlocked = std::collections::HashSet::new();
        unlocked.insert("f-focus".to_string());
        // Seal the folder (locked=1) but keep it in the session-unlocked set → visible.
        db.set_folder_locked("f-focus", true, Some(&b"wrapped"[..]))
            .unwrap();
        let (_t, notes) = gated_live_context(&db, &live, &scope, &unlocked);
        assert_eq!(
            notes, "secret roadmap",
            "a session-unlocked focused meeting shows its content"
        );

        // RELOCK: the folder leaves the session-unlocked set (as relock_all_inner/relock_folder do).
        // The focus id may stay — but the CONTENT read must now re-mask (fail-closed).
        unlocked.remove("f-focus");
        let (tail, notes) = gated_live_context(&db, &live, &scope, &unlocked);
        assert!(
            tail.is_empty() && notes.is_empty(),
            "after relock, the focused meeting's content must be re-masked"
        );

        // Reversible: a fresh session unlock re-exposes it.
        unlocked.insert("f-focus".to_string());
        let (_t, notes) = gated_live_context(&db, &live, &scope, &unlocked);
        assert_eq!(
            notes, "secret roadmap",
            "unlock re-exposes the focused meeting's content (the gate is reversible)"
        );
    }

    // ── PR-A #1/#3: clearing the buffer at Stop + lock-surface hygiene ─────────────────────────────

    #[test]
    fn clear_live_transcript_empties_the_buffer() {
        let live = std::sync::Mutex::new("stale tail of the finished recording".to_string());
        clear_live_transcript(&live);
        assert!(
            live.lock().unwrap().is_empty(),
            "the buffer must be empty after Stop clears it"
        );
        // Idempotent on an already-empty buffer.
        clear_live_transcript(&live);
        assert!(live.lock().unwrap().is_empty());
    }

    #[test]
    fn clear_live_transcript_if_idle_never_wipes_a_recording_in_flight() {
        // Mid-recording the lock surface must NOT wipe the user's in-flight buffer (egress
        // correctness there is the visibility gate's job); idle it clears.
        let live = std::sync::Mutex::new("in-flight captions".to_string());
        clear_live_transcript_if_idle(&live, true);
        assert_eq!(
            *live.lock().unwrap(),
            "in-flight captions",
            "mid-recording buffer untouched"
        );
        clear_live_transcript_if_idle(&live, false);
        assert!(live.lock().unwrap().is_empty(), "idle buffer cleared");
    }

    #[test]
    fn assistant_system_prompt_injects_transcript_only_when_present() {
        // No live transcript (not recording / no captions yet) ⇒ the base prompt, no transcript section.
        let base = assistant_system_prompt("", "", "", false);
        assert!(
            !base.contains("LIVE TRANSCRIPT"),
            "no transcript section when empty: {base}"
        );
        assert!(base.contains("in-meeting assistant"));
        // With a transcript ⇒ it is embedded + the brain is told to use it for the current meeting.
        let with = assistant_system_prompt(
            "we shipped the beta and assigned the deck to Anna",
            "",
            "",
            false,
        );
        assert!(
            with.contains("LIVE TRANSCRIPT"),
            "names the transcript section: {with}"
        );
        assert!(
            with.contains("assigned the deck to Anna"),
            "embeds the transcript text"
        );
        assert!(
            with.to_lowercase().contains("current"),
            "tells the brain it's the current meeting"
        );
    }

    /// The system prompt instructs the model to DECIDE answer-vs-propose: name the `propose_note` tool
    /// and tell it to call it ONLY when the user asks for a note (the model decides; no regex in code).
    /// Present in BOTH the no-transcript and with-transcript branches (it lives in the shared base).
    #[test]
    fn assistant_system_prompt_instructs_propose_note_decision() {
        for prompt in [
            assistant_system_prompt("", "", "", false),
            assistant_system_prompt("we shipped the beta", "", "", false),
        ] {
            assert!(
                prompt.contains("propose_note"),
                "names the propose_note tool: {prompt}"
            );
            assert!(
                prompt.to_lowercase().contains("note"),
                "tells the model when to draft a note: {prompt}"
            );
            // Both an answer path and a note-draft path are described (decide between them).
            assert!(
                prompt.to_lowercase().contains("answer"),
                "describes plain answering too: {prompt}"
            );
        }
    }

    #[test]
    fn assistant_system_prompt_truncates_to_recent_tail() {
        // A very long transcript is truncated to the RECENT tail (so a 2-hour meeting can't blow the
        // context) and marked elided.
        let long = "x ".repeat(LIVE_TRANSCRIPT_INJECT_CHARS); // way over the inject budget
        let p = assistant_system_prompt(&long, "", "", false);
        assert!(p.contains('…'), "elision marker present when truncated");
        assert!(
            p.chars().count() < long.chars().count(),
            "shorter than the raw transcript"
        );
    }

    /// brain2 realtime notes: typed notes are injected as their own section when present, and the
    /// EMPTY-typed-notes prompt is BYTE-IDENTICAL to the pre-feature output (no regression).
    #[test]
    fn assistant_system_prompt_injects_typed_notes_and_empty_is_byte_identical() {
        // Empty typed notes ⇒ no typed-notes section, AND byte-identical to the no-notes prompt for
        // BOTH the no-transcript and with-transcript branches.
        let no_tx = assistant_system_prompt("", "", "", false);
        assert!(
            !no_tx.contains("TYPED NOTES"),
            "no typed-notes section when empty: {no_tx}"
        );
        let tx = "we shipped the beta and assigned the deck to Anna";
        assert_eq!(
            assistant_system_prompt(tx, "", "", false),
            assistant_system_prompt(tx, "   ", "", false),
            "whitespace-only typed notes must be byte-identical to none (no regression)"
        );

        // Present typed notes ⇒ embedded under the labeled section, alongside the transcript.
        let with = assistant_system_prompt(
            tx,
            "DECISION: ship Friday. Anna owns QA sign-off.",
            "",
            false,
        );
        assert!(
            with.contains("TYPED NOTES"),
            "names the typed-notes section: {with}"
        );
        assert!(
            with.contains("Anna owns QA sign-off"),
            "embeds the typed-notes text"
        );
        assert!(
            with.contains("LIVE TRANSCRIPT"),
            "still injects the transcript too"
        );

        // Typed notes inject even with NO transcript (the user can type before any caption lands).
        let notes_only =
            assistant_system_prompt("", "remember: budget cap is the blocker", "", false);
        assert!(
            notes_only.contains("TYPED NOTES"),
            "typed notes inject without a transcript"
        );
        assert!(notes_only.contains("budget cap is the blocker"));
        assert!(
            !notes_only.contains("LIVE TRANSCRIPT"),
            "no transcript section when transcript empty"
        );

        // A very long typed-notes buffer is truncated to the recent tail (bounded like the transcript).
        let long = "y ".repeat(LIVE_TRANSCRIPT_INJECT_CHARS);
        let p = assistant_system_prompt("", &long, "", false);
        assert!(
            p.contains('…'),
            "elision marker present when typed notes truncated"
        );
    }

    // ── Phase 3 CROSS-MEETING USER MEMORY: brief injection + the seal invariant ───────────────────

    /// The memory brief is injected as its own labeled section when present, and the EMPTY-brief
    /// prompt is BYTE-IDENTICAL to the no-brief output (no regression). This is the "included when
    /// present, empty when memory is empty" contract from the task.
    #[test]
    fn assistant_system_prompt_injects_memory_brief_and_empty_is_byte_identical() {
        // Empty brief ⇒ no memory section, AND byte-identical to the no-brief prompt for BOTH the
        // no-transcript and with-transcript branches.
        let no_brief = assistant_system_prompt("", "", "", false);
        assert!(
            !no_brief.to_uppercase().contains("KNOW ABOUT THE USER"),
            "no memory section when empty"
        );
        assert_eq!(
            assistant_system_prompt("we shipped the beta", "", "", false),
            assistant_system_prompt("we shipped the beta", "", "   ", false),
            "whitespace-only brief must be byte-identical to none (no regression)"
        );

        // Present brief ⇒ embedded under the labeled memory section, alongside transcript + notes.
        let brief = "- You prefer: Polish replies\n- You work on: Project Atlas";
        let with = assistant_system_prompt("we shipped the beta", "ship Friday", brief, false);
        assert!(
            with.to_uppercase().contains("KNOW ABOUT THE USER"),
            "names the memory section: {with}"
        );
        assert!(with.contains("Project Atlas"), "embeds the brief text");
        assert!(
            with.contains("LIVE TRANSCRIPT"),
            "still injects the transcript too"
        );
        assert!(
            with.contains("TYPED NOTES"),
            "still injects the typed notes too"
        );

        // The brief injects even with NO transcript and NO notes.
        let brief_only = assistant_system_prompt("", "", brief, false);
        assert!(
            brief_only.to_uppercase().contains("KNOW ABOUT THE USER"),
            "brief injects standalone"
        );
        assert!(brief_only.contains("Polish replies"));
    }

    /// THE WHOLE-FEATURE SEAL INVARIANT (RED-before-GREEN, design spec D3 / task C5.i): a user-memory
    /// fact whose SOURCE meeting is sealed-and-not-session-unlocked is NEITHER returned by
    /// `list_user_facts_visible` NOR present in the injected brief. Before the visibility gate on
    /// `list_user_facts_visible` this test FAILS (the sealed fact leaks into the brief); after the
    /// gate it passes. Session re-unlock re-admits it (the gate is reversible, not a wipe).
    #[test]
    fn user_memory_brief_excludes_sealed_source_and_reinjects_after_unlock() {
        let db = tmp_db("user-mem-seal");
        db.insert_folder(&crate::storage::Folder {
            id: "f1".into(),
            name: "Secret".into(),
            path: "Secret".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-01T00:00:00Z".into(),
        })
        .unwrap();
        // A summarized, foldered meeting is the SOURCE of a user fact.
        seed_meeting(&db, "m1", Some("f1"));
        db.apply_user_fact_ops(&[crate::facts::FactOp::Add(crate::facts::NewFact {
            entity_id: crate::user_memory::USER_SCOPE.to_string(),
            subject: "You".into(),
            predicate: "prefer".into(),
            object: "Polish replies".into(),
            valid_from: "2026-07-01T00:00:00Z".into(),
            recorded_at: "2026-07-01T00:00:00Z".into(),
            confidence: 1.0,
            meeting_id: Some("m1".into()),
        })])
        .unwrap();

        // While the folder is OPEN the fact is visible → the brief contains it.
        let mut unlocked = std::collections::HashSet::new();
        let brief_open = gated_user_memory_brief(&db, &unlocked, true, "");
        assert!(
            brief_open.contains("Polish replies"),
            "an open-folder user fact must be in the brief"
        );
        let prompt_open = assistant_system_prompt("", "", &brief_open, false);
        assert!(
            prompt_open.contains("Polish replies"),
            "and injected into the prompt"
        );

        // SEAL the folder (session NOT unlocked). NOTE: `set_folder_locked` only flips the flag — the
        // real seal path PURGES the fact (`purge_user_facts_tx`); here we assert the READ GATE alone,
        // so we don't purge, proving the gate hides a fact whose row still exists at rest.
        db.set_folder_locked("f1", true, Some(&b"wrapped"[..]))
            .unwrap();
        let visible_sealed = db.list_user_facts_visible(&unlocked).unwrap();
        assert!(
            visible_sealed.is_empty(),
            "a sealed-not-unlocked meeting's user fact must be INVISIBLE to the gated reader"
        );
        let brief_sealed = gated_user_memory_brief(&db, &unlocked, true, "");
        assert!(
            brief_sealed.is_empty(),
            "sealed-source fact must NOT appear in the injected brief"
        );
        let prompt_sealed = assistant_system_prompt("", "", &brief_sealed, false);
        assert!(
            !prompt_sealed.contains("Polish replies"),
            "the sealed fact must not reach the prompt"
        );
        assert!(
            !prompt_sealed.to_uppercase().contains("KNOW ABOUT THE USER"),
            "no memory section at all"
        );

        // A SESSION UNLOCK re-admits it (reversible gate).
        unlocked.insert("f1".to_string());
        assert!(
            gated_user_memory_brief(&db, &unlocked, true, "").contains("Polish replies"),
            "a session unlock must re-inject the user fact"
        );

        // FLAG OFF: even a VISIBLE, open-folder fact must NOT be injected when memory is disabled —
        // the brief is empty regardless of visibility, so the @brain loop injects nothing.
        assert!(
            gated_user_memory_brief(&db, &unlocked, false, "").is_empty(),
            "memory disabled must suppress the brief even for a visible fact"
        );
    }

    // ── Phase 5: the cascade tier suffixes + badge determinism + Tier-1 citation gate ───────────────

    /// The tier prompt suffixes carry the EXACT escalation sentinel (must match
    /// `crate::agent::ESCALATE_SENTINEL`) at Tiers 1/2, and the TERMINAL Tier 3 must NOT instruct the
    /// sentinel (there is no higher tier to escalate to). Prompt-drift guard.
    #[test]
    fn tier_suffixes_carry_the_sentinel_except_terminal_tier() {
        assert!(
            TIER1_SUFFIX.contains(crate::agent::ESCALATE_SENTINEL),
            "Tier 1 must instruct the escalation sentinel"
        );
        assert!(
            TIER2_SUFFIX.contains(crate::agent::ESCALATE_SENTINEL),
            "Tier 2 must instruct the escalation sentinel"
        );
        assert!(
            !TIER3_SUFFIX.contains(crate::agent::ESCALATE_SENTINEL),
            "the TERMINAL Tier 3 must NEVER instruct the escalation sentinel"
        );
        // Tier 1's scope contract really is "this meeting only".
        assert!(
            TIER1_SUFFIX.contains("CURRENT MEETING ONLY"),
            "Tier 1 must scope to the current meeting only"
        );
    }

    /// DETERMINISTIC BADGE: `from_agent` stamps the tier the ladder passes — never string-sniffed —
    /// and Tier 1 PREPENDS its `extra_citations` (the current meeting's own [[Title]]) ahead of the
    /// loop's gated citations, de-duplicated in first-seen order.
    #[test]
    fn from_agent_sets_the_tier_badge_and_prepends_extra_citations() {
        let outcome = crate::agent::AgentOutcome {
            answer: "We decided to ship Friday.".to_string(),
            steps: Vec::new(),
            citations: vec!["[[Other Meeting]]".to_string()],
        };
        let res = crate::voice_action::VoiceActionResult::from_agent(
            &VoiceIntent::Research {
                topic: "ship".into(),
            },
            outcome,
            crate::voice_action::AnsweredFrom::CurrentMeeting,
            vec!["[[This Meeting]]".to_string()],
        );
        assert_eq!(
            res.answered_from,
            Some(crate::voice_action::AnsweredFrom::CurrentMeeting),
            "the badge is the tier the ladder passed, deterministically"
        );
        assert_eq!(
            res.citations,
            vec![
                "[[This Meeting]]".to_string(),
                "[[Other Meeting]]".to_string()
            ],
            "Tier 1's own [[Title]] is prepended ahead of the loop's gated citations"
        );
    }

    /// TIER-1 CITATION GATE: for a VISIBLE current meeting, Tier 1 resolves its own [[Title]]; for a
    /// SEALED-not-unlocked meeting it resolves NOTHING (fail-closed — no title leaks behind a lock).
    #[test]
    fn tier_extra_citations_gates_the_current_meeting_title() {
        use crate::voice_action::AnsweredFrom;
        let db = tmp_db("tier1-cite");
        db.insert_folder(&crate::storage::Folder {
            id: "f1".into(),
            name: "Secret".into(),
            path: "Secret".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-01T00:00:00Z".into(),
        })
        .unwrap();
        seed_meeting(&db, "m1", Some("f1")); // title "Sync", foldered into f1

        // OPEN folder → visible → Tier 1 cites its own [[Sync]].
        let open = std::collections::HashSet::new();
        assert_eq!(
            tier_extra_citations(&db, "m1", AnsweredFrom::CurrentMeeting, &open),
            vec!["[[Sync]]".to_string()],
            "a visible current meeting contributes its own [[Title]] citation"
        );

        // A non-Tier-1 badge adds nothing here (Tier 2/3 use the loop's own gated citations).
        assert!(
            tier_extra_citations(&db, "m1", AnsweredFrom::Vault, &open).is_empty(),
            "Tier 2 adds no extra citation from here"
        );
        assert!(
            tier_extra_citations(&db, "m1", AnsweredFrom::Connectors, &open).is_empty(),
            "Tier 3 adds no extra citation from here"
        );

        // SEAL the folder + nothing unlocked → invisible → NO title citation (fail-closed).
        db.set_folder_locked("f1", true, Some(&b"wrapped"[..]))
            .unwrap();
        let sealed = std::collections::HashSet::new();
        assert!(
            !db.meeting_is_visible("m1", &sealed).unwrap(),
            "seed self-check: the sealed meeting is invisible"
        );
        assert!(
            tier_extra_citations(&db, "m1", AnsweredFrom::CurrentMeeting, &sealed).is_empty(),
            "a sealed-not-unlocked current meeting must contribute NO [[Title]] (no leak behind a lock)"
        );

        // An empty meeting_id (idle, no scope) contributes nothing.
        assert!(
            tier_extra_citations(&db, "", AnsweredFrom::CurrentMeeting, &open).is_empty(),
            "no scope meeting ⇒ no citation"
        );
    }

    // ── Tier-1 current-meeting reader: live + PAST, both gated (2026-07-09 structural fix) ─────────

    fn seg(idx: i64, start_s: f64, text: &str) -> crate::transcribe::types::Segment {
        crate::transcribe::types::Segment {
            idx,
            start_s,
            end_s: start_s + 1.0,
            text: text.to_string(),
            speaker: None,
            confidence: None,
        }
    }

    /// RED-TODAY GAP: a VIEWED PAST (not-recording) meeting has an EMPTY live buffer, so the old
    /// `floor_current_context` yielded "" → the floor wrongly fanned out. `tier1_current_content`
    /// ALSO reads the past meeting's OWN gated transcript + note, so a viewed meeting is answerable
    /// from itself. (On the pre-fix code this content path did not exist.)
    #[test]
    fn tier1_current_content_reads_a_viewed_past_meetings_gated_transcript_and_note() {
        let db = tmp_db("tier1-past");
        // A visible past meeting (no folder → trivially visible) with its OWN transcript + note.
        db.insert_meeting(&crate::storage::Meeting {
            id: "m-past".to_string(),
            started_at: "2026-07-01T09:00:00Z".to_string(),
            ended_at: Some("2026-07-01T10:00:00Z".to_string()),
            title: Some("Roadmap Sync".to_string()),
            duration_s: 3600,
            audio_path: None,
            status: crate::storage::MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.insert_segments(
            "m-past",
            &[
                seg(0, 0.0, "We decided to cut the Q3 travel budget."),
                seg(1, 5.0, "Alice will own the migration."),
            ],
        )
        .unwrap();
        db.upsert_note(&crate::storage::NoteRecord {
            meeting_id: "m-past".to_string(),
            provider_id: "claude_code".to_string(),
            markdown: "## Decisions\n- Cut Q3 travel\n- Alice owns migration".to_string(),
            created_at: "2026-07-01T10:05:00Z".to_string(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();

        // NOT recording → empty live buffer. The reader must still surface the past meeting's content.
        let live = std::sync::Mutex::new(String::new());
        let unlocked = std::collections::HashSet::new();

        // RED-documenting the gap: the OLD floor reader (gated_live_context → floor_current_context)
        // reads ONLY the live buffer + manual notes, so for a viewed past meeting it is EMPTY — which
        // is exactly why the floor wrongly fanned out. `tier1_current_content` closes that gap.
        let (old_live, old_typed) = gated_live_context(&db, &live, "m-past", &unlocked);
        assert!(
            floor_current_context(&old_live, &old_typed).is_empty(),
            "the OLD reader was empty for a viewed past meeting (the fan-out cause this fix closes)"
        );

        let ctx = tier1_current_content(&db, &live, "m-past", &unlocked);
        assert!(
            ctx.contains("cut the Q3 travel budget"),
            "the viewed past meeting's OWN transcript must be read (RED today): {ctx}"
        );
        assert!(
            ctx.contains("Alice owns migration"),
            "the viewed past meeting's OWN note must be read: {ctx}"
        );
        // The title is resolvable (gated) for the [[Title]] citation.
        assert_eq!(
            current_meeting_title(&db, "m-past", &unlocked),
            Some("Roadmap Sync".to_string())
        );
    }

    /// GATE (fail-closed): a SEALED-not-unlocked viewed past meeting yields NOTHING from the Tier-1
    /// reader — its transcript/note are never read, its title never resolved. No leak behind a lock.
    #[test]
    fn tier1_current_content_yields_nothing_for_a_sealed_not_unlocked_meeting() {
        let db = tmp_db("tier1-sealed");
        db.insert_folder(&crate::storage::Folder {
            id: "f1".into(),
            name: "Secret".into(),
            path: "Secret".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-01T00:00:00Z".into(),
        })
        .unwrap();
        db.insert_meeting(&crate::storage::Meeting {
            id: "m-sealed".to_string(),
            started_at: "2026-07-01T09:00:00Z".to_string(),
            ended_at: Some("2026-07-01T10:00:00Z".to_string()),
            title: Some("Confidential Terms".to_string()),
            duration_s: 3600,
            audio_path: None,
            status: crate::storage::MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.insert_segments(
            "m-sealed",
            &[seg(0, 0.0, "SECRET acquisition price is 40M.")],
        )
        .unwrap();
        db.upsert_note(&crate::storage::NoteRecord {
            meeting_id: "m-sealed".to_string(),
            provider_id: "claude_code".to_string(),
            markdown: "SECRET terms".to_string(),
            created_at: "2026-07-01T10:05:00Z".to_string(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_meeting_folder("m-sealed", Some("f1")).unwrap();
        db.set_note_folder("m-sealed", Some("f1")).unwrap();
        // Seal the folder → sealed-not-session-unlocked.
        db.set_folder_locked("f1", true, Some(&b"wrapped"[..]))
            .unwrap();

        let live = std::sync::Mutex::new(String::new());
        let sealed = std::collections::HashSet::new(); // folder NOT in the session unlock set
                                                       // Self-check: the meeting is invisible to the sealed session.
        assert!(!db.meeting_is_visible("m-sealed", &sealed).unwrap());

        let ctx = tier1_current_content(&db, &live, "m-sealed", &sealed);
        assert!(
            ctx.is_empty(),
            "a sealed-not-unlocked viewed meeting must yield NO Tier-1 content: {ctx}"
        );
        assert!(
            !ctx.contains("SECRET") && !ctx.contains("40M"),
            "sealed content must NEVER leak into the Tier-1 reader"
        );
        // The title must not resolve either (no title leak behind a lock).
        assert_eq!(
            current_meeting_title(&db, "m-sealed", &sealed),
            None,
            "no [[Title]] for a sealed-not-unlocked meeting"
        );

        // A SESSION UNLOCK makes it readable again (reversible, not a wipe).
        let mut unlocked = std::collections::HashSet::new();
        unlocked.insert("f1".to_string());
        let ctx2 = tier1_current_content(&db, &live, "m-sealed", &unlocked);
        assert!(
            ctx2.contains("SECRET acquisition price") || ctx2.contains("SECRET terms"),
            "after session unlock the Tier-1 reader surfaces the content again: {ctx2}"
        );
    }

    /// The LIVE buffer still wins for an in-progress recording (unchanged): `tier1_current_content`
    /// returns the live block without touching the past-meeting read path.
    #[test]
    fn tier1_current_content_prefers_the_live_buffer_for_a_recording() {
        let db = tmp_db("tier1-live");
        db.insert_meeting(&crate::storage::Meeting {
            id: "m-rec".to_string(),
            started_at: "2026-07-01T09:00:00Z".to_string(),
            ended_at: None,
            title: None,
            duration_s: 0,
            audio_path: None,
            status: crate::storage::MeetingStatus::Recording,
            folder_id: None,
        })
        .unwrap();
        db.set_manual_notes("m-rec", "ship Friday").unwrap();
        let live = std::sync::Mutex::new("we agreed to ship friday".to_string());
        let unlocked = std::collections::HashSet::new();
        let ctx = tier1_current_content(&db, &live, "m-rec", &unlocked);
        assert!(
            ctx.contains("Live transcript") && ctx.contains("we agreed to ship friday"),
            "the live buffer is the Tier-1 source for a recording: {ctx}"
        );
        assert!(ctx.contains("ship Friday"), "typed notes come along: {ctx}");
    }

    /// Brain v2 L3 — the escalation-ledger GUARD, bound (adversarial finding 2026-07-10: removing
    /// the reasoner-only guard previously survived the whole suite). A reasoner-only Live target
    /// (local/AFM/off — a local→local escalation is NOT egress) yields `None`: NO ledger row. A
    /// provider target yields exactly the content-free `escalation_entry` row.
    #[test]
    fn escalation_ledger_guard_skips_reasoner_only_targets() {
        // The guard the mutation probe killed silently: reasoner-only ⇒ None, whatever the tiers.
        assert!(should_ledger_escalation(true, "local", 1, 2).is_none());
        assert!(should_ledger_escalation(true, "claude_code", 2, 3).is_none());

        // Provider-served next tier ⇒ the content-free row (connection + tier transition only).
        let e = should_ledger_escalation(false, "claude_code", 1, 2)
            .expect("a provider target must ledger the escalation");
        assert_eq!(e.call_kind, "escalation");
        assert_eq!(e.provider_id, "claude_code");
        assert_eq!(e.destination, "cascade tier1→tier2");
        assert_eq!(e.model_requested, "");
        assert_eq!(e.system_bytes, 0);
        assert_eq!(e.user_bytes, 0);
        assert!(
            e.meeting_id.is_none(),
            "content-free: no meeting id, no query text"
        );
    }
}
