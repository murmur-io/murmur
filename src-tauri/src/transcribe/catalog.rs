//! The ONE whisper model registry — four user-facing rungs plus the honest long tail.
//!
//! Before this module the same nine-or-six model list was hardcoded in THREE places in the
//! frontend (`settings.store.ts`'s `hints` map, `onboarding.component.ts`'s `SIZE_HINTS`, and the
//! `<option>` list in `settings-transcription-section.component.html`) and they had already
//! diverged. This is now the single source of truth; the FE renders what Rust hands it.
//!
//! # The ladder
//!
//! | rung | id | download | headline |
//! |---|---|---|---|
//! | Light | `base` | ~150 MB | Gets the gist. |
//! | Balanced | `small` | ~470 MB | Readable transcripts, small footprint. *(a backend default)* |
//! | Sharp | `large-v3-turbo-q8_0` | ~875 MB | Near-best accuracy, no speed penalty. *(the other backend default)* |
//! | Maximum | `large-v3` | ~3 GB | The heaviest model, for hard audio. |
//!
//! BOTH backend defaults are ON the ladder. That is load-bearing: `default_model_size` resolves
//! `small` on every sub-12-GiB Mac and on every existing install without the turbo file on disk, so
//! a ladder that omitted `small` would render "Custom" as the default state for a whole class of
//! Macs. Everything else (`tiny`, `small-q8_0`, `medium`, `medium-q8_0`, `large-v3-turbo`) lives in
//! the "show every size" long tail with its honest id.
//!
//! # Where the numbers come from (nothing here is invented)
//!
//! RAM figures: `docs/research/2026-07-09-transcription-performance.md` — "RAM guard" bullet
//! (`turbo-q8_0 (~1.2–1.5 GB)`, `small live (~0.9 GB)`, `large-v3 fp16 (~3.9 GB)`) and the
//! executive summary (turbo `~1.2 GB`). Each is rounded UP to the top of the cited range.
//! Download figures: the size table already shipped in-tree and shown to users today
//! (`src/app/features/settings/settings.store.ts` `hints`, mirrored by
//! `onboarding.component.ts` `SIZE_HINTS` and the Settings `<option>` labels).
//!
//! A figure that exists in NEITHER source is `None` — never guessed. `large-v3-q5_0` is the whole
//! reason [`WhisperModel::provisional`] exists: it is a real, downloadable size
//! (`QUANT_MODEL_SIZES`) whose download size appears nowhere in this repo, so it ships hidden
//! rather than with an invented number.
//!
//! # What this module does NOT do
//!
//! [`recommend`] is PURE OVER HARDWARE. It answers "what does this Mac deserve", NOT "what should
//! we select" — the latter is `model::default_model_size`, which is presence-first (a turbo file
//! already on disk wins at any RAM) and must stay that way so no existing install is surprised by
//! an unrequested download. The two answers disagree for most existing installs BY DESIGN, which is
//! why the DTO carries both.

use crate::machine::MachineProfile;
// `is_live_heavy_model_file` is imported inside `mod tests` on purpose: it is used ONLY by the
// equivalence regression, and a top-level import used solely under `#[cfg(test)]` is an
// `unused_imports` error in CI's lib-only clippy build (which `cargo test --lib` masks entirely).
use crate::transcribe::model::TURBO_DEFAULT_MIN_RAM_BYTES;

/// The four rung ids, spelled out so callers never string-literal them.
pub const LIGHT_ID: &str = "base";
pub const BALANCED_ID: &str = "small";
pub const SHARP_ID: &str = "large-v3-turbo-q8_0";
pub const MAXIMUM_ID: &str = "large-v3";

const MB: u64 = 1024 * 1024;
const GB: u64 = 1024 * 1024 * 1024;

/// A user-facing rung of the ladder. `None` on a [`WhisperModel`] means the row is long-tail only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Tier {
    Light,
    Balanced,
    Sharp,
    Maximum,
}

impl Tier {
    /// The human label. This — not the raw checkpoint id — is what the AI map and the ladder show.
    pub fn label(self) -> &'static str {
        match self {
            Tier::Light => "Light",
            Tier::Balanced => "Balanced",
            Tier::Sharp => "Sharp",
            Tier::Maximum => "Maximum",
        }
    }
}

