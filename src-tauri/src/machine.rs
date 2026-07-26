//! Cached HARDWARE PROFILE — the one place that answers "what Mac is this?".
//!
//! Murmur's whisper recommendation ([`crate::transcribe::catalog::recommend`]) and its brain
//! advice ([`crate::reason::brain_advice_for`]) both branch on hardware. Before this module every
//! caller re-spawned `sysctl -n hw.memsize` per call (`transcribe::model::total_ram_bytes`,
//! `commands::model_perf::total_ram_gb`) — so a single `list_brain_models` paid a subprocess. The
//! profile is now computed ONCE behind a [`OnceLock`] and shared, which is strictly FEWER spawns
//! than before, not more.
//!
//! ZERO NEW DEPENDENCIES, ZERO manifest edits. This extends the pattern already shipped at
//! `transcribe::model::total_ram_bytes`: spawn `sysctl -n <key>` and parse stdout. `libc` is NOT a
//! direct dependency of this crate (only transitively in `Cargo.lock`), so `libc::sysctlbyname`
//! would have required a manifest edit; spawning `sysctl` does not.
//!
//! Keys read (all verified readable on the development Mac, `hw.model` = `Mac16,5`, 2026-07-26 —
//! see `docs/research/2026-07-26-ux-program-five-workstreams.md` §8):
//!
//! | key | value there | used for |
//! |---|---|---|
//! | `hw.memsize` | `68719476736` | the RAM term of every recommendation |
//! | `hw.optional.arm64` | `1` | Apple Silicon vs Intel (the Intel cap) |
//! | `sysctl.proc_translated` | `0` | Rosetta detection (recorded; nothing branches on it yet) |
//! | `machdep.cpu.brand_string` | `Apple M4 Max` | the display chip clause |
//!
//! FFI SAFETY (rules §7). The `sysctl` reads are subprocess spawns — no FFI at all. The ONE FFI
//! read here is free disk ([`free_disk_bytes`]): `NSURL resourceValuesForKeys:error:` can raise
//! `NSInvalidArgumentException`, and a raised ObjC exception unwinding across the FFI boundary is
//! exactly the class that aborted this app at launch once (`NSScreen.isCaptured`). So the whole
//! read runs inside `objc2::exception::catch` — NOT `std::panic::catch_unwind`, which cannot catch
//! an ObjC exception — and the `NSNumber` the dictionary returns is read through a
//! `respondsToSelector:`-GUARDED `msg_send![num, longLongValue]` (`objc2-foundation` does not
//! enable the `NSValue` feature here, so there is no typed binding; a manifest edit was
//! deliberately avoided).
//!
//! EVERY probe FAILS SOFT: a probe that cannot read returns `None` and NEVER panics
//! (regression: `probes_never_panic`). No PII is logged — this module logs nothing at all.

use std::path::Path;
use std::sync::OnceLock;

/// Longest `machdep.cpu.brand_string` we are willing to show. Apple Silicon returns short marketing
/// strings (`Apple M4 Max` = 12 chars); Intel returns `Intel(R) Core(TM) i7-9750H CPU @ 2.60GHz`,
/// which reads absurdly in a one-line "this Mac" clause and is REJECTED by [`normalize_chip_name`].
const MAX_CHIP_NAME_CHARS: usize = 24;

/// The prefix a chip string must carry to be shown at all (see [`normalize_chip_name`]).
const CHIP_NAME_PREFIX: &str = "Apple ";

/// A cached snapshot of the STABLE hardware facts. Everything here is immutable for the life of the
/// process (RAM, arch, chip), so it is computed once. VOLATILE facts (free disk) deliberately live
/// outside this struct — see [`free_disk_bytes`].
///
/// Every field is `Option`: a probe that cannot read is `None`, never a guessed value. Consumers
/// must decide their own fail direction (the whisper default fails SMALL; the parakeet RAM guard
/// fails OPEN) — this module never guesses on their behalf.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MachineProfile {
    /// Total physical RAM in bytes (`hw.memsize`).
    pub total_ram_bytes: Option<u64>,
    /// `true` on Apple Silicon (`hw.optional.arm64` == 1); `None` when the key could not be read.
    ///
    /// IMPORTANT, and easy to get wrong: a real Intel Mac lands in `None`, **not** `Some(false)` —
    /// the `hw.optional.arm64` OID does not EXIST on Intel, so `sysctl` exits non-zero rather than
    /// printing `0`. (That absence is precisely why Apple's own guidance treats a `sysctlbyname`
    /// error as "not Apple Silicon".) `Some(false)` is therefore near-unreachable on shipping
    /// hardware and is kept only for a machine that answers the key negatively.
    ///
    /// Consumers must NOT collapse `Some(false)` and `None` into one "Intel" case: the recommender
    /// gives them the same conservative SIZE but deliberately different reasons
    /// (`NotAppleSilicon` vs `ArchUnknown`), because only the first may put the word "Intel" in
    /// front of a user.
    pub apple_silicon: Option<bool>,
    /// `true` when this process runs under Rosetta (`sysctl.proc_translated` == 1). Recorded
    /// because it is free to read; nothing branches on it yet (rule: ship only what a consumer
    /// reads — this one does NOT cross IPC).
    pub rosetta: Option<bool>,
    /// The NORMALISED marketing chip name (`Apple M4 Max`), or `None` when the raw string fails
    /// [`normalize_chip_name`] (every Intel string does).
    pub chip_name: Option<String>,
}

