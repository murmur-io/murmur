//! Thermal governor + QoS tagging for the LIVE transcription loop (T1.5 of the
//! 2026-07-09 transcription-performance plan).
//!
//! The live caption loop re-decodes a rolling audio window on the Metal GPU it shares with
//! the on-device LLM. Under sustained load the Mac heats up; macOS surfaces that as
//! `NSProcessInfo.thermalState`. This module maps that state to a PURE back-off policy:
//!
//! | state    | live tick | reactions scans | live captions |
//! |----------|-----------|-----------------|---------------|
//! | Nominal  | 3 s       | run             | run           |
//! | Fair     | 6 s       | run             | run           |
//! | Serious  | 9 s       | **paused**      | run           |
//! | Critical | 9 s       | paused          | **suspended** |
//!
//! The RECORDING and the post-Stop batch pipeline are NEVER touched by this governor — only
//! the best-effort live loop backs off. Captions degrade; the authoritative transcript is
//! produced at Stop regardless.
//!
//! FFI safety (rules §7): the thermal read uses the objc2-foundation TYPED
//! `NSProcessInfo::processInfo().thermalState()` binding — a plain getter present since
//! macOS 10.10.3 (our floor is 13.4), generated `extern_methods!` over a selector the class
//! is documented to implement; it does not throw. Any doubt — an unknown/future enum value,
//! a Rust-side panic — DEGRADES TO `Nominal` (today's behavior), never a crash. The QoS tag
//! is a plain C function (`pthread_set_qos_class_self_np`) that returns an error code on
//! failure — no exception path at all.

use std::time::Duration;

/// The live-loop tick period at each thermal level (see the module table).
const TICK_NOMINAL: Duration = Duration::from_millis(3000);
const TICK_FAIR: Duration = Duration::from_millis(6000);
const TICK_SERIOUS: Duration = Duration::from_millis(9000);

/// Consecutive BETTER thermal reads required before the governor recovers to a lighter
/// level. Degrading is instant (protect the hardware fast); recovering is damped so a
/// noisy Serious↔Fair boundary can't flap the tick period every read.
const RECOVER_AFTER_READS: u8 = 2;

/// Our own thermal ladder — decoupled from the ObjC enum so the POLICY is pure and
/// headless-testable, and an unknown/future `NSProcessInfoThermalState` value maps to
/// [`ThermalLevel::Nominal`] (degrade-to-today, never a wrong-side surprise).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ThermalLevel {
    Nominal,
    Fair,
    Serious,
    Critical,
}

/// The live loop's thermal back-off state machine. PURE (no FFI inside): the caller feeds it
/// one [`ThermalLevel`] observation per tick (from [`read_thermal_level`]) and reads the
/// policy. Hysteresis: degrade FAST (a worse read applies immediately), recover SLOW
/// ([`RECOVER_AFTER_READS`] consecutive better reads) — no flapping at a boundary.
#[derive(Debug, Default)]
pub struct ThermalGovernor {
    level: Option<ThermalLevel>,
    /// Consecutive reads strictly BETTER than the current level (reset by an equal/worse read).
    better_streak: u8,
}

impl ThermalGovernor {
    /// The governed level (defaults to `Nominal` before the first observation).
    fn level(&self) -> ThermalLevel {
        self.level.unwrap_or(ThermalLevel::Nominal)
    }

    /// Fold one thermal observation in. Worse-or-equal ⇒ adopt immediately (degrade fast) and
    /// reset the recovery streak; better ⇒ adopt only after [`RECOVER_AFTER_READS`] consecutive
    /// better reads (recover slow), adopting the LATEST observed (better) level.
    pub fn observe(&mut self, read: ThermalLevel) {
        let current = self.level();
        if read >= current {
            self.level = Some(read);
            self.better_streak = 0;
            return;
        }
        self.better_streak = self.better_streak.saturating_add(1);
        if self.better_streak >= RECOVER_AFTER_READS {
            self.level = Some(read);
            self.better_streak = 0;
        }
    }

    /// The live-loop sleep for the governed level (see the module table). `Critical` keeps the
    /// `Serious` tick — the loop still runs (recorder-gone detection, manual capture bypass),
    /// it just skips the caption decode via [`Self::captions_suspended`].
    pub fn effective_tick(&self) -> Duration {
        match self.level() {
            ThermalLevel::Nominal => TICK_NOMINAL,
            ThermalLevel::Fair => TICK_FAIR,
            ThermalLevel::Serious | ThermalLevel::Critical => TICK_SERIOUS,
        }
    }