/// One registry row. Static metadata only — presence on disk is layered on by the command.
#[derive(Debug, Clone, Copy)]
pub struct WhisperModel {
    /// The `model_size` string the rest of the app already speaks (`model_filename` maps it to a
    /// `ggml-*.bin`).
    pub id: &'static str,
    /// The ladder rung, or `None` for a long-tail size.
    pub tier: Option<Tier>,
    /// One honest sentence. No superlatives we cannot back with the cited research.
    pub headline: &'static str,
    /// Approximate DOWNLOAD size in bytes, or `None` when no source in this repo states one.
    pub approx_download_bytes: Option<u64>,
    /// Approximate RESIDENT size in bytes while transcribing, or `None` when unmeasured.
    pub approx_ram_bytes: Option<u64>,
    /// Whether this size is safe for the 3 s LIVE caption tick. MUST agree with
    /// `model::is_live_heavy_model_file` (regression: `registry_live_safe_matches_classifier`).
    pub live_safe: bool,
    /// Hidden from every surface until its numbers are measured. See the module doc.
    pub provisional: bool,
    /// Display/ordering rank by COST, ascending — deliberately not "by accuracy". The cited
    /// research puts `large` at FLEURS Polish 7.2, WORSE than large-v2's 5.4, and direct
    /// turbo-vs-large-v3 is unpublished; so `large-v3` ranking after `large-v3-turbo-q8_0` is a
    /// statement about download + RAM + wall-clock, never a claim that it transcribes better.
    pub power: u8,
}

/// The registry, in display order (ascending cost).
const REGISTRY: &[WhisperModel] = &[
    WhisperModel {
        id: "tiny",
        tier: None,
        headline: "Smallest and fastest; makes frequent mistakes.",
        approx_download_bytes: Some(75 * MB),
        approx_ram_bytes: None,
        live_safe: true,
        provisional: false,
        power: 1,
    },
    WhisperModel {
        id: LIGHT_ID,
        tier: Some(Tier::Light),
        headline: "Gets the gist.",
        approx_download_bytes: Some(150 * MB),
        approx_ram_bytes: None,
        live_safe: true,
        provisional: false,
        power: 2,
    },
    WhisperModel {
        id: "small-q8_0",
        tier: None,
        headline: "Balanced accuracy at a smaller download.",
        approx_download_bytes: Some(270 * MB),
        approx_ram_bytes: None,
        live_safe: true,
        provisional: false,
        power: 3,
    },
    WhisperModel {
        id: BALANCED_ID,
        tier: Some(Tier::Balanced),
        headline: "Readable transcripts, small footprint. Also powers live captions.",
        approx_download_bytes: Some(470 * MB),
        // "small live (~0.9 GB)" — 2026-07-09 research, RAM-guard bullet.
        approx_ram_bytes: Some(9 * GB / 10),
        live_safe: true,
        provisional: false,
        power: 4,
    },
    WhisperModel {
        id: "medium-q8_0",
        tier: None,
        headline: "Medium accuracy at a smaller download.",
        approx_download_bytes: Some(850 * MB),
        approx_ram_bytes: None,
        live_safe: false,
        provisional: false,
        power: 5,
    },
    WhisperModel {
        id: SHARP_ID,
        tier: Some(Tier::Sharp),
        headline: "Near-best accuracy, no speed penalty.",
        approx_download_bytes: Some(875 * MB),
        // "turbo-q8_0 (~1.2–1.5 GB)" — 2026-07-09 research, RAM-guard bullet; rounded UP.
        approx_ram_bytes: Some(3 * GB / 2),
        live_safe: false,
        provisional: false,
        power: 6,
    },
    WhisperModel {
        id: "medium",
        tier: None,
        headline: "The older mid-size model; superseded by Sharp.",
        approx_download_bytes: Some(3 * GB / 2),
        approx_ram_bytes: None,
        live_safe: false,
        provisional: false,
        power: 7,
    },
    WhisperModel {
        id: "large-v3-turbo",
        tier: None,
        headline: "Sharp without the quantisation; a bigger download for the same speed.",
        approx_download_bytes: Some(1638 * MB),
        approx_ram_bytes: None,
        live_safe: false,
        provisional: false,
        power: 8,
    },
    WhisperModel {
        id: MAXIMUM_ID,
        tier: Some(Tier::Maximum),
        headline: "The heaviest model, for hard audio.",
        approx_download_bytes: Some(3 * GB),
        // "large-v3 fp16 (~3.9 GB)" — 2026-07-09 research, RAM-guard bullet.
        approx_ram_bytes: Some(39 * GB / 10),
        live_safe: false,
        provisional: false,
        power: 9,
    },
    WhisperModel {
        id: "large-v3-q5_0",
        tier: None,
        headline: "A smaller quantisation of Maximum.",
        // DELIBERATELY None: this size appears nowhere in the repo and inventing one would be
        // exactly the slop this registry exists to remove. `provisional` keeps it hidden until
        // somebody measures it.
        approx_download_bytes: None,
        approx_ram_bytes: None,
        live_safe: false,
        provisional: true,
        power: 10,
    },
];

