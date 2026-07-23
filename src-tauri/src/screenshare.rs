//! Screen-share / screen-capture watcher → auto-relock (Stage E).
//!
//! When the screen STARTS being shared in a video call (a Zoom/Meet/Teams "share screen") any
//! session-unlocked sealed folders are a privacy leak — their plaintext markdown is live in the DB
//! and on screen. This watcher detects the rising edge (not-sharing → sharing) and calls
//! `relock_all_inner`, which clears the session unlock set and zeroizes the cached KEK, plus emits
//! a Tauri event so the UI can toast.
//!
//! DETECTION IS A BEST-EFFORT HEURISTIC (the user explicitly accepted false positives — a relock is
//! cheap and non-destructive). There is NO public macOS API that reliably reports "a conferencing
//! app is sharing the screen": `CGDisplayIsCaptured` only fires on *exclusive* display capture and
//! never trips on Zoom/Meet/Teams; `NSScreen.isCaptured` does not exist on macOS (it is a UIScreen /
//! iOS selector — sending it was the prior boot-abort root cause). So instead we look for the
//! tell-tale UI a conferencing app floats ONLY while a share is active: the "you're sharing your
//! screen" / "stop sharing" control bars and share toolbars. We enumerate on-screen windows with
//! the pure CoreGraphics C function `CGWindowListCopyWindowInfo` and match each window's owner +
//! title against a maintainable heuristic table (see the consts below). Biasing on the *sharing
//! control window* (not merely the app running, nor merely being in a call) keeps false positives
//! to the share-active window.
//!
//! IMPLEMENTATION: a ~1.5s poll on a dedicated OS thread, marshaled to the main thread, with
//! rising-edge detection (we act only on false→true, never re-fire while sharing stays on).
//!
//! HONEST LIMITS (documented): this only sees what apps expose. (a) Apps that do NOT name their
//! share-control window — Zoom in particular often exposes no `kCGWindowName` for its share toolbar
//! — are MISSED by the title match. (b) A single-window/tab WebRTC share with no floating control
//! bar can be missed. (c) macOS bias toward false positives means a browser tab merely *titled*
//! "screen sharing" can trip it (accepted). The manual "Lock all" button in the UI remains the
//! authoritative backstop for everything the heuristic misses.
//!
//! CRASH-SAFETY: every probe call is a plain CoreGraphics / CoreFoundation **C function**
//! (`CGWindowListCopyWindowInfo`, `CFArrayGetCount`, `CFArrayGetValueAtIndex`,
//! `CFDictionaryGetValue`, `CFGetTypeID`, CFString reads). NONE is an Objective-C `msg_send` /
//! selector dispatch, so there is NO "unrecognized selector" `NSException` that could cross the FFI
//! boundary and abort the process ("Rust cannot catch foreign exceptions") — which is exactly what
//! the prior `msg_send![screen, isCaptured]` attempt did. We deliberately do NOT call NSWorkspace
//! (which would require a guarded `respondsToSelector:` dance); the window list already names the
//! owning app, so app-set corroboration is free and exception-free.
//!
//! GRACEFUL DEGRADATION: on a non-macOS host, or if the window list cannot be obtained, the poll
//! simply reports "not sharing" and never fires — it never panics.

use std::time::Duration;

use tauri::{AppHandle, Emitter, Manager};

use crate::state::AppState;

/// Event emitted to all windows when screen sharing STARTS (rising edge). The UI listens and toasts
/// "Screen sharing detected — locked notes were re-secured."
pub const EVENT_SCREEN_SHARE_STARTED: &str = "murmur://screen-share-started";
/// Emitted when logical visibility was revoked but physical cleanup could not finish. The main
/// Murmur window is hidden before this event is sent; the UI must never present this as success.
pub const EVENT_SCREEN_SHARE_RELOCK_FAILED: &str = "murmur://screen-share-relock-failed";

/// Poll cadence for the share-state check. ~1.5s is responsive enough to re-secure quickly while
/// being negligible CPU.
const POLL_INTERVAL: Duration = Duration::from_millis(1500);

