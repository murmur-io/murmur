//! LIVE-CAPTION model readiness — the ONE resolution shared by every surface that needs it.
//!
//! WHY THIS MODULE EXISTS (the bug it closes): on a fresh ≥ 12 GB Mac
//! `transcribe::model::default_model_size` flips the BATCH default to the turbo quant, so
//! onboarding downloads exactly ONE file — `ggml-large-v3-turbo-q8_0.bin`. The LIVE tick is pinned
//! to `small` (`live_model_pin`) and DELIBERATELY never runs a medium/large-class encoder every 3 s
//! (T1.3 heat — a large live tick saturates the shared Metal GPU for the whole meeting). With only
//! the turbo model on disk, `live_pin_size` → `small` (absent) → `live_fallback_model` (nothing
//! live-safe) → the configured model is heavy ⇒ REFUSED, so the default install got NO live
//! captions and the only trace was a backend `warn!`. The heat policy is correct; the two things
//! missing were upstream:
//!
//!   1. **A live-safe companion download.** [`companion_size_for`] tells `download_model`
//!      (`commands/models.rs`) to ALSO fetch the pinned live size beside a heavy batch model, so the
//!      default path ends up with a working live model.
//!   2. **A user-visible STATE.** [`resolve`] classifies the same resolution `start_recording`
//!      performs, and `get_config` ships it to the FE as `AppConfigDto::live_captions` so the
//!      recorder can render a calm "live captions are off" notice instead of hiding the truth in a
//!      log line.
//!
//! `start_recording` consumes [`resolve`] too — ONE decision, so the state the UI shows and the
//! model the live tick actually loads can never drift apart.

use std::path::{Path, PathBuf};

use crate::settings::AppConfig;
use crate::transcribe::model::{
    effective_model_size, is_live_heavy_model_file, live_fallback_model_in, live_pin_size,
    model_filename, models_dir,
};

/// How the LIVE-caption tick's whisper model resolved — the outcome of the exact chain
/// `start_recording` walks. The four path-carrying variants (`Pinned` / `Fallback` / `Configured` /
/// `Unpinned`) mean captions RUN — which one it was only shapes the log line. The other three are
/// the "no live captions for this recording" states, split by CAUSE so the recorder UI can say
/// something true (a failed/absent companion download is recoverable by retrying it; a heavy live
/// PIN is a deliberate configuration).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LiveCaptions {
    /// The pinned live size is downloaded — the pin is satisfied verbatim.
    Pinned(PathBuf),
    /// The pinned size is absent, so the largest downloaded live-SAFE model is used instead
    /// (`live_fallback_model`).
    Fallback(PathBuf),
    /// Nothing live-safe is downloaded but the CONFIGURED model is itself live-safe, so the live
    /// tick rides it (pre-pin behavior; it may contend with the light reasoner).
    Configured(PathBuf),
    /// The pin is disabled (`live_model_pin == ""` and `brain_live` off) ⇒ the configured model is
    /// used verbatim, heavy or not (an explicit un-pin is the user's call).
    Unpinned(PathBuf),
    /// No whisper model at all on disk (a fresh install before onboarding's download). The FE's
    /// "Transcription model needed" banner owns this state — NOT the live-captions notice.
    NoModel,
    /// Whisper IS downloaded, but nothing LIVE-SAFE is: the pinned live size is absent and every
    /// model on disk is medium/large-class. That is the live-safe companion download never landing
    /// (offline during onboarding, an aborted/failed fetch) — recoverable by running it again.
    ModelMissing,
    /// The live PIN itself names a medium/large-class size that is NOT downloaded. We never
    /// auto-fetch a heavy model for the 3 s tick, so this is a configuration outcome rather than a
    /// failed download, and the notice must say so.
    PinnedHeavy,
}

impl LiveCaptions {
    /// The model the live tick should load, or `None` = no live captions for this recording.
    pub(crate) fn model_path(self) -> Option<PathBuf> {
        match self {
            Self::Pinned(p) | Self::Fallback(p) | Self::Configured(p) | Self::Unpinned(p) => {
                Some(p)
            }
            Self::NoModel | Self::ModelMissing | Self::PinnedHeavy => None,
        }
    }