    /// Whether background Realtime-Reactions/bullets scans should be paused (`Serious`+).
    pub fn reactions_paused(&self) -> bool {
        self.level() >= ThermalLevel::Serious
    }

    /// Whether the live caption decode should be suspended entirely (`Critical` only).
    /// The recording + the post-Stop batch pipeline are NEVER affected, and the live loop
    /// exempts user-armed bypass flows (manual voice-capture / a just-fired wake) from the
    /// suspend — a user-facing "listening" state must never freeze on thermal back-off.
    pub fn captions_suspended(&self) -> bool {
        self.level() >= ThermalLevel::Critical
    }
}

/// USER-TURN DECODE DEFER (Brain v2 P0.3 companion): skip exactly ONE live decode tick per
/// user-turn window while an assistant turn is in flight on the LOCAL GGUF brain — the worst
/// Metal co-residency spike is the live whisper decode landing mid-generation. PURE state
/// machine, headless-testable; mirrors the flag-read discipline of
/// `brain_reactions::should_defer_scan` (advisory scheduling hint, `Relaxed` load upstream).
#[derive(Debug, Default)]
pub struct TurnDefer {
    /// Whether the CURRENT contiguous turn window has already consumed its one skipped tick.
    deferred_for_current: bool,
}

impl TurnDefer {
    /// Decide whether THIS tick's decode should be skipped. Skips only when a user turn is in
    /// flight AND the live path is local-GGUF-backed AND this turn window hasn't already been
    /// deferred once. The window re-arms as soon as the turn flag clears.
    pub fn should_skip(&mut self, user_turn_in_progress: bool, live_is_local_gguf: bool) -> bool {
        if !user_turn_in_progress {
            self.deferred_for_current = false;
            return false;
        }
        if !live_is_local_gguf || self.deferred_for_current {
            return false;
        }
        self.deferred_for_current = true;
        true
    }
}

/// Read the CURRENT process thermal state, degraded to [`ThermalLevel::Nominal`] on ANY doubt.
///
/// Uses the objc2-foundation TYPED `NSProcessInfo` binding (feature `NSProcessInfo`) — a plain
/// documented getter (macOS 10.10.3+; our deployment floor is 13.4), so no
/// unrecognized-selector risk (the `NSScreen.isCaptured` class of abort — rules §7). The raw
/// `NSInteger` is matched by VALUE so an unknown/future state maps to `Nominal` (= today's
/// behavior) rather than a wrong back-off. A Rust-side panic inside the call is contained by
/// `catch_unwind` and likewise degrades to `Nominal` — this probe must NEVER take down the
/// live loop, let alone the process.
#[cfg(target_os = "macos")]
pub fn read_thermal_level() -> ThermalLevel {
    let read = std::panic::catch_unwind(|| {
        let state = objc2_foundation::NSProcessInfo::processInfo().thermalState();
        match state.0 {
            1 => ThermalLevel::Fair,
            2 => ThermalLevel::Serious,
            3 => ThermalLevel::Critical,
            // 0 = Nominal; anything unknown/future degrades to Nominal (no wrong-side back-off).
            _ => ThermalLevel::Nominal,
        }
    });
    read.unwrap_or(ThermalLevel::Nominal)
}

/// Non-macOS builds have no thermal probe — always `Nominal` (today's behavior).
#[cfg(not(target_os = "macos"))]
pub fn read_thermal_level() -> ThermalLevel {
    ThermalLevel::Nominal
}

/// Tag the CALLING thread `QOS_CLASS_UTILITY` so macOS schedules the live caption tick and the
/// reactions worker on efficiency cores under contention (Apple energy guidance: background
/// inference belongs at utility-or-lower). Best-effort: `pthread_set_qos_class_self_np` is a
/// plain C function (declared here directly — libc is only a transitive dep and this is its
/// exact, ABI-stable signature) that returns a non-zero error code on failure — logged at
/// debug, never fatal. No exception path exists (rules §7).
#[cfg(target_os = "macos")]
pub fn set_utility_qos() {
    use std::os::raw::{c_int, c_uint};
    /// `QOS_CLASS_UTILITY` from `<sys/qos.h>` (0x11) — the value libc's `qos_class_t` carries.
    const QOS_CLASS_UTILITY: c_uint = 0x11;
    extern "C" {
        fn pthread_set_qos_class_self_np(qos_class: c_uint, relative_priority: c_int) -> c_int;
    }
    // SAFETY: plain C call with primitive args; documented to return an errno-style code.
    let rc = unsafe { pthread_set_qos_class_self_np(QOS_CLASS_UTILITY, 0) };
    if rc != 0 {
        tracing::debug!(target: "thermal", rc, "QoS utility tagging failed; thread stays at default QoS");
    }
}