// ── Heuristic tables (BEST-EFFORT — expect to tweak these as apps change) ─────────────────────────
//
// These are maintained by hand and WILL drift as conferencing apps rename processes or restyle
// their share UI. They are matched case-insensitively as substrings. Keep them specific to the
// *active-share* surface so we fire on sharing, not on the app merely running or being in a call.

/// Window-OWNER names (`kCGWindowOwnerName`) of apps that can screen-share. Matching the owner is
/// only the *first* gate — we additionally require a sharing-indicator title (see
/// `APP_SHARING_INDICATORS`) so that merely running the app does not trip a relock. HEURISTIC.
const CONFERENCING_OWNERS: &[&str] = &[
    "zoom.us",         // Zoom
    "zoom",            // Zoom (older / localized helper process names)
    "microsoft teams", // Teams classic + "Microsoft Teams (work or school)"
    "google chrome",   // Meet / generic WebRTC share runs inside Chrome
    "chromium",
    "google chrome canary",
    "safari", // Meet / WebRTC share inside Safari
    "microsoft edge",
    "firefox",
    "webex", // Cisco Webex Meetings ("Webex", "Meeting Center")
    "cisco webex",
    "slack", // Slack huddles
    "discord",
    "whereby",
    "around",
];

/// STRONG, owner-AGNOSTIC active-share phrases. A window titled with one of these is almost
/// certainly a live share-control bar (Teams: "You're sharing your screen"; Chrome/Meet: "X is
/// sharing your screen"). We fire on these regardless of the reported owner — important because on
/// macOS 26 (Tahoe) `CGWindowListCopyWindowInfo` mis-attributes some status-bar items to "Control
/// Center" (FB18327911), which would otherwise hide the real owner. HEURISTIC.
const STRONG_SHARING_PHRASES: &[&str] = &[
    "sharing your screen", // covers "you are/you're/X is sharing your screen"
    "stop sharing",        // the Stop-Sharing control common to share bars
];

/// WEAKER active-share hints that only count when the window is owned by a known conferencing app
/// (`CONFERENCING_OWNERS`). HEURISTIC.
const APP_SHARING_INDICATORS: &[&str] = &[
    "you are sharing",
    "you're sharing",
    "screen sharing",
    "screen share",
    "share toolbar", // Zoom share/annotation toolbar
    "as_toolbar",    // Zoom internal share/annotation toolbar window name
    "sharing indicator",
];

/// Spawn the screen-share watcher on a dedicated OS thread. Best-effort: if the config flag is off,
/// or if the platform can't report share state, this is a no-op loop.
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

    // Poll on a DEDICATED OS THREAD (not the async runtime): we marshal the actual probe to the main
    // thread (see `captured_on_main`) and block on its reply between polls, which a std thread can do
    // safely. The probe itself is pure CoreGraphics C — main-thread-safe and exception-free — but we
    // keep the marshaling for consistency with the rest of the macOS bridge.
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
        initial_sharing = was_captured,
        "screen-share watcher started"
    );

    loop {
        std::thread::sleep(POLL_INTERVAL);
        let now_captured = captured_on_main(&app);

        // Rising edge only: fire on START (false → true). We do not re-fire while it stays true, and
        // we silently reset on the falling edge.
        if now_captured && !was_captured {
            tracing::warn!(
                target: "screenshare",
                "screen sharing started — relocking all session-unlocked folders"
            );
            on_capture_started(&app);
        }
        was_captured = now_captured;
    }
}

/// Run `is_any_screen_captured()` on the MAIN THREAD via Tauri's main-thread dispatch, returning the
/// result over a channel. Returns false if the app is shutting down or the main thread does not
/// reply promptly (never blocks forever, never throws across the boundary).
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

