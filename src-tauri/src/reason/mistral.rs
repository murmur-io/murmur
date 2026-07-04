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
//! - the token cap / `enable_thinking` actually taking effect in the sampler;
//! - Polish-language quality;
//! - Metal performance (load time, tokens/sec, memory), and the cap-2 co-residency / drop-leak
//!   behavior (spec §3.3, Spike B).
//!
//! `cargo test --lib` NEVER runs a forward pass here. Treat a green build as proof the impl
//! typechecks/links against mistralrs 0.8.1 — NOT as proof inference works.
//!
//! ## Shared, capped weight cache (spec §3.3)
//!
//! Loaded engines live in a PROCESS-GLOBAL [`model_cache`] keyed by canonical GGUF path, so a model's
//! multi-GB weights load ONCE and the light + heavy engines can co-reside. The cache is capped at
//! [`MODEL_CACHE_CAP`] and **REFUSES rather than evicts**: mistral.rs has documented drop-leaks
//! (issues #723/#865), so until a real-Mac spike (Spike B) proves clean drops we NEVER unload — a 3rd
//! distinct model is refused (`Err`), the caller degrades to the deterministic floor, and the FE can
//! nudge "restart to switch models".
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
use std::sync::{Arc, Mutex, OnceLock};

use mistralrs::{GgufModelBuilder, Model, RequestBuilder, TextMessageRole};
use serde_json::Value;

use crate::error::{AppError, Result};
use crate::reason::{parse_first_json, GenOptions, LocalReasoner};

/// Max distinct GGUF models held resident at once — the light + heavy co-residency budget (spec
/// §3.3). REFUSE-don't-evict at the cap (see the module header).
const MODEL_CACHE_CAP: usize = 2;

/// The resident-model list: (canonical GGUF path → loaded engine), capped at [`MODEL_CACHE_CAP`].
type ResidentModels = Vec<(PathBuf, Arc<Model>)>;

/// The process-global loaded-model cache. Shared by every [`MistralReasoner`] instance (and the
/// LocalSummarizerProvider), so a model's weights load exactly once. A `Vec` (not a map) keeps the
/// cap trivially enforceable and preserves load order.
fn model_cache() -> &'static Mutex<ResidentModels> {
    static CACHE: OnceLock<Mutex<ResidentModels>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

/// A [`LocalReasoner`] running a local GGUF model in-process via mistralrs (Metal). The heavy engine
/// lives in the shared [`model_cache`]; this holds only the path + a worker runtime, so instances are
/// cheap to create (the light and heavy handles each build one, sharing weights by path).
pub struct MistralReasoner {
    id: String,
    /// Full canonical GGUF path — the shared-cache key.
    model_path: PathBuf,
    /// Directory holding the GGUF (mistralrs' `GgufModelBuilder` takes a dir + filename list).
    model_dir: PathBuf,
    /// GGUF filename within `model_dir`.
    model_file: String,
    /// Dedicated runtime used to drive mistralrs' async API from the sync trait methods.
    rt: tokio::runtime::Runtime,
}

impl MistralReasoner {
    /// Build a reasoner for the GGUF at `model_path`. CHEAP + non-blocking: it only splits the path
    /// and stands up the worker runtime — the multi-GB model load is deferred to first use (and shared
    /// via [`model_cache`]), so this is safe to call from `active_reasoner` on the startup path.
    /// Returns `Err` (never panics) if the path has no parent/filename or the runtime can't be built.
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
            model_path,
            model_dir,
            model_file,
            rt,
        })
    }

    /// Return the cached engine for this model's path, loading it once if absent. Serializes loads
    /// behind the shared-cache mutex; at [`MODEL_CACHE_CAP`] a NEW path is REFUSED (`Err`) rather than
    /// evicting a resident model — the caller then degrades to the deterministic floor.
    fn model(&self) -> Result<Arc<Model>> {
        let mut cache = model_cache()
            .lock()
            .map_err(|_| AppError::Summarize("brain model cache poisoned".into()))?;
        if let Some((_, m)) = cache.iter().find(|(p, _)| p == &self.model_path) {
            return Ok(m.clone());
        }
        if cache.len() >= MODEL_CACHE_CAP {
            return Err(AppError::Summarize(format!(
                "Murmur Brain holds at most {MODEL_CACHE_CAP} models at once; restart to switch models"
            )));
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
        cache.push((self.model_path.clone(), arc.clone()));
        Ok(arc)
    }

    /// Free-form generation with explicit [`GenOptions`] — the load-bearing path. The token cap
    /// (`set_sampler_max_len`) bounds a runaway decode; `enable_thinking(false)` keeps qwen3 thinking
    /// traces out (a no-op on non-thinking models). An in-flight generation is NOT cancellable once
    /// submitted — the cap is the only per-call bound (spec §4.3).
    fn reason_with(&self, system: &str, user: &str, opts: GenOptions) -> Result<String> {
        let model = self.model()?;
        let mut req = RequestBuilder::new()
            .add_message(TextMessageRole::System, system)
            .add_message(TextMessageRole::User, user)
            .enable_thinking(opts.enable_thinking);
        if let Some(t) = opts.temperature {
            req = req.set_sampler_temperature(t);
        }
        if let Some(n) = opts.max_tokens {
            req = req.set_sampler_max_len(n);
        }
        let resp = block_on(&self.rt, model.send_chat_request(req))?
            .map_err(|e| AppError::Summarize(format!("brain generation failed: {e}")))?;
        Self::first_content(resp)
    }

    /// Structured generation with explicit [`GenOptions`]: instruct the JSON schema in the prompt
    /// (mistralrs' `Constraint::JsonSchema` overflowed the context on Bielik-11B; see the module
    /// header) and recover the object via the robust extractor — the SAME approach `CloudReasoner`
    /// uses. Threads the token cap through so the realtime path stays bounded.
    fn structured_with_opts(&self, system: &str, user: &str, json_schema: &Value, opts: GenOptions) -> Result<Value> {
        let sys = format!(
            "{system}\n\nRespond with ONLY a single JSON object conforming to this schema: \
             {json_schema}. No prose, no markdown fences."
        );
        let content = self.reason_with(&sys, user, opts)?;
        parse_first_json(&content)
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
        self.reason_with(system, user, GenOptions::default())
    }

    fn structured(&self, system: &str, user: &str, json_schema: &Value) -> Result<Value> {
        self.structured_with_opts(system, user, json_schema, GenOptions::default())
    }

    fn structured_with(
        &self,
        system: &str,
        user: &str,
        json_schema: &Value,
        opts: GenOptions,
    ) -> Result<Value> {
        self.structured_with_opts(system, user, json_schema, opts)
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
