//! LIVE-caption ASR seam — the engine-agnostic decode contract the live loop consumes.
//!
//! The live caption tick decodes a rolling 16 kHz window into a [`Transcript`] every few seconds.
//! Historically that was ALWAYS whisper's Fast profile. This seam lets an OPTIONAL alternative
//! engine take the LIVE path — currently NVIDIA parakeet-tdt-0.6b-v3 int8 on CPU
//! ([`crate::transcribe::parakeet::ParakeetAsr`]), which runs OFF the Metal GPU so the shared
//! GPU is free for the brain LLM. Whisper stays the BATCH authority: the post-Stop Accurate
//! pipeline (`pipeline.rs`) is UNTOUCHED, and the wake-word listener + manual voice-capture stay
//! on the whisper Fast path (they need whisper's short-clip language forcing).
//!
//! Contract: `transcribe_live(samples_16k, lang) -> Result<Transcript>` — 16 kHz mono f32 in, a
//! decoded `Transcript` out. The live loop only ever reads `Transcript::full_text` / the joined
//! `segments` text, so a single-segment transcript (what parakeet returns) is consumed identically
//! to whisper's multi-segment output.
//!
//! FALL-BACK DISCIPLINE (captions must never hard-fail): [`build_live_asr`] returns the whisper
//! transcriber whenever parakeet is not selected, its models are absent, or its recognizer fails
//! to load — logged ONCE, never an error to the caller.

use std::path::Path;
use std::sync::Arc;

use crate::error::Result;
use crate::transcribe::types::Transcript;
use crate::transcribe::whisper::Transcriber;

/// The config value that SELECTS the live ASR engine (`AppConfig::live_asr_engine`).
/// `"whisper"` (default) or `"parakeet"`. Any other value falls back to whisper.
pub const ENGINE_WHISPER: &str = "whisper";
pub const ENGINE_PARAKEET: &str = "parakeet";

/// The engine-agnostic LIVE decode contract. Object-safe so the live loop can hold a
/// `Box<dyn LiveAsr>` chosen at record-start from config, and swap engines without touching the
/// tick body. `Send + Sync` so it can live on the caption thread (both impls are `Send + Sync`).
pub trait LiveAsr: Send + Sync {
    /// Decode 16 kHz mono f32 `samples_16k` into a [`Transcript`]. `lang = Some("pl")` forces a
    /// language; `None` = auto-detect / auto-LID. Never panics; a decode failure is an `Err`.
    fn transcribe_live(&self, samples_16k: &[f32], lang: Option<&str>) -> Result<Transcript>;

    /// A coarse, non-PII engine label for `live_perf` telemetry (never a path). `"whisper"` /
    /// `"parakeet"`.
    fn engine_label(&self) -> &'static str;
}

/// Whisper takes the live path via its existing Fast profile (`Transcriber::transcribe` =
/// `transcribe_with(.., Fast)`) — byte-for-byte the pre-seam behavior, so selecting `"whisper"`
/// (the default) is a no-op change.
impl LiveAsr for Transcriber {
    fn transcribe_live(&self, samples_16k: &[f32], lang: Option<&str>) -> Result<Transcript> {
        self.transcribe(samples_16k, lang)
    }

    fn engine_label(&self) -> &'static str {
        ENGINE_WHISPER
    }
}

/// The live loop holds the whisper transcriber in an `Arc` (the wake/manual-capture paths need a
/// `&Transcriber` handle while the caption decode goes through the boxed `LiveAsr`). This impl lets
/// that SAME `Arc<Transcriber>` also be the caption engine on the whisper fall-back path — one
/// loaded model, two consumers, no double-load.
impl LiveAsr for Arc<Transcriber> {
    fn transcribe_live(&self, samples_16k: &[f32], lang: Option<&str>) -> Result<Transcript> {
        self.as_ref().transcribe(samples_16k, lang)
    }

    fn engine_label(&self) -> &'static str {
        ENGINE_WHISPER
    }
}

/// PURE selection decision — which engine the live loop should use, given the configured
/// `engine` string and whether the parakeet models are present on disk. Factored out so the
/// fall-back matrix is headless-testable without loading any model:
///
/// - `"parakeet"` AND models present ⇒ `true` (use parakeet).
/// - `"parakeet"` AND models ABSENT ⇒ `false` (fall back to whisper; the caller logs it once).
/// - anything else (`"whisper"`, `""`, an unknown value) ⇒ `false` (whisper).
///
/// Case-insensitive + trimmed so a stray `"Parakeet "` still selects. NEVER returns `true` when
/// the models are absent, so a mis-set config can never wedge the loop with a dead engine.
pub fn should_use_parakeet(engine: &str, parakeet_present: bool) -> bool {
    engine.trim().eq_ignore_ascii_case(ENGINE_PARAKEET) && parakeet_present
}