    /// The FE-facing state string carried by `AppConfigDto::live_captions` (camelCase, mirrored by
    /// the recorder component). `""` is reserved for "not probed" — never returned here.
    pub(crate) fn dto_state(&self) -> &'static str {
        match self {
            Self::Pinned(_) | Self::Fallback(_) | Self::Configured(_) | Self::Unpinned(_) => {
                "ready"
            }
            Self::NoModel => "noModel",
            Self::ModelMissing => "modelMissing",
            Self::PinnedHeavy => "pinnedHeavy",
        }
    }
}

/// [`resolve_in`] over the REAL models dir + the live config. Any failure resolving the models dir
/// classifies [`LiveCaptions::NoModel`] (a broken probe must never claim captions are ready).
pub(crate) fn resolve(cfg: &AppConfig) -> LiveCaptions {
    let Ok(dir) = models_dir() else {
        return LiveCaptions::NoModel;
    };
    resolve_in(
        &dir,
        cfg.whisper_model_path.as_deref().map(Path::new),
        &cfg.model_size,
        cfg.language.as_deref().unwrap_or(""),
        &cfg.live_model_pin,
        cfg.brain_live,
    )
}

/// File-presence core of [`resolve`], factored over an explicit `dir` so every branch is testable
/// headless with a temp dir.
///
/// This MIRRORS `start_recording`'s chain exactly — pinned size → largest downloaded live-safe
/// model → a live-safe configured model → refuse. Keep the two in lockstep by keeping
/// `start_recording` calling [`resolve`]; the classification here is the only copy.
///
/// NOTE on a blank `model_size`: `effective_model_size` resolves the machine-conditional default,
/// which probes the REAL models dir. In production that IS `dir`; tests pass a concrete size.
pub(crate) fn resolve_in(
    dir: &Path,
    configured: Option<&Path>,
    model_size: &str,
    language: &str,
    live_model_pin: &str,
    brain_live: bool,
) -> LiveCaptions {
    // Mirrors `transcribe::model::resolve_model_path`: an existing explicit path wins, else the
    // size-derived file in the models dir.
    let configured_model = || -> Option<PathBuf> {
        if let Some(p) = configured.filter(|p| p.is_file()) {
            return Some(p.to_path_buf());
        }
        let derived = dir.join(model_filename(&effective_model_size(model_size), language));
        derived.is_file().then_some(derived)
    };

    let Some(pin) = live_pin_size(live_model_pin, brain_live) else {
        return match configured_model() {
            Some(p) => LiveCaptions::Unpinned(p),
            None => LiveCaptions::NoModel,
        };
    };

    let pin_file = model_filename(&pin, language);
    let pinned = dir.join(&pin_file);
    if pinned.is_file() {
        return LiveCaptions::Pinned(pinned);
    }
    if let Some(p) = live_fallback_model_in(dir, language) {
        return LiveCaptions::Fallback(p);
    }
    match configured_model() {
        Some(p) if !is_live_heavy_model_file(&p) => LiveCaptions::Configured(p),
        // Only medium/large-class models downloaded. Split the cause: a HEAVY pin was never a
        // download we would make on the user's behalf; a live-safe pin means the companion download
        // is simply missing.
        Some(_) if is_live_heavy_model_file(Path::new(&pin_file)) => LiveCaptions::PinnedHeavy,
        Some(_) => LiveCaptions::ModelMissing,
        None => LiveCaptions::NoModel,
    }
}