/// Every registry row INCLUDING provisional ones. Surfaces must filter (see [`visible`]).
pub fn all() -> &'static [WhisperModel] {
    REGISTRY
}

/// The rows a user may see: everything except [`WhisperModel::provisional`].
pub fn visible() -> impl Iterator<Item = &'static WhisperModel> {
    REGISTRY.iter().filter(|m| !m.provisional)
}

/// Look a size id up in the registry. `None` for an unknown/blank id — the caller then renders the
/// raw id, never an empty cell.
pub fn model_by_id(id: &str) -> Option<&'static WhisperModel> {
    let id = id.trim();
    REGISTRY.iter().find(|m| m.id == id)
}

/// The human tier label for a size id, or `None` when the id is not a ladder rung (a long-tail
/// size, a provisional one, or an unknown/custom id).
pub fn tier_label(id: &str) -> Option<&'static str> {
    model_by_id(id).and_then(|m| m.tier).map(Tier::label)
}

/// WHY a recommendation is what it is — authored HERE, in Rust, next to the decision, so the copy
/// cannot drift from the branch that produced it. The frontend maps a variant to a sentence; it
/// never assembles the reasoning itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RecommendReason {
    /// The recommended file is ALREADY on disk, so presence — not RAM — decided this. Rendering a
    /// RAM-causal sentence here would be a lie about a presence-first decision.
    AlreadyDownloaded,
    /// A fresh install on an Apple-Silicon Mac with ample RAM. This is the ONE branch where a
    /// causal sentence ("your Mac has N GB, so Murmur picked Sharp") is honest.
    FreshInstallAmpleRam,
    /// PROVEN not Apple Silicon (the arch probe answered, and answered `false`): capped at Balanced
    /// regardless of RAM. Only this variant may say the word "Intel".
    ///
    /// NOTE it is currently hard to reach on real hardware, and that is exactly why it is separate
    /// from [`RecommendReason::ArchUnknown`]: `hw.optional.arm64` does not EXIST on an Intel Mac, so
    /// the probe fails rather than returning `0` (that absence is why Apple's own guidance treats a
    /// `sysctlbyname` error as "not Apple Silicon"). Collapsing the two would put the word "Intel"
    /// in front of a user whose Apple-Silicon Mac merely failed to answer.
    NotAppleSilicon,
    /// The arch probe could not be read at all, so the cap is conservative rather than informed.
    /// Makes NO claim about the chip — the copy for this variant must not name a chip family.
    ArchUnknown,
    /// Apple Silicon whose MEASURED RAM is below the turbo floor. A genuinely RAM-caused decision,
    /// so a RAM-causal sentence is honest here — but it is the "not enough", not the "plenty" one.
    ModestRam,
    /// Apple Silicon whose RAM could not be measured. Fails conservative and makes NO claim.
    RamUnknown,
    /// A model is ALREADY on disk and it is not the turbo file, so install history — and ONLY
    /// install history — kept the auto default conservative: this machine's hardware would have
    /// justified more. The only variant that may reference an existing install, and it is emitted
    /// only when the hardware answer was [`RecommendReason::FreshInstallAmpleRam`], so it never
    /// denies a hardware cause that also applies.
    ExistingInstall,
}

/// A hardware recommendation: the size plus the reason for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Recommendation {
    pub id: &'static str,
    pub reason: RecommendReason,
}

