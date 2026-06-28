//! Real on-device reasoning brain (Phase B) — a [`LocalReasoner`] backed by mistralrs 0.8.1 GGUF
//! inference (Metal). ALWAYS compiled; [`crate::reason::active_reasoner`] (BrainBackend::Local)
//! selects it at runtime when a GGUF is present on disk, else ships the dependency-free
//! `StubReasoner` instead.
//!
//! ## Honest scope (READ THIS)
//!
//! Everything here is **COMPILE-proven only** in the headless CI loop. What can ONLY be verified on a
//! signed/dev build on a real Mac, with a GGUF actually present:
//! - real inference correctness (does the model produce sane text at all);
//! - the **JSON-schema-constrained decode** ([`Constraint::JsonSchema`]) actually yielding valid,
//!   schema-conforming JSON;
//! - Polish-language quality;
//! - Metal performance (load time, tokens/sec, memory).
//!
//! `cargo test --lib` NEVER runs a forward pass here. Treat a green build as proof the impl
//! typechecks/links against mistralrs 0.8.1 — NOT as proof inference works.
//!
//! ## Graceful + crash-safe
//!
//! Every fallible step returns [`AppError`] — model load, runtime init, and generation NEVER panic or
//! `unwrap`. Construction ([`MistralReasoner::new`]) is cheap and infallible-by-design beyond a runtime
//! build: the heavy GGUF load is **lazy** (first `reason`/`structured` call), so this can back
//! `active_reasoner` without ever blocking or aborting app startup.
//!
//! ## Async ↔ sync bridge
//!
//! The [`LocalReasoner`] trait is synchronous, but mistralrs is async. The reasoner owns a dedicated
//! multi-thread Tokio runtime and drives each async op to completion on a freshly-spawned scoped OS
//! thread (via [`std::thread::scope`]). Running `block_on` on a thread with no ambient runtime context
//! sidesteps the "cannot start a runtime from within a runtime" panic when a caller invokes us from
//! inside `tokio::task::spawn_blocking`. Callers SHOULD still `spawn_blocking` us off the async pool.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use mistralrs::{GgufModelBuilder, Model, TextMessageRole, TextMessages};
use serde_json::Value;

use crate::error::{AppError, Result};
use crate::reason::{parse_first_json, LocalReasoner};

/// A [`LocalReasoner`] running a local GGUF model in-process via mistralrs (Metal). The model is
/// loaded lazily + cached behind an `Arc` so repeated calls reuse one engine.
pub struct MistralReasoner {
    id: String,
    /// Directory holding the GGUF (mistralrs' `GgufModelBuilder` takes a dir + filename list).
    model_dir: PathBuf,
    /// GGUF filename within `model_dir`.
    model_file: String,
    /// Dedicated runtime used to drive mistralrs' async API from the sync trait methods.
    rt: tokio::runtime::Runtime,
    /// Lazily-built, cached engine handle. `None` until the first `reason`/`structured` call.
    model: Mutex<Option<Arc<Model>>>,
}

impl MistralReasoner {
    /// Build a reasoner for the GGUF at `model_path`. CHEAP + non-blocking: it only splits the path
    /// and stands up the worker runtime — the multi-GB model load is deferred to first use, so this
    /// is safe to call from `active_reasoner` on the startup path. Returns `Err` (never panics) if
    /// the path has no parent/filename or the runtime can't be built.
    pub fn new(model_path: PathBuf) -> Result<Self> {
        let model_dir = model_path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| AppError::Summarize("brain model path has no parent dir".into()))?;
        let model_file = model_path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| AppError::Summarize("brain model path has no filename".into()))?
            .to_string();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| AppError::Summarize(format!("brain runtime init failed: {e}")))?;
        Ok(Self {
            id: format!("mistralrs:{model_file}"),
            model_dir,
            model_file,
            rt,
            model: Mutex::new(None),
        })
    }

    /// Lazily load (once) + return the cached engine handle. Serializes concurrent first-loads behind
    /// the mutex; a load failure surfaces as `Err` and leaves the cache empty (a later call may retry).
    fn model(&self) -> Result<Arc<Model>> {
        let mut guard = self
            .model
            .lock()
            .map_err(|_| AppError::Summarize("brain model mutex poisoned".into()))?;
        if let Some(m) = guard.as_ref() {
            return Ok(m.clone());
        }
        let builder = GgufModelBuilder::new(
            self.model_dir.to_string_lossy().to_string(),
            vec![self.model_file.clone()],
        )
        .with_logging();
        // `build()` returns `anyhow::Result<Model>`; flatten the thread-join + the inner result.
        let model = block_on(&self.rt, builder.build())?
            .map_err(|e| AppError::Summarize(format!("brain model load failed: {e}")))?;
        let arc = Arc::new(model);
        *guard = Some(arc.clone());
        Ok(arc)
    }

    /// Pull the first choice's text content out of a chat completion response.
    fn first_content(resp: mistralrs::ChatCompletionResponse) -> Result<String> {
        resp.choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| AppError::Summarize("brain returned no content".into()))
    }
}

impl LocalReasoner for MistralReasoner {
    fn id(&self) -> &str {
        &self.id
    }

    fn reason(&self, system: &str, user: &str) -> Result<String> {
        let model = self.model()?;
        let messages = TextMessages::new()
            .add_message(TextMessageRole::System, system)
            .add_message(TextMessageRole::User, user);
        let resp = block_on(&self.rt, model.send_chat_request(messages))?
            .map_err(|e| AppError::Summarize(format!("brain generation failed: {e}")))?;
        Self::first_content(resp)
    }

    fn structured(&self, system: &str, user: &str, json_schema: &Value) -> Result<Value> {
        // mistralrs' `Constraint::JsonSchema` overflowed the model context on Bielik-11B
        // (a `narrow … start: 32768` model error at the 32K context boundary), so instead we
        // INSTRUCT JSON in the prompt and recover it with the robust extractor — the SAME
        // approach `CloudReasoner` uses. `reason()` (unconstrained generation) is proven to work
        // (the on-Mac smoke test produced sane Polish); `parse_first_json` tolerates any prose or
        // markdown fence the model wraps around the object.
        let sys = format!(
            "{system}\n\nRespond with ONLY a single JSON object conforming to this schema: \
             {json_schema}. No prose, no markdown fences."
        );
        let content = self.reason(&sys, user)?;
        parse_first_json(&content)
    }
}

/// Drive a future to completion on a freshly-spawned scoped OS thread, using `rt`. That thread has no
/// ambient Tokio runtime, so `rt.block_on` cannot trip the nested-runtime panic even when the caller
/// is already inside a runtime (e.g. `spawn_blocking`). A panic in the worker is converted to `Err`.
fn block_on<F>(rt: &tokio::runtime::Runtime, fut: F) -> Result<F::Output>
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    std::thread::scope(|scope| scope.spawn(|| rt.block_on(fut)).join())
        .map_err(|_| AppError::Summarize("brain inference worker thread panicked".into()))
}
