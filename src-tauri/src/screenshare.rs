//! Screen-share / screen-capture watcher → auto-relock (Stage E).
//!
//! When the screen STARTS being captured or shared (a screen recording, a Zoom/Meet "share
//! screen", a QuickTime capture, etc.) any session-unlocked sealed folders are a privacy leak —
//! their plaintext markdown is live in the DB and on screen. This watcher detects the rising edge
//! (not-captured → captured) of `NSScreen.isCaptured` and calls `relock_all_inner`, which clears
//! the session unlock set and zeroizes the cached KEK, plus emits a Tauri event so the UI can
//! toast.
//!
//! IMPLEMENTATION: a ~1.5s `tokio::interval` poll of `NSScreen.isCaptured` across all screens on a
//! background task. The model wants "fire on START", which the rising-edge detection over the poll
//! gives us (we only act on false→true, never re-fire while it stays captured). We deliberately do
//! NOT use the deprecated `CGDisplayIsCaptured()` — it is semantically wrong (exclusive-render,
//! not screen-share) and deprecated.
//!
//! HONEST DENT (documented): full-screen / whole-display share flips `isCaptured`; a single-window
//! WebRTC share (e.g. Meet sharing one Chrome tab) may NOT flip whole-screen `isCaptured`. Relock
//! is best-effort hiding — it cannot recall what is already on screen.
//!
//! GRACEFUL DEGRADATION: on a non-macOS host or if AppKit can't be reached, the poll simply
//! reports "not captured" forever and never fires — it never panics.

use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};

use crate::state::AppState;

/// Event emitted to all windows when screen capture/sharing STARTS (rising edge). The UI listens
/// and toasts "Screen sharing detected — locked notes were re-secured."
pub const EVENT_SCREEN_SHARE_STARTED: &str = "murmur://screen-share-started";

/// Poll cadence for the capture-state check. ~1.5s is responsive enough to re-secure quickly while
/// being negligible CPU.
const POLL_INTERVAL: Duration = Duration::from_millis(1500);

/// Spawn the screen-share watcher on the Tauri async runtime. Best-effort: if the config flag is
/// off, or if the platform can't report capture state, this is a no-op loop.
///
/// Gated by `K_RELOCK_ON_SCREENSHARE` (default true) read once at spawn time.
pub fn spawn(app: AppHandle) {
    // Read the flag once at startup; mirrors how the MCP token flag is read in lib::setup.
    let enabled = {
        let state = app.state::<AppState>();
        let cfg = state.config.lock();
        cfg.map(|c| c.relock_on_screenshare).unwrap_or(true)
    };
    if !enabled {
        tracing::info!(
            target: "screenshare",
            "screen-share auto-relock disabled by config (relock_on_screenshare=false)"
        );
        return;
    }

    // Poll on a DEDICATED OS THREAD (not the async runtime): the NSScreen read MUST be marshaled
    // to the main thread — AppKit is main-thread-affine, and calling it off-main throws an
    // Objective-C exception that aborts the whole process ("Rust cannot catch foreign exceptions").
    // A std thread can safely block on the main-thread reply between polls.
    std::thread::Builder::new()
        .name("murmur-screenshare".into())
        .spawn(move || watch(app))
        .ok();
}

/// The poll loop (dedicated OS thread). Holds the rising-edge state and acts only on false→true.
fn watch(app: AppHandle) {
    let mut was_captured = captured_on_main(&app);
    tracing::info!(
        target: "screenshare",
        initial_captured = was_captured,
        "screen-share watcher started"
    );

    loop {
        std::thread::sleep(POLL_INTERVAL);
        let now_captured = captured_on_main(&app);

        // Rising edge only: fire on START (false → true). We do not re-fire while it stays true,
        // and we silently reset on the falling edge.
        if now_captured && !was_captured {
            tracing::warn!(
                target: "screenshare",
                "screen capture/sharing started — relocking all session-unlocked folders"
            );
            on_capture_started(&app);
        }
        was_captured = now_captured;
    }
}

/// Read `is_any_screen_captured()` on the MAIN THREAD (AppKit requirement) via Tauri's main-thread
/// dispatch, returning the result over a channel. Returns false if the app is shutting down or the
/// main thread does not reply promptly (never blocks forever, never throws across the boundary).
fn captured_on_main(app: &AppHandle) -> bool {
    let (tx, rx) = std::sync::mpsc::channel();
    if app
        .run_on_main_thread(move || {
            let _ = tx.send(is_any_screen_captured());
        })
        .is_err()
    {
        return false; // app is tearing down
    }
    rx.recv_timeout(Duration::from_secs(2)).unwrap_or(false)
}

/// React to a capture-start rising edge: relock everything + emit the UI event.
fn on_capture_started(app: &AppHandle) {
    let state = app.state::<AppState>();
    // relock_all_inner takes &AppState; State derefs to it.
    if let Err(e) = crate::commands::relock_all_inner(&state) {
        tracing::error!(
            target: "screenshare",
            error = %e,
            "relock_all_inner failed during screen-share auto-relock"
        );
    }
    // Emit regardless so the UI can surface the privacy event even if there was nothing to relock.
    if let Err(e) = app.emit(EVENT_SCREEN_SHARE_STARTED, ()) {
        tracing::warn!(
            target: "screenshare",
            error = %e,
            "failed to emit screen-share-started event"
        );
    }
}

/// Capture-state probe. Returns `false` for now (detection inactive — see below).
///
/// DISABLED PENDING A CORRECT API: the earlier attempt sent `-isCaptured` to `NSScreen`, but that
/// is NOT a valid `NSScreen` selector — `msg_send![screen, isCaptured]` raises an "unrecognized
/// selector" `NSException`, which crosses the FFI boundary as a foreign exception and ABORTS the
/// whole process ("Rust cannot catch foreign exceptions") right after launch. A correct macOS
/// screen-capture/share detection (e.g. ScreenCaptureKit `SCShareableContent`, or observing the
/// system capture indicator) needs to be wired AND verified on a signed build before re-enabling.
/// Until then this returns `false` so the watcher never fires and never crashes. Everything else
/// in the lock model — encryption, per-folder seal, MCP-hiding, and MANUAL relock — is unaffected;
/// only the *automatic* relock-on-screen-share trigger is inactive.
#[cfg(target_os = "macos")]
fn is_any_screen_captured() -> bool {
    false
}

/// Non-macOS fallback: no capture signal available → always reports "not captured" (never fires).
#[cfg(not(target_os = "macos"))]
fn is_any_screen_captured() -> bool {
    false
}