/// What does THIS HARDWARE deserve? PURE over `(total_ram_bytes, apple_silicon)` — it takes NO
/// models-dir argument and is completely blind to what is on disk.
///
/// That blindness is the whole point, and it is the single most important correctness property in
/// this module. `model::default_model_size` is presence-first (a downloaded turbo file wins at any
/// RAM; an existing install is never surprised by an 875 MB download), so on most real installs the
/// two answers DIFFER. The DTO ships both — the honest hardware answer AND the never-surprise auto
/// default — because collapsing them would make the "your pick is below what this Mac can run"
/// state unreachable and would make the badge disagree with the selected size on every existing
/// install. Regression: `recommendation_ignores_whats_on_disk`.
///
/// Every branch below caps at [`BALANCED_ID`] except the one that can justify more, and each one
/// carries a reason that states EXACTLY what is true about it — never a neighbouring branch's claim.
///
/// Branches:
/// 1. `Some(false)` — proven not Apple Silicon ⇒ [`BALANCED_ID`], [`RecommendReason::NotAppleSilicon`].
///    Murmur ships `--target universal-apple-darwin` and whisper-rs runs with `features = ["metal"]`;
///    Metal on an Intel integrated GPU is not the machine the 12 GiB floor was tuned against.
/// 2. `None` — the arch probe could not be read ⇒ [`BALANCED_ID`], [`RecommendReason::ArchUnknown`].
///    Same conservative SIZE as branch 1 but a DIFFERENT reason, because the copy for branch 1 names
///    a chip family and that claim would be false here.
/// 3. Apple Silicon, RAM ≥ `TURBO_DEFAULT_MIN_RAM_BYTES` ⇒ [`SHARP_ID`],
///    [`RecommendReason::FreshInstallAmpleRam`] — the one branch where "your Mac has N GB, so…" is honest.
/// 4. Apple Silicon, RAM measured and below the floor ⇒ [`BALANCED_ID`], [`RecommendReason::ModestRam`].
/// 5. Apple Silicon, RAM unreadable ⇒ [`BALANCED_ID`], [`RecommendReason::RamUnknown`].
///
/// Every failure direction is SMALL: never recommend a multi-hundred-MB download on the strength of
/// a measurement we could not take.
pub fn recommend(total_ram_bytes: Option<u64>, apple_silicon: Option<bool>) -> Recommendation {
    match apple_silicon {
        Some(false) => {
            return Recommendation {
                id: BALANCED_ID,
                reason: RecommendReason::NotAppleSilicon,
            }
        }
        None => {
            return Recommendation {
                id: BALANCED_ID,
                reason: RecommendReason::ArchUnknown,
            }
        }
        Some(true) => {}
    }
    match total_ram_bytes {
        Some(b) if b >= TURBO_DEFAULT_MIN_RAM_BYTES => Recommendation {
            id: SHARP_ID,
            reason: RecommendReason::FreshInstallAmpleRam,
        },
        Some(_) => Recommendation {
            id: BALANCED_ID,
            reason: RecommendReason::ModestRam,
        },
        None => Recommendation {
            id: BALANCED_ID,
            reason: RecommendReason::RamUnknown,
        },
    }
}

/// [`recommend`] over the cached machine profile.
pub fn recommend_for(profile: &MachineProfile) -> Recommendation {
    recommend(profile.total_ram_bytes, profile.apple_silicon)
}