/// Whether a whisper model download should ALSO fetch a live-SAFE companion model, and which SIZE.
/// `batch_model` is the file the batch download just produced (`ensure_model`'s return — the real
/// on-disk model, so a custom `whisper_model_path` is covered without re-deriving it).
///
/// `None` (nothing to fetch) when:
///   - the live pin is DISABLED (`""` + `brain_live` off) — the live tick then uses the configured
///     model verbatim, so a companion would never be consulted;
///   - the pin itself names a medium/large-class size — we never fetch a heavy model for the 3 s
///     tick, and an explicit heavy pin is the user's own configuration;
///   - the pinned model is already on disk;
///   - ANY live-safe model is already on disk (`live_fallback_model_in` — the live tick has
///     something to run);
///   - the batch model is ITSELF live-safe (a `small`/`base`/`tiny` selection, or an unrecognizable
///     custom `whisper_model_path`, which `is_live_heavy_model_file` classifies live-safe to
///     preserve the pre-pin behavior) — one download already covers both roles.
///
/// `Some(size)` = the pinned live size (default `small`, ~470 MB; an explicit live-safe pin such as
/// `base`/`tiny`/`small-q8_0` is honored verbatim) must be fetched beside the heavy batch model.
pub(crate) fn companion_size_for(
    dir: &Path,
    batch_model: &Path,
    language: &str,
    live_model_pin: &str,
    brain_live: bool,
) -> Option<String> {
    companion_size_lazy(
        dir,
        || batch_model.to_path_buf(),
        language,
        live_model_pin,
        brain_live,
    )
}

/// [`companion_size_for`] with the batch model resolved LAZILY. Every cheap check (the pin's own
/// class, the pinned file, any downloaded live-safe model) runs first, so [`companion_pending_in`]
/// — which must DERIVE the batch filename, and for a blank `model_size` pays
/// `effective_model_size` → `default_model_size_now` (a `read_dir` + a `sysctl` subprocess) — only
/// pays for it on a machine where the answer actually hinges on it.
fn companion_size_lazy(
    dir: &Path,
    batch_model: impl FnOnce() -> PathBuf,
    language: &str,
    live_model_pin: &str,
    brain_live: bool,
) -> Option<String> {
    let pin = live_pin_size(live_model_pin, brain_live)?;
    let pin_file = model_filename(&pin, language);
    if is_live_heavy_model_file(Path::new(&pin_file)) {
        return None;
    }
    if dir.join(&pin_file).is_file() {
        return None;
    }
    if live_fallback_model_in(dir, language).is_some() {
        return None;
    }
    if !is_live_heavy_model_file(&batch_model()) {
        return None;
    }
    Some(pin)
}

/// [`companion_size_for`] over the REAL models dir. `None` on any dir-resolution failure (a broken
/// probe must never trigger an unrequested download).
pub(crate) fn companion_size(
    batch_model: &Path,
    language: &str,
    live_model_pin: &str,
    brain_live: bool,
) -> Option<String> {
    let dir = models_dir().ok()?;
    companion_size_for(&dir, batch_model, language, live_model_pin, brain_live)
}

/// Would a `download_model` run right now ALSO fetch a live-safe companion? The SAME decision
/// [`companion_size_for`] makes, asked BEFORE the batch download, against the file that download
/// would produce — so the onboarding wizard can disclose the extra transfer from the backend
/// instead of re-deriving half the rule in TypeScript (where it drifted: the FE copy knew only the
/// size-name test, not the pin/presence terms, and promised a fetch the backend would skip).
fn companion_pending_in(
    dir: &Path,
    configured: Option<&Path>,
    model_size: &str,
    language: &str,
    live_model_pin: &str,
    brain_live: bool,
) -> bool {
    companion_size_lazy(
        dir,
        // Mirrors `ensure_model`'s own resolution: an existing explicit path is used verbatim,
        // otherwise the size-derived file lands in the models dir.
        || match configured.filter(|p| p.is_file()) {
            Some(p) => p.to_path_buf(),
            None => dir.join(model_filename(&effective_model_size(model_size), language)),
        },
        language,
        live_model_pin,
        brain_live,
    )
    .is_some()
}

