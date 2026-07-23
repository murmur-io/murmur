//! The on-device HEAVY engine as a [`SummarizerProvider`] (spec §3.2, P3) — lets the Notes/Ask roles
//! be served FULLY LOCALLY under the Fully-Local posture. Wraps a shared local [`LocalReasoner`]
//! (whose weights live in the process-global mistral MODEL_CACHE, loaded once) and bridges the sync
//! reasoner to the async trait via [`crate::perf::run_heavy`] — the shared "one heavy inference at
//! a time" gate, not a bare `spawn_blocking` — so a local Notes/Ask generation can never run
//! concurrently with an in-progress whisper transcription/diarization/embedding pass.
//!
//! ## Privacy (load-bearing)
//! Built UNWRAPPED — no [`crate::summarize::redact::RedactingProvider`], no egress-ledger sink —
//! exactly like a loopback Ollama, because the `local` connection is classified on-device
//! ([`crate::summarize::egress_is_cloud`] returns `false`). Nothing leaves the machine, so there is
//! nothing to redact or consent-gate; the note's Privacy Receipt reads zero cloud calls.
//!
//! ## Honest scope
//! Real note QUALITY on a Q4 GGUF (and Polish quality) is Mac-verified only (see the `mistral.rs`
//! honest-scope header). This typechecks against the reasoner seam; a green build is not proof the
//! note reads well — that gate is the P3 notes bake-off on a real Mac.

use std::sync::Arc;

use async_trait::async_trait;

use crate::error::AppError;
use crate::reason::LocalReasoner;
use crate::summarize::provider::*;
use crate::summarize::template;

async fn run_local_reasoner<T, F>(
    heavy: &Arc<tokio::sync::Semaphore>,
    recording_token: Option<crate::perf::RecordingSessionToken>,
    reasoner: Arc<dyn LocalReasoner>,
    call: F,
) -> crate::error::Result<T>
where
    T: Send + 'static,
    F: FnOnce(&dyn LocalReasoner) -> crate::error::Result<T> + Send + 'static,
{
    if reasoner.id().starts_with("sidecar:") {
        crate::perf::run_heavy_maybe_recording(
            heavy,
            recording_token,
            crate::perf::ResidentModelKind::BrainGguf,
            move || call(reasoner.as_ref()),
        )
        .await
    } else {
        // Deterministic fixtures/fallbacks own no weights and must not publish a fake resident kind.
        crate::perf::run_blocking_serialized(heavy, move || call(reasoner.as_ref())).await
    }
}

/// A [`SummarizerProvider`] backed by the on-device heavy reasoner.
pub struct LocalSummarizerProvider {
    reasoner: Arc<dyn LocalReasoner>,
    /// The shared `AppState::heavy_inference` gate — see `crate::perf::run_heavy`'s doc comment.
    /// Every call into `reasoner.reason(...)` routes through it so a local Notes/Ask generation
    /// can never run concurrently with an in-progress whisper transcription/diarization/embedding
    /// pass on the same machine.
    heavy: Arc<tokio::sync::Semaphore>,
    recording_token: Option<crate::perf::RecordingSessionToken>,
}

impl LocalSummarizerProvider {
    pub fn new(reasoner: Arc<dyn LocalReasoner>, heavy: Arc<tokio::sync::Semaphore>) -> Self {
        Self {
            reasoner,
            heavy,
            recording_token: None,
        }
    }

    pub(crate) fn new_recording(
        reasoner: Arc<dyn LocalReasoner>,
        heavy: Arc<tokio::sync::Semaphore>,
        token: crate::perf::RecordingSessionToken,
    ) -> Self {
        Self {
            reasoner,
            heavy,
            recording_token: Some(token),
        }
    }
}

#[async_trait]
impl SummarizerProvider for LocalSummarizerProvider {
    fn id(&self) -> &str {
        "local"
    }

    async fn availability(&self) -> Availability {
        // The factory only builds this when a GGUF resolved on disk, so the model is present; the
        // heavy load stays lazy (first call). A load failure surfaces per-call, not here.
        Availability::Available
    }

    async fn summarize(&self, req: &SummarizeRequest) -> crate::error::Result<String> {
        // Same combined-prompt shape as the local Ollama provider (instructions + metadata + vault
        // titles + transcript in one blob) — the on-device reasoner has one system+user channel.
        let prompt = template::render_prompt(req);
        let reasoner = Arc::clone(&self.reasoner);
        // Routed through the shared heavy-inference gate (perf::run_heavy), not a bare
        // spawn_blocking — a local Notes generation must serialize against any OTHER heavy
        // native-runtime call (whisper ASR, the diarizer, the embedder/NER) running concurrently.
        let note = run_local_reasoner(
            &self.heavy,
            self.recording_token.clone(),
            reasoner,
            move |reasoner| {
                reasoner.reason(
                "You are a meeting-notes writer. Produce clean Obsidian-ready Markdown; follow the \
                 instructions exactly.",
                &prompt,
            )
            },
        )
        .await?;
        let note = note.trim_start_matches('\u{feff}').trim();
        if note.is_empty() {
            return Err(AppError::Summarize(
                "the on-device model returned no note".into(),
            ));
        }
        Ok(note.to_string())
    }