/// React to a share-start rising edge: relock everything + emit the UI event.
fn on_capture_started(app: &AppHandle) {
    let state = app.state::<AppState>();
    // relock_all_inner takes &AppState; State derefs to it.
    let secured = match crate::commands::relock_all_inner(&state) {
        Ok(()) => true,
        Err(e) => {
            tracing::error!(
                target: "screenshare",
                error = %e,
                "screen-share relock cleanup failed after logical visibility revocation"
            );
            // Fail closed on the surface the watcher controls. The vault conflict remains intact
            // for loss-safe recovery, but Murmur itself disappears from the active capture instead
            // of rendering gated content or claiming that every physical export was secured.
            if let Some(window) = app.get_webview_window("main") {
                if let Err(error) = window.hide() {
                    tracing::error!(
                        target: "screenshare",
                        error = %error,
                        "failed to hide Murmur after screen-share relock cleanup failure"
                    );
                }
            }
            false
        }
    };
    let event = if secured {
        EVENT_SCREEN_SHARE_STARTED
    } else {
        EVENT_SCREEN_SHARE_RELOCK_FAILED
    };
    if let Err(e) = app.emit(event, ()) {
        tracing::warn!(
            target: "screenshare",
            error = %e,
            secured,
            "failed to emit screen-share privacy event"
        );
    }
    // The auto-relock purged ALL pending audit findings — ping the FE inbox too (count-only,
    // same posture as the share-started emit above; best-effort).
    let pending = state.db.count_pending_audit_findings().unwrap_or(0);
    crate::events::emit_audit_updated(app, pending as u32);
}

/// Pure heuristic decision: does an on-screen window (owner + title) look like a LIVE screen-share
/// control surface? Factored out of the FFI walk so it is unit-testable without any window server.
///
/// Logic: fire if the title carries a STRONG owner-agnostic share phrase, OR if the owner is a known
/// conferencing app AND the title carries a weaker app-share hint. Bias is toward active sharing —
/// an app merely running (no share-control window) or an in-call-but-not-sharing state does not
/// match.
#[cfg(target_os = "macos")]
fn is_active_share_window(owner: Option<&str>, title: Option<&str>) -> bool {
    let title_lc = title.map(|t| t.to_ascii_lowercase());
    if let Some(t) = title_lc.as_deref() {
        if STRONG_SHARING_PHRASES.iter().any(|p| t.contains(p)) {
            return true;
        }
    }

    let owner_is_conferencing = owner
        .map(|o| {
            let o = o.to_ascii_lowercase();
            CONFERENCING_OWNERS.iter().any(|c| o.contains(c))
        })
        .unwrap_or(false);
    if owner_is_conferencing {
        if let Some(t) = title_lc.as_deref() {
            if APP_SHARING_INDICATORS.iter().any(|p| t.contains(p)) {
                return true;
            }
        }
    }
    false
}

/// Read a string value (`kCGWindow*` key) out of a window-info dictionary. Crash-safe: every step is
/// a CoreFoundation C function and a type-checked downcast — a missing key or a non-string value
/// degrades to `None` rather than panicking.
#[cfg(target_os = "macos")]
fn dict_string(
    dict: &objc2_core_foundation::CFDictionary,
    key: &objc2_core_foundation::CFString,
) -> Option<String> {
    use core::ffi::c_void;

    use objc2_core_foundation::{CFString, CFType};

    let key_ptr = (key as *const CFString).cast::<c_void>();
    // SAFETY: `key` is a valid CFString constant; `value` returns a borrowed (non-owned) pointer or
    // null. We never retain it past this function.
    let raw = unsafe { dict.value(key_ptr) };
    if raw.is_null() {
        return None;
    }
    // SAFETY: a non-null CF value; treat it as the root CFType, then type-check before reading.
    let cf = unsafe { &*raw.cast::<CFType>() };
    cf.downcast_ref::<CFString>().map(|s| s.to_string())
}