/// The reason to show NEXT TO THE AUTO DEFAULT (`model::default_model_size`), which is
/// presence-first and therefore has its own branch structure. Pure over the two facts that decide
/// it plus the hardware reason.
///
/// - the auto default is the turbo size AND its file is already on disk ⇒ `AlreadyDownloaded`
///   (presence decided it — do NOT render the RAM-causal sentence);
/// - the auto default is the turbo size and the file is NOT on disk ⇒ `FreshInstallAmpleRam`
///   (branch 2 of `default_model_size`: fresh install, ample RAM — the one causal case);
/// - a non-turbo model is ALREADY on disk ⇒ `ExistingInstall` — install history, not hardware,
///   kept the default conservative;
/// - otherwise the HARDWARE reason carries through unchanged, so a machine that was capped for a
///   hardware reason keeps that exact reason instead of being relabelled as an install that has a
///   history it does not have.
///
/// `any_model_on_disk` is what separates the last two, and it is the whole point of the parameter:
/// without it a genuinely FRESH install on a modest Apple-Silicon Mac was reported as
/// `ExistingInstall` — an install-history claim about a machine with no install history.
pub fn auto_default_reason(
    auto_default_id: &str,
    turbo_already_on_disk: bool,
    any_model_on_disk: bool,
    hardware: RecommendReason,
) -> RecommendReason {
    if auto_default_id == SHARP_ID {
        return if turbo_already_on_disk {
            RecommendReason::AlreadyDownloaded
        } else {
            RecommendReason::FreshInstallAmpleRam
        };
    }
    // Claim install history ONLY when history is the sole cause. If the hardware would have capped
    // the default anyway, saying "you already have a model" denies a cause that is equally true —
    // the mirror image of the bug this parameter was added to fix. `FreshInstallAmpleRam` is the one
    // hardware answer that would NOT have capped, so it is exactly when history is doing the work.
    if any_model_on_disk && hardware == RecommendReason::FreshInstallAmpleRam {
        return RecommendReason::ExistingInstall;
    }
    hardware
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcribe::model::{
        is_live_heavy_model_file, model_filename, QUANT_MODEL_SIZES,
    };
    use std::path::Path;

    const GIB: u64 = 1024 * 1024 * 1024;

    /// RED-first, and the single most important assertion in this module: the recommendation is
    /// blind to the models dir. `default_model_size` (presence-first) says `small` for an install
    /// that already has `ggml-large-v3.bin`; `recommend` on the SAME 64 GiB Apple-Silicon machine
    /// says Sharp, because that is what the hardware deserves. Collapsing the two would make the
    /// badge lie on every existing install.
    #[test]
    fn recommendation_ignores_whats_on_disk() {
        assert_eq!(recommend(Some(64 * GIB), Some(true)).id, SHARP_ID);
        assert_eq!(
            crate::transcribe::model::default_model_size(&["ggml-large-v3.bin"], Some(64 * GIB)),
            BALANCED_ID,
            "the AUTO default stays presence-first — no existing install is surprised"
        );
    }

    /// Intel leg, on its own (never a conjunction — a conjunction pins neither leg).
    #[test]
    fn intel_caps_at_balanced_regardless_of_ram() {
        let r = recommend(Some(32 * GIB), Some(false));
        assert_eq!(r.id, BALANCED_ID);
        assert_eq!(r.reason, RecommendReason::NotAppleSilicon);
        // Even absurd RAM does not lift the cap.
        assert_eq!(recommend(Some(512 * GIB), Some(false)).id, BALANCED_ID);
    }

    /// Apple-Silicon leg, on its own — the SAME RAM figure as the Intel test above, so the two
    /// together prove the arch term is what moved the answer.
    #[test]
    fn apple_silicon_with_ample_ram_gets_sharp() {
        let r = recommend(Some(32 * GIB), Some(true));
        assert_eq!(r.id, SHARP_ID);
        assert_eq!(r.reason, RecommendReason::FreshInstallAmpleRam);
    }

    /// An UNREADABLE arch probe fails SMALL — never a large download on a measurement we could not
    /// take. Same fail direction as `default_model_size`'s unknown-RAM branch.
    ///
    /// It must reach the same SIZE as the Intel cap but a DIFFERENT reason. This matters on real
    /// hardware, not in theory: `hw.optional.arm64` does not exist on an Intel Mac, so the probe
    /// FAILS there rather than returning `0` — meaning `None` is the state a real Intel Mac lands
    /// in too, and a shared `IntelCap` reason would put the word "Intel" in front of an
    /// Apple-Silicon user whose probe merely failed.
    #[test]
    fn unreadable_arch_fails_balanced_without_claiming_a_chip() {
        let r = recommend(Some(64 * GIB), None);
        assert_eq!(r.id, BALANCED_ID);
        assert_eq!(r.reason, RecommendReason::ArchUnknown);
        assert_ne!(
            r.reason,
            RecommendReason::NotAppleSilicon,
            "an unreadable probe must never render chip-family copy"
        );
    }

    /// The RAM floor on Apple Silicon: inclusive at the floor, Balanced below it, Balanced when
    /// unknown.
    #[test]
    fn ram_floor_is_inclusive_and_unknown_fails_balanced() {
        assert_eq!(
            recommend(Some(TURBO_DEFAULT_MIN_RAM_BYTES), Some(true)).id,
            SHARP_ID
        );
        assert_eq!(
            recommend(Some(TURBO_DEFAULT_MIN_RAM_BYTES - 1), Some(true)).id,
            BALANCED_ID
        );
        let unknown = recommend(None, Some(true));
        assert_eq!(unknown.id, BALANCED_ID);
        assert_eq!(unknown.reason, RecommendReason::RamUnknown);
        // A MEASURED sub-floor machine is a different claim from an unmeasurable one.
        assert_eq!(
            recommend(Some(8 * GIB), Some(true)).reason,
            RecommendReason::ModestRam
        );
    }

    /// A genuinely FRESH install on a modest Apple-Silicon Mac must NOT be described with an
    /// install-history reason — it has no install history. Regression for the inverse of the
    /// causality bug: a hardware-caused decision wearing a history label.
    #[test]
    fn fresh_low_ram_install_reports_hardware_not_history() {
        let hardware = recommend(Some(8 * GIB), Some(true));
        assert_eq!(hardware.reason, RecommendReason::ModestRam);
        let shown = auto_default_reason(BALANCED_ID, false, false, hardware.reason);
        assert_eq!(shown, RecommendReason::ModestRam);
        assert_ne!(
            shown,
            RecommendReason::ExistingInstall,
            "a machine with an EMPTY models dir has no install history to cite"
        );
    }

    /// The RAM-causal sentence is emitted for EXACTLY ONE branch. A presence-first decision must
    /// never be dressed up as a hardware conclusion.
    #[test]
    fn auto_default_reason_only_claims_ram_causality_for_a_fresh_ample_install() {
        // Turbo already downloaded ⇒ presence, not RAM.
        assert_eq!(
            auto_default_reason(SHARP_ID, true, true, RecommendReason::FreshInstallAmpleRam),
            RecommendReason::AlreadyDownloaded
        );
        // Turbo chosen by branch 2 (fresh + ample RAM) ⇒ the one causal case.
        assert_eq!(
            auto_default_reason(SHARP_ID, false, false, RecommendReason::FreshInstallAmpleRam),
            RecommendReason::FreshInstallAmpleRam
        );
        // A non-turbo model ALREADY on disk on a machine whose hardware would NOT have capped ⇒
        // history alone decided it, so the history claim is true.
        assert_eq!(
            auto_default_reason(BALANCED_ID, false, true, RecommendReason::FreshInstallAmpleRam),
            RecommendReason::ExistingInstall
        );
        // …but when the HARDWARE would also have capped, history must not deny it. Both causes hold
        // on an 8 GiB Mac that already has `ggml-small.bin`, and the honest answer is the hardware
        // one — `ExistingInstall`'s copy explicitly says the machine could have run more.
        assert_eq!(
            auto_default_reason(BALANCED_ID, false, true, RecommendReason::ModestRam),
            RecommendReason::ModestRam
        );
        assert_eq!(
            auto_default_reason(BALANCED_ID, false, true, RecommendReason::ArchUnknown),
            RecommendReason::ArchUnknown
        );
        // A proven-Intel machine keeps its own reason rather than gaining a history it lacks.
        assert_eq!(
            auto_default_reason(BALANCED_ID, false, false, RecommendReason::NotAppleSilicon),
            RecommendReason::NotAppleSilicon
        );
        // An unreadable arch probe likewise keeps its own non-claiming reason.
        assert_eq!(
            auto_default_reason(BALANCED_ID, false, false, RecommendReason::ArchUnknown),
            RecommendReason::ArchUnknown
        );
    }

    /// The registry's `live_safe` flag and the shipped `is_live_heavy_model_file` classifier must
    /// agree for EVERY row — they gate different things (the delete guard vs the live-tick
    /// fallback) and a divergence would let one of them contradict the other.
    #[test]
    fn registry_live_safe_matches_classifier() {
        for m in all() {
            for lang in ["", "en", "pl"] {
                let file = model_filename(m.id, lang);
                assert_eq!(
                    m.live_safe,
                    !is_live_heavy_model_file(Path::new(&file)),
                    "id={} lang={lang} file={file}",
                    m.id
                );
            }
        }
    }

    /// Every registry id must be a size `model_filename` really resolves, and every quant size the
    /// downloader supports must appear in the registry — otherwise a user could hold a size the
    /// catalog cannot describe (the 9-vs-6 divergence this module replaces).
    #[test]
    fn registry_covers_every_supported_size_and_resolves_a_filename() {
        for m in all() {
            let file = model_filename(m.id, "");
            assert_eq!(file, format!("ggml-{}.bin", m.id), "id={}", m.id);
        }
        for quant in QUANT_MODEL_SIZES {
            assert!(
                model_by_id(quant).is_some(),
                "downloadable quant {quant} missing from the registry"
            );
        }
        for plain in ["tiny", "base", "small", "medium", "large-v3", "large-v3-turbo"] {
            assert!(model_by_id(plain).is_some(), "{plain} missing");
        }
    }

    /// The four rungs are exactly the four rungs, in ascending cost, and BOTH backend defaults are
    /// on the ladder. If `small` ever fell off, "Custom" would become the default display state for
    /// every sub-12-GiB Mac and every existing install.
    #[test]
    fn the_ladder_is_four_rungs_and_carries_both_backend_defaults() {
        let rungs: Vec<_> = all().iter().filter(|m| m.tier.is_some()).collect();
        assert_eq!(rungs.len(), 4);
        assert_eq!(
            rungs.iter().map(|m| m.id).collect::<Vec<_>>(),
            vec![LIGHT_ID, BALANCED_ID, SHARP_ID, MAXIMUM_ID]
        );
        assert!(
            rungs.windows(2).all(|w| w[0].power < w[1].power),
            "rungs must ascend by cost"
        );
        // The two sizes `default_model_size` can ever return.
        assert_eq!(tier_label(BALANCED_ID), Some("Balanced"));
        assert_eq!(
            tier_label(crate::transcribe::model::TURBO_DEFAULT_SIZE),
            Some("Sharp")
        );
        // A long-tail size has no tier; an unknown id has none either (the caller renders the raw id).
        assert_eq!(tier_label("medium"), None);
        assert_eq!(tier_label("not-a-model"), None);
        assert_eq!(tier_label(""), None);
    }

    /// `large-v3-q5_0` ships provisional + hidden precisely because its size is nowhere in this
    /// repo. A provisional row must never carry an invented figure, and must never be `visible`.
    #[test]
    fn provisional_rows_are_hidden_and_carry_no_invented_size() {
        let q5 = model_by_id("large-v3-q5_0").expect("registry row exists");
        assert!(q5.provisional);
        assert_eq!(q5.approx_download_bytes, None);
        assert_eq!(q5.approx_ram_bytes, None);
        assert!(!visible().any(|m| m.id == "large-v3-q5_0"));
        // Everything visible is non-provisional and states a download size.
        for m in visible() {
            assert!(!m.provisional, "id={}", m.id);
            assert!(
                m.approx_download_bytes.is_some(),
                "visible row {} must state a download size",
                m.id
            );
        }
        // Powers are unique so the display order is total.
        let mut powers: Vec<u8> = all().iter().map(|m| m.power).collect();
        powers.sort_unstable();
        powers.dedup();
        assert_eq!(powers.len(), all().len());
    }

    /// The RAM figures we DO ship must match the cited research (rounded UP), and every row that
    /// cites nothing must say `None` rather than a guess.
    #[test]
    fn ram_figures_match_the_cited_research() {
        // "small live (~0.9 GB)"
        assert_eq!(
            model_by_id(BALANCED_ID).unwrap().approx_ram_bytes,
            Some(9 * GB / 10)
        );
        // "turbo-q8_0 (~1.2–1.5 GB)" rounded UP to 1.5 GB.
        assert_eq!(
            model_by_id(SHARP_ID).unwrap().approx_ram_bytes,
            Some(3 * GB / 2)
        );
        // "large-v3 fp16 (~3.9 GB)"
        assert_eq!(
            model_by_id(MAXIMUM_ID).unwrap().approx_ram_bytes,
            Some(39 * GB / 10)
        );
        // Nothing in the research states a resident size for the long tail — so we state none.
        for id in ["tiny", LIGHT_ID, "small-q8_0", "medium", "medium-q8_0"] {
            assert_eq!(
                model_by_id(id).unwrap().approx_ram_bytes,
                None,
                "{id} must not invent a RAM figure"
            );
        }
    }
}