/// The process-wide cached profile. Computed on first read; the underlying facts cannot change
/// while the process lives, so caching is sound and saves a subprocess per consumer.
pub fn profile() -> &'static MachineProfile {
    static PROFILE: OnceLock<MachineProfile> = OnceLock::new();
    PROFILE.get_or_init(read_profile)
}

/// Total physical RAM in bytes, from the cached profile. The one place the rest of the crate should
/// ask (`transcribe::model` and `commands::model_perf` both route here).
pub fn total_ram_bytes() -> Option<u64> {
    profile().total_ram_bytes
}

/// Read every stable probe once. Each term fails soft to `None` independently — an unreadable arch
/// never costs us the RAM figure.
fn read_profile() -> MachineProfile {
    MachineProfile {
        total_ram_bytes: sysctl("hw.memsize").and_then(|v| v.parse().ok()),
        apple_silicon: sysctl("hw.optional.arm64").and_then(|v| parse_sysctl_bool(&v)),
        rosetta: sysctl("sysctl.proc_translated").and_then(|v| parse_sysctl_bool(&v)),
        chip_name: sysctl("machdep.cpu.brand_string")
            .as_deref()
            .and_then(normalize_chip_name),
    }
}

/// Spawn `sysctl -n <key>` and return its trimmed stdout. `None` on ANY failure (missing binary,
/// non-zero exit — which is what an ABSENT key produces — non-UTF8 output). Never panics.
///
/// TRAP (recorded 2026-07-26): under the default agent sandbox every one of these reads fails with
/// "Operation not permitted". That is a sandbox artefact, NOT evidence the key is missing — re-probe
/// with the sandbox disabled before concluding anything about hardware availability.
fn sysctl(key: &str) -> Option<String> {
    let out = std::process::Command::new("sysctl")
        .arg("-n")
        .arg(key)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() {
        return None;
    }
    Some(s)
}