/// Share-state probe (macOS). Returns `true` when a live screen-share control surface is on screen.
///
/// Enumerates on-screen, non-desktop windows via the pure CoreGraphics C function
/// `CGWindowListCopyWindowInfo` and matches each window's owner + title against the heuristic tables
/// above. No Objective-C selector dispatch is involved, so it cannot raise a foreign `NSException`.
/// Returns `false` when nothing is sharing (the sane default) or when the window list is
/// unavailable.
#[cfg(target_os = "macos")]
fn is_any_screen_captured() -> bool {
    use objc2_core_foundation::{CFDictionary, CFType};
    use objc2_core_graphics::{
        kCGNullWindowID, kCGWindowName, kCGWindowOwnerName, CGWindowListCopyWindowInfo,
        CGWindowListOption,
    };

    // On-screen windows only, excluding desktop chrome (wallpaper, Dock, icons). This is the share
    // *control bar* surface we want, and keeps the list small.
    let option =
        CGWindowListOption::OptionOnScreenOnly | CGWindowListOption::ExcludeDesktopElements;
    let windows = match CGWindowListCopyWindowInfo(option, kCGNullWindowID) {
        Some(list) => list,
        // No window list (e.g. headless, or pre-Sonoma without Screen Recording permission). Treat
        // as "not sharing" — best-effort, never throws.
        None => return false,
    };

    let count = windows.count();
    for idx in 0..count {
        // SAFETY: idx in [0, count); the array holds borrowed window-info CFDictionary refs.
        let raw = unsafe { windows.value_at_index(idx) };
        if raw.is_null() {
            continue;
        }
        // SAFETY: non-null CF value from the window list; type-check before treating as a dictionary.
        let cf = unsafe { &*raw.cast::<CFType>() };
        let Some(dict) = cf.downcast_ref::<CFDictionary>() else {
            continue;
        };

        // SAFETY (statics): `kCGWindowOwnerName` / `kCGWindowName` are valid CoreGraphics CFString
        // constants linked from the framework.
        let owner = dict_string(dict, unsafe { kCGWindowOwnerName });
        let title = dict_string(dict, unsafe { kCGWindowName });

        if is_active_share_window(owner.as_deref(), title.as_deref()) {
            // Log ONLY the owning app name + a boolean hint — NEVER the raw window title (a browser
            // tab / doc title is mild PII, and logs must not become the leak — rules §8).
            tracing::info!(
                target: "screenshare",
                owner = owner.as_deref().unwrap_or("<unknown>"),
                has_share_hint = true,
                "active screen-share indicator window detected (heuristic) — will relock"
            );
            return true;
        }
    }
    false
}

/// Non-macOS fallback: no share signal available → always reports "not sharing" (never fires).
#[cfg(not(target_os = "macos"))]
fn is_any_screen_captured() -> bool {
    false
}

#[cfg(test)]
#[cfg(target_os = "macos")]
mod tests {
    use super::*;

    /// The pure heuristic must fire on a real share-control title regardless of owner (Teams /
    /// Chrome-Meet style), so a macOS-26 "Control Center" owner mis-attribution still trips it.
    #[test]
    fn strong_phrase_fires_owner_agnostic() {
        assert!(is_active_share_window(
            Some("Control Center"),
            Some("Alex is sharing your screen")
        ));
        assert!(is_active_share_window(
            Some("Microsoft Teams"),
            Some("You're sharing your screen")
        ));
        assert!(is_active_share_window(None, Some("Stop Sharing")));
    }

    /// A known conferencing owner PLUS a weaker share hint fires; the same owner with a plain
    /// in-call / app-running title does NOT (bias toward active sharing, not mere running).
    #[test]
    fn conferencing_owner_needs_a_share_hint() {
        assert!(is_active_share_window(
            Some("zoom.us"),
            Some("Zoom - screen sharing")
        ));
        // In a Zoom meeting but NOT sharing → must not fire.
        assert!(!is_active_share_window(
            Some("zoom.us"),
            Some("Zoom Meeting")
        ));
        // Zoom merely running with its main window → must not fire.
        assert!(!is_active_share_window(Some("zoom.us"), Some("Zoom")));
    }

    /// Non-conferencing windows never fire, and a weak hint only counts for a conferencing owner.
    #[test]
    fn non_conferencing_or_no_signal_is_false() {
        assert!(!is_active_share_window(Some("Finder"), Some("Desktop")));
        assert!(!is_active_share_window(None, None));
        // Weak hint, but a non-conferencing owner → not enough on its own.
        assert!(!is_active_share_window(
            Some("Preview"),
            Some("screen sharing diagram.png")
        ));
    }

    /// The real FFI probe must return a bool WITHOUT panicking across the CoreGraphics boundary.
    /// We do not assert a specific value: in a normal test/CI environment nothing is sharing (so
    /// `false` is expected), but asserting that would be environment-shaped — the load-bearing
    /// guarantee is "no foreign-exception abort, returns cleanly".
    #[test]
    fn probe_does_not_panic_across_ffi() {
        let _ = is_any_screen_captured();
    }
}