/// Build the LIVE ASR engine for this recording from config. The whisper `Transcriber` is ALWAYS
/// loaded (it is the batch-quality Fast engine AND the fall-back), then:
///
/// - if `engine == "parakeet"` and its models are present, try to load [`ParakeetAsr`]; on success
///   the caption decode runs on the CPU-only parakeet engine (Metal freed for the brain);
/// - on ANY parakeet miss (not selected, models absent, or a load error) return the whisper
///   `Transcriber` boxed as the `LiveAsr` — a one-time `info`/`warn` log, NEVER a hard failure.
///
/// The returned boxed engine drives ONLY the caption decode; the wake/manual-capture paths keep a
/// direct `&Transcriber` handle via the SAME `whisper` `Arc` (they need whisper's short-clip
/// language forcing). On the whisper fall-back path the box just re-shares that Arc (no double-
/// load); on the parakeet path the box owns a separate CPU recognizer while whisper stays live for
/// the wake/manual paths.
///
/// RAM guard (parity with the reasoner's whisper-large refuse): a parakeet load is refused when
/// total RAM is affirmatively below the floor (`model::parakeet_ram_permits_now`) — the loop then
/// uses whisper for captions, logged once. A broken RAM probe fails OPEN (never refuse on a broken
/// measurement).
pub fn build_live_asr(engine: &str, whisper: Arc<Transcriber>) -> Box<dyn LiveAsr> {
    if !should_use_parakeet(engine, crate::transcribe::model::parakeet_models_present()) {
        if engine.trim().eq_ignore_ascii_case(ENGINE_PARAKEET) {
            tracing::info!(
                target: "live",
                "parakeet live engine selected but models not downloaded; using whisper for captions"
            );
        }
        return Box::new(whisper);
    }
    if !crate::transcribe::model::parakeet_ram_permits_now() {
        tracing::warn!(
            target: "live",
            "parakeet live engine refused under memory pressure; using whisper for captions"
        );
        return Box::new(whisper);
    }
    match crate::transcribe::model::parakeet_model_paths() {
        Ok(paths) => match crate::transcribe::parakeet::ParakeetAsr::load(&paths) {
            Ok(p) => {
                tracing::info!(target: "live", "live captions using the parakeet (CPU) engine");
                Box::new(p)
            }
            Err(e) => {
                tracing::warn!(target: "live", error = %e, "parakeet load failed; using whisper for captions");
                Box::new(whisper)
            }
        },
        Err(e) => {
            tracing::warn!(target: "live", error = %e, "parakeet model paths unresolved; using whisper for captions");
            Box::new(whisper)
        }
    }
}

/// The four resolved parakeet model files (encoder/decoder/joiner int8 ONNX + tokens.txt), all
/// verified present by [`crate::transcribe::model::parakeet_model_paths`]. A plain owned bundle so
/// [`crate::transcribe::parakeet::ParakeetAsr::load`] takes one argument.
#[derive(Clone, Debug)]
pub struct ParakeetModelPaths {
    pub encoder: std::path::PathBuf,
    pub decoder: std::path::PathBuf,
    pub joiner: std::path::PathBuf,
    pub tokens: std::path::PathBuf,
}

impl ParakeetModelPaths {
    /// Every file is a real file on disk. Guards `ParakeetAsr::load` against a half-present bundle.
    pub fn all_present(&self) -> bool {
        [&self.encoder, &self.decoder, &self.joiner, &self.tokens]
            .iter()
            .all(|p| Path::new(p).is_file())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The engine-selection matrix (pure): parakeet ONLY when selected AND present; every other
    /// combination (absent models, whisper, empty, unknown) resolves whisper — so a mis-set config
    /// can never leave the loop with a dead engine.
    #[test]
    fn parakeet_selected_only_when_configured_and_present() {
        assert!(should_use_parakeet("parakeet", true));
        assert!(!should_use_parakeet("parakeet", false), "selected but absent → whisper");
        assert!(!should_use_parakeet("whisper", true));
        assert!(!should_use_parakeet("whisper", false));
        assert!(!should_use_parakeet("", true), "empty → whisper default");
        assert!(!should_use_parakeet("something-else", true));
    }

    /// Case/whitespace-insensitive selection: a stored `" Parakeet "` still selects when present.
    #[test]
    fn parakeet_selection_is_trimmed_and_case_insensitive() {
        assert!(should_use_parakeet("  Parakeet  ", true));
        assert!(should_use_parakeet("PARAKEET", true));
        assert!(!should_use_parakeet("  Parakeet  ", false));
    }

    /// `all_present` is false unless every one of the four files exists (a half-present bundle is
    /// treated as absent — the load-guard contract).
    #[test]
    fn parakeet_paths_all_present_requires_every_file() {
        let dir = std::env::temp_dir().join(format!("murmur-parakeet-paths-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = ParakeetModelPaths {
            encoder: dir.join("encoder.int8.onnx"),
            decoder: dir.join("decoder.int8.onnx"),
            joiner: dir.join("joiner.int8.onnx"),
            tokens: dir.join("tokens.txt"),
        };
        assert!(!paths.all_present(), "nothing on disk → absent");
        for p in [&paths.encoder, &paths.decoder, &paths.joiner] {
            std::fs::write(p, b"x").unwrap();
        }
        assert!(!paths.all_present(), "three of four → still absent");
        std::fs::write(&paths.tokens, b"x").unwrap();
        assert!(paths.all_present(), "all four → present");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