/// The two DISPLAY-ONLY facts `get_config` ships to the FE, resolved over ONE models-dir lookup:
/// the live-caption readiness state (`AppConfigDto::live_captions`, the recorder's notice) and
/// whether a model download would also fetch the live-safe companion
/// (`AppConfigDto::live_companion_pending`, the onboarding disclosure).
///
/// A models-dir failure reads as [`LiveCaptions::NoModel`] + no pending companion — a broken probe
/// must never claim captions are ready, nor promise a download.
pub(crate) fn dto_probe(cfg: &AppConfig) -> (String, bool) {
    let Ok(dir) = models_dir() else {
        return (LiveCaptions::NoModel.dto_state().to_string(), false);
    };
    let configured = cfg.whisper_model_path.as_deref().map(Path::new);
    let language = cfg.language.as_deref().unwrap_or("");
    let state = resolve_in(
        &dir,
        configured,
        &cfg.model_size,
        language,
        &cfg.live_model_pin,
        cfg.brain_live,
    );
    let pending = companion_pending_in(
        &dir,
        configured,
        &cfg.model_size,
        language,
        &cfg.live_model_pin,
        cfg.brain_live,
    );
    (state.dto_state().to_string(), pending)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A unique temp models dir per fixture (the real app-data dir must never be touched).
    fn tmp_models_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "murmur-live-captions-{tag}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(dir: &Path, name: &str) {
        std::fs::write(dir.join(name), b"MODEL-BYTES").unwrap();
    }

    /// The turbo file the T2 default flip downloads on a fresh ≥ 12 GB Mac.
    const TURBO: &str = "ggml-large-v3-turbo-q8_0.bin";

    /// RED — THE REPORTED DEFECT, at the classification level: a fresh ≥ 12 GB install whose ONLY
    /// whisper model is the turbo default has NO live captions, and the cause is a MISSING live-safe
    /// companion download (not a deliberate heavy pin). Before the fix this state existed but was
    /// nameless — a backend `warn!` and nothing the UI could render.
    #[test]
    fn fresh_turbo_only_install_is_model_missing_not_ready() {
        let dir = tmp_models_dir("turbo-only");
        touch(&dir, TURBO);

        let state = resolve_in(
            &dir,
            None,
            "large-v3-turbo-q8_0",
            "",
            "small", // the serde-default live pin
            false,
        );
        assert_eq!(state, LiveCaptions::ModelMissing);
        assert_eq!(state.dto_state(), "modelMissing");
        assert_eq!(state.model_path(), None, "the live tick must NOT run turbo");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The fix's other half: with the live-safe companion on disk the SAME install resolves Ready —
    /// pinned exactly when the pinned size landed, Fallback when a different live-safe size did.
    #[test]
    fn companion_on_disk_makes_the_same_install_ready() {
        let dir = tmp_models_dir("companion");
        touch(&dir, TURBO);

        // The pinned `small` landed ⇒ the pin is satisfied verbatim.
        touch(&dir, "ggml-small.bin");
        let ready = resolve_in(&dir, None, "large-v3-turbo-q8_0", "", "small", false);
        assert_eq!(ready, LiveCaptions::Pinned(dir.join("ggml-small.bin")));
        assert_eq!(ready.dto_state(), "ready");

        // Only `base` landed (a lighter explicit companion) ⇒ the absent-pin FALLBACK, still ready.
        let dir2 = tmp_models_dir("companion-base");
        touch(&dir2, TURBO);
        touch(&dir2, "ggml-base.bin");
        let fb = resolve_in(&dir2, None, "large-v3-turbo-q8_0", "", "small", false);
        assert_eq!(fb, LiveCaptions::Fallback(dir2.join("ggml-base.bin")));
        assert_eq!(fb.dto_state(), "ready");

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    /// A live-SAFE batch selection needs no companion at all: the one downloaded model IS the live
    /// model (the pinned size itself, or a live-safe size the absent-pin fallback picks up).
    #[test]
    fn live_safe_batch_selection_is_ready() {
        let dir = tmp_models_dir("small-batch");
        touch(&dir, "ggml-small.bin");
        // `small` IS the pinned size here, so it resolves through the Pinned arm.
        assert_eq!(
            resolve_in(&dir, None, "small", "", "small", false),
            LiveCaptions::Pinned(dir.join("ggml-small.bin"))
        );

        // A tiny-only install with the default `small` pin: the pin is absent, but tiny is
        // live-safe ⇒ Fallback (ready).
        let dir2 = tmp_models_dir("tiny-batch");
        touch(&dir2, "ggml-tiny.bin");
        assert_eq!(
            resolve_in(&dir2, None, "tiny", "", "small", false),
            LiveCaptions::Fallback(dir2.join("ggml-tiny.bin"))
        );

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    /// A DELIBERATELY heavy live pin that isn't downloaded is `PinnedHeavy`, not `ModelMissing` —
    /// the two get different notice copy (a failed companion download vs a configuration choice).
    #[test]
    fn heavy_pin_absent_classifies_pinned_heavy() {
        let dir = tmp_models_dir("heavy-pin");
        touch(&dir, TURBO);
        let state = resolve_in(&dir, None, "large-v3-turbo-q8_0", "", "medium", false);
        assert_eq!(state, LiveCaptions::PinnedHeavy);
        assert_eq!(state.dto_state(), "pinnedHeavy");

        // …and a heavy pin that IS downloaded still runs (the pin wins unconditionally — T1.3).
        touch(&dir, "ggml-medium.bin");
        assert_eq!(
            resolve_in(&dir, None, "large-v3-turbo-q8_0", "", "medium", false),
            LiveCaptions::Pinned(dir.join("ggml-medium.bin"))
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An empty pin + `brain_live` off = the pre-pin behavior: the configured model verbatim, heavy
    /// or not. That is Ready (captions DO run), and it is exactly why no companion is fetched then.
    #[test]
    fn unpinned_uses_the_configured_model_verbatim() {
        let dir = tmp_models_dir("unpinned");
        touch(&dir, TURBO);
        let state = resolve_in(&dir, None, "large-v3-turbo-q8_0", "", "", false);
        assert_eq!(state, LiveCaptions::Unpinned(dir.join(TURBO)));
        assert_eq!(state.dto_state(), "ready");

        // The LEGACY `brain_live` pin-to-small still applies with an empty pin — and with only the
        // heavy turbo on disk that lands back in the missing-companion state.
        assert_eq!(
            resolve_in(&dir, None, "large-v3-turbo-q8_0", "", "", true),
            LiveCaptions::ModelMissing
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A CUSTOM `whisper_model_path` (the explicit-path users): an unrecognizable name classifies
    /// live-safe (preserving the pre-pin behavior) ⇒ Ready via the configured arm; a heavy custom
    /// file is refused for the live tick like any other heavy model.
    #[test]
    fn custom_model_path_live_safe_is_ready_heavy_is_missing() {
        let dir = tmp_models_dir("custom");
        let custom = dir.join("my-tuned-model.bin");
        std::fs::write(&custom, b"MODEL-BYTES").unwrap();
        let state = resolve_in(&dir, Some(&custom), "", "", "small", false);
        assert_eq!(state, LiveCaptions::Configured(custom.clone()));
        assert_eq!(state.dto_state(), "ready");

        let heavy = dir.join("my-large-v3-finetune.bin");
        std::fs::write(&heavy, b"MODEL-BYTES").unwrap();
        assert_eq!(
            resolve_in(&dir, Some(&heavy), "", "", "small", false),
            LiveCaptions::ModelMissing
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Nothing downloaded at all ⇒ `NoModel`: the FE's transcription-model download banner owns
    /// that state, so the live-captions notice must NOT also fire.
    #[test]
    fn empty_models_dir_is_no_model() {
        let dir = tmp_models_dir("empty");
        let state = resolve_in(&dir, None, "small", "", "small", false);
        assert_eq!(state, LiveCaptions::NoModel);
        assert_eq!(state.dto_state(), "noModel");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// English resolves the `.en` build for the pin (mirroring `model_filename`), and rides a
    /// multilingual build of a live-safe size through the fallback when no `.en` build exists.
    #[test]
    fn english_pin_resolves_en_build_then_multilingual_fallback() {
        let dir = tmp_models_dir("en");
        touch(&dir, TURBO);
        touch(&dir, "ggml-small.bin");
        // No `ggml-small.en.bin` ⇒ the pin file is absent, but the fallback rides multilingual.
        assert_eq!(
            resolve_in(&dir, None, "large-v3-turbo-q8_0", "en", "small", false),
            LiveCaptions::Fallback(dir.join("ggml-small.bin"))
        );
        touch(&dir, "ggml-small.en.bin");
        assert_eq!(
            resolve_in(&dir, None, "large-v3-turbo-q8_0", "en", "small", false),
            LiveCaptions::Pinned(dir.join("ggml-small.en.bin"))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ── the companion-download decision ────────────────────────────────────────────────────────

    /// RED — the ROOT CAUSE: a heavy batch download with no live-safe model on disk must ALSO fetch
    /// the pinned live size. Before the fix `download_model` fetched exactly one file and the
    /// default install shipped without live captions.
    #[test]
    fn heavy_batch_download_fetches_the_pinned_companion() {
        let dir = tmp_models_dir("comp-turbo");
        let batch = dir.join(TURBO);
        touch(&dir, TURBO);
        assert_eq!(
            companion_size_for(&dir, &batch, "", "small", false).as_deref(),
            Some("small")
        );
        // An explicit lighter/quant live pin is honored verbatim.
        assert_eq!(
            companion_size_for(&dir, &batch, "", "base", false).as_deref(),
            Some("base")
        );
        assert_eq!(
            companion_size_for(&dir, &batch, "pl", "tiny", false).as_deref(),
            Some("tiny")
        );
        assert_eq!(
            companion_size_for(&dir, &batch, "", "small-q8_0", false).as_deref(),
            Some("small-q8_0")
        );
        // An empty pin + brain_live still pins small (the legacy D1 guarantee) ⇒ fetch it.
        assert_eq!(
            companion_size_for(&dir, &batch, "", "", true).as_deref(),
            Some("small")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// No companion when it would be pointless or unrequested: a live-safe batch model, a live-safe
    /// model already on disk, the pinned model already on disk, a disabled pin, or a heavy pin.
    #[test]
    fn no_companion_when_unnecessary_or_unrequested() {
        // (a) a live-safe batch SELECTION: what landed in the models dir is itself live-safe, so the
        //     pinned/fallback presence checks already say "nothing to fetch". (The `batch_model` is
        //     live-safe here too — that branch matters for a model OUTSIDE the models dir, i.e. a
        //     custom `whisper_model_path`, which `companion_decision_covers_a_custom_model_path`
        //     covers.)
        let dir = tmp_models_dir("comp-small");
        let small = dir.join("ggml-small.bin");
        touch(&dir, "ggml-small.bin");
        assert_eq!(companion_size_for(&dir, &small, "", "small", false), None);
        let base = dir.join("ggml-base.bin");
        touch(&dir, "ggml-base.bin");
        assert_eq!(companion_size_for(&dir, &base, "", "small", false), None);

        // (b) heavy batch, but the PINNED model is already downloaded.
        let dir2 = tmp_models_dir("comp-pinned");
        let batch2 = dir2.join(TURBO);
        touch(&dir2, TURBO);
        touch(&dir2, "ggml-small.bin");
        assert_eq!(companion_size_for(&dir2, &batch2, "", "small", false), None);

        // (c) heavy batch, a DIFFERENT live-safe model already downloaded (the live tick has
        //     something to run via the fallback) ⇒ no second download.
        let dir3 = tmp_models_dir("comp-fallback");
        let batch3 = dir3.join(TURBO);
        touch(&dir3, TURBO);
        touch(&dir3, "ggml-tiny.bin");
        assert_eq!(companion_size_for(&dir3, &batch3, "", "small", false), None);

        // (d) the pin is DISABLED ⇒ the live tick uses the configured model verbatim; a companion
        //     would never be consulted.
        let dir4 = tmp_models_dir("comp-unpinned");
        let batch4 = dir4.join(TURBO);
        touch(&dir4, TURBO);
        assert_eq!(companion_size_for(&dir4, &batch4, "", "", false), None);

        // (e) the pin is itself HEAVY ⇒ the user's explicit configuration, never an auto-download.
        assert_eq!(
            companion_size_for(&dir4, &batch4, "", "medium", false),
            None
        );
        assert_eq!(
            companion_size_for(&dir4, &batch4, "", "large-v3-turbo-q8_0", false),
            None
        );

        for d in [&dir, &dir2, &dir3, &dir4] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    /// The CUSTOM `whisper_model_path` case for the companion decision (`batch_model` is whatever
    /// `ensure_model` returned, which for a configured path is that path verbatim): an
    /// unrecognizable custom name classifies live-safe ⇒ no companion; a heavy custom name ⇒ fetch
    /// the pinned live-safe size.
    #[test]
    fn companion_decision_covers_a_custom_model_path() {
        let dir = tmp_models_dir("comp-custom");
        let elsewhere = tmp_models_dir("comp-custom-src");

        let custom = elsewhere.join("my-tuned-model.bin");
        std::fs::write(&custom, b"MODEL-BYTES").unwrap();
        assert_eq!(
            companion_size_for(&dir, &custom, "", "small", false),
            None,
            "an unrecognizable custom model classifies live-safe (pre-pin behavior preserved)"
        );

        let heavy_custom = elsewhere.join("ggml-medium-finetune.bin");
        std::fs::write(&heavy_custom, b"MODEL-BYTES").unwrap();
        assert_eq!(
            companion_size_for(&dir, &heavy_custom, "", "small", false).as_deref(),
            Some("small"),
            "a heavy custom model still leaves the live tick with nothing to run"
        );

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&elsewhere);
    }

    /// The ONBOARDING disclosure ("we fetch a small live-caption model alongside this one") is the
    /// SAME decision the download makes — asked BEFORE the download, against the file it would
    /// produce. RED for the drift the FE copy had: it keyed on the size NAME alone, so it promised a
    /// companion whenever the size looked heavy, even when the pin was disabled/heavy or a live-safe
    /// model was already on disk (cases b–e below), and it could not see a heavy CUSTOM path at all.
    #[test]
    fn companion_pending_matches_the_download_decision() {
        // (a) the reported default: a heavy batch size, nothing live-safe downloaded ⇒ disclose.
        let dir = tmp_models_dir("pending-turbo");
        assert!(companion_pending_in(
            &dir,
            None,
            "large-v3-turbo-q8_0",
            "",
            "small",
            false
        ));
        // …and the promise is kept: the download does fetch exactly that, and once it lands the
        // disclosure goes away rather than lying about a second transfer.
        touch(&dir, TURBO);
        let batch = dir.join(TURBO);
        assert_eq!(
            companion_size_for(&dir, &batch, "", "small", false).as_deref(),
            Some("small")
        );
        touch(&dir, "ggml-small.bin");
        assert!(!companion_pending_in(
            &dir,
            None,
            "large-v3-turbo-q8_0",
            "",
            "small",
            false
        ));

        // (b) a live-SAFE batch size ⇒ one download covers both roles, nothing to disclose.
        let dir2 = tmp_models_dir("pending-small");
        assert!(!companion_pending_in(&dir2, None, "small", "", "small", false));

        // (c) a heavy batch size but a live-safe model already on disk ⇒ no second transfer.
        let dir3 = tmp_models_dir("pending-have-tiny");
        touch(&dir3, "ggml-tiny.bin");
        assert!(!companion_pending_in(&dir3, None, "large-v3", "", "small", false));

        // (d) the pin is DISABLED, and (e) the pin is itself heavy ⇒ never an auto-fetch, so the
        //     wizard must not promise one even though the batch size is heavy.
        let dir4 = tmp_models_dir("pending-pin");
        assert!(!companion_pending_in(&dir4, None, "large-v3", "", "", false));
        assert!(!companion_pending_in(&dir4, None, "large-v3", "", "medium", false));

        // (f) a CUSTOM `whisper_model_path`: an unrecognizable name is live-safe ⇒ nothing to
        //     disclose; a heavy custom file still leaves the live tick with nothing ⇒ disclose.
        let dir5 = tmp_models_dir("pending-custom");
        let custom = dir5.join("my-tuned-model.bin");
        std::fs::write(&custom, b"MODEL-BYTES").unwrap();
        assert!(!companion_pending_in(
            &dir5,
            Some(&custom),
            "",
            "",
            "small",
            false
        ));
        let heavy = dir5.join("my-large-v3-finetune.bin");
        std::fs::write(&heavy, b"MODEL-BYTES").unwrap();
        assert!(companion_pending_in(
            &dir5,
            Some(&heavy),
            "",
            "",
            "small",
            false
        ));

        for d in [&dir, &dir2, &dir3, &dir4, &dir5] {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    /// The DTO carrying this state is DISPLAY-ONLY: the pure `config_to_dto` leaves it `""` (so it
    /// never probes the disk, and every existing config round-trip test is untouched), a settings
    /// save can neither set nor clear it, and an older FE payload omitting `liveCaptions`
    /// deserializes cleanly (`#[serde(default)]`).
    #[test]
    fn dto_state_is_display_only_and_omittable() {
        let cfg = AppConfig::default();
        let dto = super::super::config_to_dto(&cfg);
        assert_eq!(
            dto.live_captions, "",
            "the pure DTO conversion must not probe the disk"
        );
        assert!(
            !dto.live_companion_pending,
            "the pure DTO conversion must not probe the disk"
        );

        // A hostile/stale FE payload cannot persist or spoof readiness.
        let mut incoming = super::super::config_to_dto(&cfg);
        incoming.live_captions = "ready".to_string();
        incoming.live_companion_pending = true;
        let merged = super::super::dto_to_config(incoming, &cfg);
        let round_tripped = super::super::config_to_dto(&merged);
        assert_eq!(round_tripped.live_captions, "");
        assert!(!round_tripped.live_companion_pending);

        // Serialized as camelCase, and omittable by an older FE.
        let json = serde_json::to_string(&super::super::config_to_dto(&cfg)).unwrap();
        let mut v: serde_json::Value = serde_json::from_str(&json).unwrap();
        for key in ["liveCaptions", "liveCompanionPending"] {
            assert!(
                v.as_object_mut().unwrap().remove(key).is_some(),
                "DTO must serialize {key}"
            );
        }
        let old: super::super::AppConfigDto = serde_json::from_value(v).unwrap();
        assert_eq!(old.live_captions, "");
        assert!(!old.live_companion_pending);
    }

    /// The companion decision and the readiness classification agree: fetching the size
    /// [`companion_size_for`] returns turns a `ModelMissing` install into a `Ready` one. This is the
    /// property that makes the fix complete rather than merely plausible.
    #[test]
    fn fetching_the_companion_flips_model_missing_to_ready() {
        for (pin, lang) in [("small", ""), ("base", "pl"), ("tiny", "en")] {
            let dir = tmp_models_dir("flip");
            let batch = dir.join(TURBO);
            touch(&dir, TURBO);

            assert_eq!(
                resolve_in(&dir, None, "large-v3-turbo-q8_0", lang, pin, false),
                LiveCaptions::ModelMissing,
                "pin={pin} lang={lang}"
            );
            let size = companion_size_for(&dir, &batch, lang, pin, false)
                .expect("a heavy-only install must want a companion");
            assert_eq!(size, pin);
            // Simulate the download landing.
            touch(&dir, &model_filename(&size, lang));

            let after = resolve_in(&dir, None, "large-v3-turbo-q8_0", lang, pin, false);
            assert_eq!(after.dto_state(), "ready", "pin={pin} lang={lang}");
            assert!(after.model_path().is_some());
            // …and a second download run wants nothing more.
            assert_eq!(companion_size_for(&dir, &batch, lang, pin, false), None);

            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}