    async fn complete(&self, system: &str, user: &str) -> crate::error::Result<String> {
        let reasoner = Arc::clone(&self.reasoner);
        let system = system.to_string();
        let user = user.to_string();
        let out = run_local_reasoner(
            &self.heavy,
            self.recording_token.clone(),
            reasoner,
            move |reasoner| reasoner.reason(&system, &user),
        )
        .await?;
        Ok(out.trim().to_string())
    }

    /// Honor the per-call [`crate::reason::GenOptions`] on the on-device path — the note-edit token
    /// cap + low temperature reach [`crate::reason::LocalReasoner::reason_with`] →
    /// `SidecarReasoner`'s `set_sampler_max_len`, so a compression/rewrite edit can't run away and
    /// LENGTHEN. `CallMeta::default()` (no redactions — nothing egresses on the local path).
    async fn complete_with_meta_opts(
        &self,
        system: &str,
        user: &str,
        opts: crate::reason::GenOptions,
    ) -> crate::error::Result<(String, crate::summarize::meta::CallMeta)> {
        let reasoner = Arc::clone(&self.reasoner);
        let system = system.to_string();
        let user = user.to_string();
        let out = run_local_reasoner(
            &self.heavy,
            self.recording_token.clone(),
            reasoner,
            move |reasoner| reasoner.reason_with(&system, &user, opts),
        )
        .await?;
        Ok((
            out.trim().to_string(),
            crate::summarize::meta::CallMeta::default(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reason::StubReasoner;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[tokio::test]
    async fn id_is_local_and_available() {
        let p = LocalSummarizerProvider::new(
            Arc::new(StubReasoner),
            Arc::new(tokio::sync::Semaphore::new(1)),
        );
        assert_eq!(p.id(), "local");
        assert!(matches!(p.availability().await, Availability::Available));
    }

    #[tokio::test]
    async fn complete_bridges_the_sync_reasoner() {
        // The stub echoes deterministically — proves the async↔sync spawn_blocking bridge works and
        // returns the reasoner's text (no network, no egress).
        let p = LocalSummarizerProvider::new(
            Arc::new(StubReasoner),
            Arc::new(tokio::sync::Semaphore::new(1)),
        );
        let out = p.complete("sys", "user").await.unwrap();
        assert!(out.contains("stub-reason"), "got {out}");
    }

    /// A fake [`LocalReasoner`] whose `reason` call tracks max-concurrency via a shared
    /// `AtomicUsize`, sleeping ~50ms mid-call so two overlapping calls would be observable if the
    /// semaphore did NOT serialize them (mirrors `perf::run_heavy_serializes_two_concurrent_calls`).
    struct ConcurrencyTrackingReasoner {
        concurrent: Arc<AtomicUsize>,
        max_concurrent: Arc<AtomicUsize>,
    }

    impl crate::reason::LocalReasoner for ConcurrencyTrackingReasoner {
        fn id(&self) -> &str {
            "concurrency-tracking-stub"
        }

        fn reason(&self, _system: &str, _user: &str) -> crate::error::Result<String> {
            let now = self.concurrent.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_concurrent.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(50));
            self.concurrent.fetch_sub(1, Ordering::SeqCst);
            Ok("tracked".to_string())
        }

        fn structured(
            &self,
            _system: &str,
            _user: &str,
            _json_schema: &serde_json::Value,
        ) -> crate::error::Result<serde_json::Value> {
            Ok(serde_json::json!({}))
        }
    }

    /// The core contract this change adds: TWO separate `LocalSummarizerProvider` instances that
    /// share ONE `heavy_inference` semaphore never run their reasoner calls concurrently — proving
    /// the wiring is real, not just "it compiles". Without routing `complete` through
    /// `perf::run_heavy`, two concurrent `tokio::spawn`ed calls would both land on the blocking
    /// pool and race (max_concurrent would read 2).
    #[tokio::test]
    async fn two_providers_sharing_one_semaphore_never_run_concurrently() {
        let heavy = Arc::new(tokio::sync::Semaphore::new(1));
        let concurrent: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
        let max_concurrent: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));

        let make_provider = |heavy: Arc<tokio::sync::Semaphore>| {
            LocalSummarizerProvider::new(
                Arc::new(ConcurrencyTrackingReasoner {
                    concurrent: concurrent.clone(),
                    max_concurrent: max_concurrent.clone(),
                }),
                heavy,
            )
        };
        let p1 = make_provider(heavy.clone());
        let p2 = make_provider(heavy.clone());

        let t1 = tokio::spawn(async move { p1.complete("sys", "user").await });
        let t2 = tokio::spawn(async move { p2.complete("sys", "user").await });
        t1.await.unwrap().unwrap();
        t2.await.unwrap().unwrap();

        assert_eq!(
            max_concurrent.load(Ordering::SeqCst),
            1,
            "two LocalSummarizerProvider instances sharing one heavy_inference semaphore must \
             never run their reasoner calls concurrently"
        );
    }
}
