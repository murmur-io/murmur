//! The on-device HEAVY engine as a [`SummarizerProvider`] (spec §3.2, P3) — lets the Notes/Ask roles
//! be served FULLY LOCALLY under the Fully-Local posture. Wraps a shared local [`LocalReasoner`]
//! (whose weights live in the process-global mistral MODEL_CACHE, loaded once) and bridges the sync
//! reasoner to the async trait via `spawn_blocking`.
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

/// A [`SummarizerProvider`] backed by the on-device heavy reasoner.
pub struct LocalSummarizerProvider {
    reasoner: Arc<dyn LocalReasoner>,
}

impl LocalSummarizerProvider {
    pub fn new(reasoner: Arc<dyn LocalReasoner>) -> Self {
        Self { reasoner }
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
        let note = tokio::task::spawn_blocking(move || {
            reasoner.reason(
                "You are a meeting-notes writer. Produce clean Obsidian-ready Markdown; follow the \
                 instructions exactly.",
                &prompt,
            )
        })
        .await
        .map_err(|e| AppError::Summarize(format!("local summarize task join failed: {e}")))??;
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
        let out = tokio::task::spawn_blocking(move || reasoner.reason(&system, &user))
            .await
            .map_err(|e| AppError::Summarize(format!("local complete task join failed: {e}")))??;
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
        let out = tokio::task::spawn_blocking(move || reasoner.reason_with(&system, &user, opts))
            .await
            .map_err(|e| AppError::Summarize(format!("local complete task join failed: {e}")))??;
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

    #[tokio::test]
    async fn id_is_local_and_available() {
        let p = LocalSummarizerProvider::new(Arc::new(StubReasoner));
        assert_eq!(p.id(), "local");
        assert!(matches!(p.availability().await, Availability::Available));
    }

    #[tokio::test]
    async fn complete_bridges_the_sync_reasoner() {
        // The stub echoes deterministically — proves the async↔sync spawn_blocking bridge works and
        // returns the reasoner's text (no network, no egress).
        let p = LocalSummarizerProvider::new(Arc::new(StubReasoner));
        let out = p.complete("sys", "user").await.unwrap();
        assert!(out.contains("stub-reason"), "got {out}");
    }
}
