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

    tauri::async_runtime::spawn(async move {
        watch(app).await;
    });
}

/// The poll loop. Holds the rising-edge state (`was_captured`) and acts only on false→true.
async fn watch(app: AppHandle) {
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    // If a tick is missed (e.g. the runtime was busy) skip rather than burst-catch-up.
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let mut was_captured = is_any_screen_captured();
    tracing::info!(
        target: "screenshare",
        initial_captured = was_captured,
        "screen-share watcher started"
    );

    loop {
        ticker.tick().await;
        let now_captured = is_any_screen_captured();

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

/// True if ANY attached screen reports `isCaptured`. macOS implementation via objc2/AppKit.
#[cfg(target_os = "macos")]
fn is_any_screen_captured() -> bool {
    use objc2::rc::Retained;
    use objc2::{class, msg_send};
    use objc2_app_kit::NSScreen;
    use objc2_foundation::NSArray;

    // `+[NSScreen screens]` is documented as main-thread-affine, but the generated binding demands
    // a `MainThreadMarker` we don't hold on this background task. Sending the class message
    // directly returns the screens array; reading the `isCaptured` BOOL property off-main is a
    // benign property read for polling. We never mutate AppKit state here.
    //
    // SAFETY: `+screens` returns an autoreleased `NSArray<NSScreen>*`; `-isCaptured` is a `BOOL`
    // property on `NSScreen` (macOS 10.0+, the capture semantics since 12). We only read; the
    // Retained handle manages the lifetime.
    unsafe {
        let cls = class!(NSScreen);
        let screens: Retained<NSArray<NSScreen>> = msg_send![cls, screens];
        let count = screens.count();
        for i in 0..count {
            let screen = screens.objectAtIndex(i);
            let captured: bool = msg_send![&*screen, isCaptured];
            if captured {
                return true;
            }
        }
    }
    false
}

/// Non-macOS fallback: no capture signal available → always reports "not captured" (never fires).
#[cfg(not(target_os = "macos"))]
fn is_any_screen_captured() -> bool {
    false
}