/// `sysctl` renders its boolean-ish integer keys as `0` / `1`. Anything else is unrecognised and
/// reads as `None` (never a guessed `false`, which would silently look like "Intel").
fn parse_sysctl_bool(raw: &str) -> Option<bool> {
    match raw.trim() {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

/// Normalise `machdep.cpu.brand_string` for DISPLAY, or drop the chip clause entirely.
///
/// Accepts ONLY a string that starts `"Apple "` and is at most [`MAX_CHIP_NAME_CHARS`] characters —
/// i.e. the short Apple-Silicon marketing names (`Apple M4 Max`, `Apple M1`). Intel returns
/// `Intel(R) Core(TM) i7-9750H CPU @ 2.60GHz`, which MUST be rejected: pasted into a one-line "your
/// Mac" sentence it reads absurdly, and we would rather say nothing about the chip than say that.
/// `None` = show no chip clause at all (never a truncated or invented name).
pub fn normalize_chip_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if !trimmed.starts_with(CHIP_NAME_PREFIX) {
        return None;
    }
    // Count CHARACTERS, not bytes — a byte length would misjudge a non-ASCII string.
    if trimmed.chars().count() > MAX_CHIP_NAME_CHARS {
        return None;
    }
    // A bare "Apple " with nothing after it is not a chip name.
    if trimmed.len() == CHIP_NAME_PREFIX.len() {
        return None;
    }
    Some(trimmed.to_string())
}

/// A stable, content-free fingerprint of the machine, used ONLY to notice that the app moved to a
/// different Mac (a restore-from-backup / Migration Assistant move) so the recommendation can be
/// re-offered once.
///
/// `None` unless EVERY embedded term was actually read. The rule is deliberately all-or-nothing:
/// `read_profile` spawns four independent `sysctl` subprocesses, so a transient spawn failure on
/// just one of them would otherwise bake `arch=unknown` or `chip=` into an otherwise-valid string.
/// Because the profile is a process-wide `OnceLock`, that degraded value would then be persisted for
/// the whole launch and compare UNEQUAL to the next good read — firing a "you moved to a new Mac"
/// notice at a user who did nothing. A partially-degraded profile is not a weaker fingerprint, it is
/// a WRONG one, so it produces none at all.
///
/// (An unreadable arch is also unrepresentable here on purpose: `Some(false)` and `None` mean
/// different things — see [`MachineProfile::apple_silicon`] — and collapsing them into one token
/// would make a real Intel Mac and a failed probe compare equal.)
///
/// Carries no PII: a RAM byte count, an arch bit and a public marketing chip name.
pub fn fingerprint(p: &MachineProfile) -> Option<String> {
    let ram = p.total_ram_bytes?;
    let arch = match p.apple_silicon? {
        true => "arm64",
        false => "x86_64",
    };
    let chip = p.chip_name.as_deref()?;
    Some(format!("ram={ram};arch={arch};chip={chip}"))
}

/// The fingerprint of the CURRENT machine (cached profile).
pub fn current_fingerprint() -> Option<String> {
    fingerprint(profile())
}

/// Free bytes on the volume that holds `path`, as macOS itself would report them for an
/// "important" (user-initiated, non-purgeable-inclusive) write — i.e. the number that actually
/// predicts whether a multi-GB model download will fit. VOLATILE by nature, so it is deliberately
/// NOT part of the cached [`MachineProfile`]: two readers at different instants must not be able to
/// disagree, which is why exactly ONE command embeds it.
///
/// Crash-safe per rules §7, with BOTH guards — they catch different things and neither is
/// sufficient alone:
///
/// - `objc2::exception::catch` contains an **ObjC exception**. `resourceValuesForKeys:error:` can
///   raise `NSInvalidArgumentException`, and an ObjC exception unwinding across FFI is the class
///   that aborted this app at launch once. `std::panic::catch_unwind` CANNOT catch that.
/// - `std::panic::catch_unwind` contains a **Rust panic**, which `exception::catch` does not stop.
///   This is not theoretical: `objc2-foundation`'s typed `NSURL::fileURLWithPath` asserts its
///   result is non-nil and PANICS otherwise — an empty path reproduces it (caught by
///   `probes_never_panic` before this guard existed). `thermal::read_thermal_level` uses
///   `catch_unwind` for the same reason.
///
/// The `NSNumber` the dictionary returns is read through a `respondsToSelector:`-guarded
/// `msg_send![num, longLongValue]`: `objc2-foundation`'s `NSValue` feature is intentionally not
/// enabled (a manifest edit was deliberately avoided), so there is no typed binding, and we never
/// send a selector we have not proven the receiver implements. `None` on any failure. NEVER panics.
#[cfg(target_os = "macos")]
pub fn free_disk_bytes(path: &Path) -> Option<u64> {
    use std::panic::AssertUnwindSafe;

    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2::{msg_send, sel};
    use objc2_foundation::{
        NSArray, NSString, NSURLResourceKey, NSURLVolumeAvailableCapacityForImportantUsageKey, NSURL,
    };

    let path_str = path.to_string_lossy().to_string();
    // Cheap fail-fast: an empty path has no volume, and it is the input that makes the typed
    // `fileURLWithPath` binding panic. Both guards below would contain it anyway; refusing it here
    // keeps a routine miss off the panic machinery entirely.
    if path_str.trim().is_empty() {
        return None;
    }

    // The ENTIRE ObjC interaction lives inside `catch`. `Retained<_>`/`&NSURL` are not
    // `UnwindSafe`, and these are pure reads with nothing left half-mutated across an unwind, so
    // `AssertUnwindSafe` is sound (same discipline as `extract::pdf::catch_objc`).
    let read = || {
        let ns_path = NSString::from_str(&path_str);
        // `fileURLWithPath` and `resourceValuesForKeys:error:` are SAFE bindings in objc2 0.6 —
        // wrapping them in `unsafe` is an `unused_unsafe` error under CI's clippy `-D warnings`.
        // The genuinely unsafe operations below (the static constant read and the `msg_send!`)
        // keep their blocks and their SAFETY notes.
        let url = NSURL::fileURLWithPath(&ns_path);
        // SAFETY: reading a documented Foundation string constant.
        let key: &NSURLResourceKey = unsafe { NSURLVolumeAvailableCapacityForImportantUsageKey };
        let keys = NSArray::from_slice(&[key]);
        let values = url.resourceValuesForKeys_error(&keys).ok()?;
        let number: Retained<AnyObject> = values.objectForKey(key)?;
        // GUARDED selector send (rules §7): never send a selector we have not proven the receiver
        // implements. `resourceValuesForKeys:` is documented to return an NSNumber for this key,
        // but a nil/NSNull/foreign object would otherwise be an unrecognized-selector NSException.
        // SAFETY: `respondsToSelector:` is implemented by every NSObject descendant and returns BOOL.
        let responds: bool = unsafe { msg_send![&*number, respondsToSelector: sel!(longLongValue)] };
        if !responds {
            return None;
        }
        // SAFETY: the selector was just proven present; `longLongValue` returns a `long long`.
        let raw: i64 = unsafe { msg_send![&*number, longLongValue] };
        u64::try_from(raw).ok()
    };
    // Layered, outermost-last: an ObjC exception is translated to `Err` by `exception::catch`, and
    // any Rust panic that escapes it (an objc2 nil assertion) is contained by `catch_unwind`.
    // Either `Err`, or an inner `None`, means "unknown" — never a panic, never an abort.
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        objc2::exception::catch(AssertUnwindSafe(read))
    }))
    .ok()
    .and_then(|caught| caught.ok().flatten())
}