/// Non-macOS builds: no QoS API — no-op.
#[cfg(not(target_os = "macos"))]
pub fn set_utility_qos() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_maps_levels_to_ticks_and_pauses() {
        let mut g = ThermalGovernor::default();
        // Fresh governor = Nominal: 3 s tick, nothing paused.
        assert_eq!(g.effective_tick(), Duration::from_millis(3000));
        assert!(!g.reactions_paused());
        assert!(!g.captions_suspended());

        g.observe(ThermalLevel::Fair);
        assert_eq!(g.effective_tick(), Duration::from_millis(6000));
        assert!(!g.reactions_paused());
        assert!(!g.captions_suspended());

        g.observe(ThermalLevel::Serious);
        assert_eq!(g.effective_tick(), Duration::from_millis(9000));
        assert!(g.reactions_paused());
        assert!(!g.captions_suspended());

        g.observe(ThermalLevel::Critical);
        assert_eq!(g.effective_tick(), Duration::from_millis(9000));
        assert!(g.reactions_paused());
        assert!(g.captions_suspended());
    }

    #[test]
    fn degrades_fast_recovers_after_two_consecutive_better_reads() {
        let mut g = ThermalGovernor::default();
        // Degrade is INSTANT.
        g.observe(ThermalLevel::Serious);
        assert!(g.reactions_paused());

        // ONE better read does not recover yet…
        g.observe(ThermalLevel::Nominal);
        assert!(g.reactions_paused(), "one better read must not recover");
        // …the SECOND consecutive better read does — to the observed (better) level.
        g.observe(ThermalLevel::Nominal);
        assert!(!g.reactions_paused());
        assert_eq!(g.effective_tick(), Duration::from_millis(3000));
    }

    #[test]
    fn recovery_streak_resets_on_a_worse_or_equal_read_no_flapping() {
        let mut g = ThermalGovernor::default();
        g.observe(ThermalLevel::Serious);
        // better, then equal-to-current: the streak resets, so a later single better read
        // still isn't enough — the boundary can't flap the tick period.
        g.observe(ThermalLevel::Fair);
        g.observe(ThermalLevel::Serious);
        g.observe(ThermalLevel::Fair);
        assert!(
            g.reactions_paused(),
            "streak must reset on the worse read in between"
        );
        g.observe(ThermalLevel::Fair);
        // Two consecutive Fair reads now ⇒ recover to Fair (6 s, reactions run again).
        assert!(!g.reactions_paused());
        assert_eq!(g.effective_tick(), Duration::from_millis(6000));
    }

    #[test]
    fn turn_defer_skips_exactly_one_tick_per_turn_window() {
        let mut d = TurnDefer::default();
        // Idle: never skips.
        assert!(!d.should_skip(false, true));
        // Turn on local GGUF: skip the FIRST tick only…
        assert!(d.should_skip(true, true));
        assert!(!d.should_skip(true, true), "only one skip per turn window");
        assert!(!d.should_skip(true, true));
        // Turn ends → window re-arms → next local turn skips one tick again.
        assert!(!d.should_skip(false, true));
        assert!(d.should_skip(true, true));
    }

    #[test]
    fn turn_defer_never_skips_on_cloud_backed_live_path() {
        let mut d = TurnDefer::default();
        // Cloud turn: no Metal co-residency ⇒ no defer, and it must not consume the window
        // (a mid-turn switch to local would still get its one skip).
        assert!(!d.should_skip(true, false));
        assert!(d.should_skip(true, true));
    }

    #[test]
    fn read_thermal_level_never_panics_and_qos_is_callable() {
        // FFI smoke: on macOS this exercises the real NSProcessInfo getter + the QoS call;
        // elsewhere the no-op stubs. Either way: no panic, a valid level.
        let level = read_thermal_level();
        assert!(matches!(
            level,
            ThermalLevel::Nominal
                | ThermalLevel::Fair
                | ThermalLevel::Serious
                | ThermalLevel::Critical
        ));
        set_utility_qos();
    }
}