/// Non-macOS builds have no NSURL volume API — free disk is simply unknown.
#[cfg(not(target_os = "macos"))]
pub fn free_disk_bytes(_path: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C10 — the chip-string normalisation, against LITERAL fixtures for BOTH shapes. The Apple
    /// fixture is the string this development Mac really returns (verified 2026-07-26); the Intel
    /// fixture is the documented long form that must be REJECTED outright rather than truncated.
    #[test]
    fn chip_name_accepts_apple_and_rejects_intel() {
        assert_eq!(
            normalize_chip_name("Apple M4 Max").as_deref(),
            Some("Apple M4 Max")
        );
        assert_eq!(normalize_chip_name("Apple M1").as_deref(), Some("Apple M1"));
        assert_eq!(
            normalize_chip_name("  Apple M2 Ultra  ").as_deref(),
            Some("Apple M2 Ultra"),
            "surrounding whitespace from sysctl stdout is trimmed"
        );

        // Intel's long form — rejected ENTIRELY (drop the clause), never truncated.
        assert_eq!(
            normalize_chip_name("Intel(R) Core(TM) i7-9750H CPU @ 2.60GHz"),
            None
        );
        // Anything that does not start "Apple " is rejected regardless of length.
        assert_eq!(normalize_chip_name("M4 Max"), None);
        assert_eq!(normalize_chip_name("apple m4 max"), None);
        assert_eq!(normalize_chip_name(""), None);
        // A bare prefix is not a chip name.
        assert_eq!(normalize_chip_name("Apple "), None);
        // An `Apple …` string LONGER than the cap is rejected too (no truncation).
        let long = format!("Apple {}", "M".repeat(MAX_CHIP_NAME_CHARS));
        assert!(long.chars().count() > MAX_CHIP_NAME_CHARS);
        assert_eq!(normalize_chip_name(&long), None);
    }

    /// `sysctl` renders `0`/`1`; anything else must read as UNKNOWN, never a guessed `false`
    /// (a guessed `false` would silently look like "Intel" and cap a real Apple-Silicon Mac).
    #[test]
    fn sysctl_bool_parses_only_zero_and_one() {
        assert_eq!(parse_sysctl_bool("1"), Some(true));
        assert_eq!(parse_sysctl_bool("0"), Some(false));
        assert_eq!(parse_sysctl_bool(" 1\n"), Some(true));
        assert_eq!(parse_sysctl_bool(""), None);
        assert_eq!(parse_sysctl_bool("true"), None);
        assert_eq!(parse_sysctl_bool("2"), None);
    }

    /// The fingerprint is ALL-OR-NOTHING: `None` unless every embedded term was really read.
    ///
    /// A PARTIALLY degraded profile is the dangerous case, not the blind one. `read_profile` spawns
    /// four independent `sysctl` subprocesses; if only the arch or chip spawn fails, an earlier
    /// version still produced `arch=unknown` / `chip=`. Since the profile is a process-wide
    /// `OnceLock`, that string would be persisted for the whole launch and then compare UNEQUAL to
    /// the next good read — telling a user who did nothing that they moved to a new Mac.
    #[test]
    fn fingerprint_is_none_unless_every_term_was_read() {
        let blind = MachineProfile::default();
        assert_eq!(fingerprint(&blind), None);

        // RAM alone is NOT enough: the arch and chip terms are part of the compared string.
        let ram_only = MachineProfile {
            total_ram_bytes: Some(68719476736),
            ..MachineProfile::default()
        };
        assert_eq!(
            fingerprint(&ram_only),
            None,
            "a partially-degraded profile must not produce a fingerprint that will flap"
        );

        // Missing ONLY the chip is still a refusal.
        let no_chip = MachineProfile {
            total_ram_bytes: Some(68719476736),
            apple_silicon: Some(true),
            ..MachineProfile::default()
        };
        assert_eq!(fingerprint(&no_chip), None);

        // Missing ONLY the arch is still a refusal.
        let no_arch = MachineProfile {
            total_ram_bytes: Some(68719476736),
            chip_name: Some("Apple M4 Max".to_string()),
            ..MachineProfile::default()
        };
        assert_eq!(fingerprint(&no_arch), None);

        // Fully read ⇒ a fingerprint.
        let complete = MachineProfile {
            total_ram_bytes: Some(68719476736),
            apple_silicon: Some(true),
            chip_name: Some("Apple M4 Max".to_string()),
            ..MachineProfile::default()
        };
        assert_eq!(
            fingerprint(&complete).as_deref(),
            Some("ram=68719476736;arch=arm64;chip=Apple M4 Max")
        );
    }

    /// The fingerprint changes when (and only when) a term that matters changes, and is STABLE for
    /// an identical profile — otherwise every launch would nudge.
    #[test]
    fn fingerprint_is_stable_and_term_sensitive() {
        let mac = MachineProfile {
            total_ram_bytes: Some(68719476736),
            apple_silicon: Some(true),
            rosetta: Some(false),
            chip_name: Some("Apple M4 Max".to_string()),
        };
        assert_eq!(fingerprint(&mac), fingerprint(&mac.clone()));

        let more_ram = MachineProfile {
            total_ram_bytes: Some(137438953472),
            ..mac.clone()
        };
        assert_ne!(fingerprint(&mac), fingerprint(&more_ram));

        let intel = MachineProfile {
            apple_silicon: Some(false),
            chip_name: None,
            ..mac.clone()
        };
        assert_ne!(fingerprint(&mac), fingerprint(&intel));

        // `rosetta` is NOT part of the fingerprint: launching the same Mac's x86_64 slice under
        // Rosetta must not read as "you moved to a different machine".
        let translated = MachineProfile {
            rosetta: Some(true),
            ..mac.clone()
        };
        assert_eq!(fingerprint(&mac), fingerprint(&translated));
    }

    /// FFI/subprocess smoke in the mould of `thermal::read_thermal_level_never_panics_and_qos_is_callable`:
    /// EVERY probe must return a value or `None` and NEVER panic — including under a sandbox that
    /// refuses `sysctl` outright and on a path whose volume cannot be read. This is the regression
    /// for the abort-at-launch class (rules §7): the free-disk read must survive a raised ObjC
    /// exception, which is why it is wrapped in `objc2::exception::catch`.
    #[test]
    fn probes_never_panic() {
        // Cached profile: any combination of Some/None is acceptable; not panicking is the contract.
        let p = profile();
        if let Some(ram) = p.total_ram_bytes {
            assert!(ram > 0, "a readable hw.memsize must be positive");
        }
        if let Some(chip) = p.chip_name.as_deref() {
            assert!(chip.starts_with(CHIP_NAME_PREFIX));
            assert!(chip.chars().count() <= MAX_CHIP_NAME_CHARS);
        }
        // Reading twice returns the SAME cached snapshot (one probe per process).
        assert_eq!(p, profile());

        // A missing sysctl key exits non-zero → None, no panic.
        assert_eq!(sysctl("murmur.no.such.key"), None);

        // The fingerprint follows the profile without panicking.
        let _ = current_fingerprint();

        // Free disk over a real dir and a path that does not exist. Neither may panic or abort.
        // The VALUE is deliberately unasserted — `0` is a legitimate answer (a full volume, or a
        // URL whose volume the OS declines to describe); the contract here is "never aborts", not
        // "always knows".
        for path in [
            std::env::temp_dir(),
            std::env::temp_dir().join("murmur-no-such-dir-probe"),
        ] {
            let _: Option<u64> = free_disk_bytes(&path);
        }

        // REGRESSION (observed, not hypothesised): an EMPTY path made `objc2-foundation`'s typed
        // `NSURL::fileURLWithPath` binding panic with "unexpected NULL returned from
        // +[NSURL fileURLWithPath:]" — a RUST panic, which `objc2::exception::catch` does not
        // contain. It must read as a plain unknown.
        assert_eq!(free_disk_bytes(&std::path::PathBuf::new()), None);
        assert_eq!(free_disk_bytes(std::path::Path::new("   ")), None);
    }
}
