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

/// KV / activation / runtime headroom factor applied to a GGUF's on-disk (weights) size to estimate
/// its true peak resident footprint (P0.3, perf-memory-audit §2). The GGUF weights stay quantized in
/// RAM (~on-disk size — audit §1 BF16 note), but the prefill KV cache + Metal buffers + activations
/// add substantially on top for a long prompt. `1.5×` is DELIBERATELY CONSERVATIVE (low, so we do not
/// false-refuse a healthy machine): the goal is to catch "load a multi-GB model when free RAM is
/// already nearly exhausted", not to reject a normal load. Integer-scaled as `× 3 / 2` to stay in
/// `u64` without a float.
const MODEL_RAM_HEADROOM_NUM: u64 = 3;
const MODEL_RAM_HEADROOM_DEN: u64 = 2;

/// Only guard loads for models at least this large on disk. Tiny GGUFs (e.g. an embedding-sized
/// model) never move the needle on a memory-pressure kill, and probing/estimating for them just
/// risks a false refuse — so a small model always loads (fail-open by size).
const MODEL_RAM_GUARD_MIN_DISK_BYTES: u64 = 1_500_000_000; // ~1.5 GB

/// Decide whether there is enough FREE system RAM to load a model of `model_disk_bytes` on top of
/// what is ALREADY resident. PURE + injectable (no OS probe here) so it is unit-testable.
///
/// - `free_bytes = None` ⇒ the OS probe FAILED — FAIL OPEN (return `true`). We never block a working
///   setup because the RAM probe broke; we only refuse when we AFFIRMATIVELY measure that a load
///   would not fit. (The never-evict `model_cache` means an already-resident model stays put; this
///   check is purely about the headroom for the NEXT load.)
/// - a small model (`< MODEL_RAM_GUARD_MIN_DISK_BYTES`) ⇒ always `true` (never worth guarding).
/// - otherwise ⇒ `true` iff `free_bytes >= model_disk_bytes × headroom`.
fn ram_permits_load(free_bytes: Option<u64>, model_disk_bytes: u64) -> bool {
    if model_disk_bytes < MODEL_RAM_GUARD_MIN_DISK_BYTES {
        return true; // tiny model — never the OOM driver; don't risk a false refuse.
    }
    let Some(free) = free_bytes else {
        return true; // probe failed → fail OPEN (never break a working machine on a broken probe).
    };
    let needed = model_disk_bytes
        .saturating_mul(MODEL_RAM_HEADROOM_NUM)
        / MODEL_RAM_HEADROOM_DEN;
    free >= needed
}

/// Best-effort AVAILABLE (free + reclaimable) system RAM in bytes, macOS, via `vm_stat` (no new
/// crate/FFI — mirrors the `sysctl` pattern in `commands.rs::total_ram_gb`). Sums the page classes
/// macOS can hand to a new allocation WITHOUT swapping — free + inactive + speculative + purgeable
/// (NOT wired/active/compressed, which are in use) — times the page size. Returns `None` on ANY
/// parse/exec failure so the caller FAILS OPEN (never refuses a load because the probe broke). This
/// is a coarse estimate, deliberately so: the guard is a swap-death backstop, not an accountant.
fn available_ram_bytes() -> Option<u64> {
    let out = std::process::Command::new("vm_stat").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    // Header line: "Mach Virtual Memory Statistics: (page size of 16384 bytes)".
    let page_size = text
        .lines()
        .next()
        .and_then(|l| l.split("page size of ").nth(1))
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(4096);
    // Parse "Pages free: 12345." style lines into a page count.
    let pages = |label: &str| -> u64 {
        text.lines()
            .find(|l| l.trim_start().starts_with(label))
            .and_then(|l| l.rsplit(':').next())
            .map(|v| v.trim().trim_end_matches('.').replace(',', ""))
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0)
    };
    let free = pages("Pages free");
    let inactive = pages("Pages inactive");
    let speculative = pages("Pages speculative");
    let purgeable = pages("Pages purgeable");
    let avail_pages = free
        .saturating_add(inactive)
        .saturating_add(speculative)
        .saturating_add(purgeable);
    // A zero result means we couldn't read any of the classes → treat as probe failure (fail open).
    if avail_pages == 0 {
        return None;
    }
    Some(avail_pages.saturating_mul(page_size))
}

/// The on-disk size (bytes) of the GGUF at `path`, or `None` if it can't be stat'd — the estimate
/// input for [`ram_permits_load`]. A stat failure yields `None` ⇒ the guard fails OPEN.
fn model_disk_bytes(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len())
}

/// The resident-model list: (canonical GGUF path → loaded engine), capped at [`MODEL_CACHE_CAP`].
type ResidentModels = Vec<(PathBuf, Arc<Model>)>;

/// The process-global loaded-model cache. Shared by every [`MistralReasoner`] instance (and the
/// LocalSummarizerProvider), so a model's weights load exactly once. A `Vec` (not a map) keeps the
/// cap trivially enforceable and preserves load order.
fn model_cache() -> &'static Mutex<ResidentModels> {
    static CACHE: OnceLock<Mutex<ResidentModels>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

/// One PROCESS-WIDE multi-thread runtime driving every mistralrs async op (loads + generations).
/// Built once and reused — the Brain-Live `light()`/`heavy()` handles build a fresh [`MistralReasoner`]
/// per call, so a per-instance runtime would spawn (and churn) a thread pool on every reaction scan
/// (~every 21 s) — a real thread leak (deep-review). A build failure yields `None` and every inference
/// then returns `Err` gracefully (never a panic).
fn brain_rt() -> Option<&'static tokio::runtime::Runtime> {
    static RT: OnceLock<Option<tokio::runtime::Runtime>> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| tracing::error!(target: "reason", error = %e, "brain runtime build failed"))
            .ok()
    })
    .as_ref()
}

/// A [`LocalReasoner`] running a local GGUF model in-process via mistralrs (Metal). The heavy engine
/// lives in the shared [`model_cache`] and the runtime is the shared [`brain_rt`], so an instance holds
/// only paths — it is CHEAP to build (the light and heavy handles each build one per call, sharing
/// both weights and the runtime).
pub struct MistralReasoner {
    id: String,
    /// Full canonical GGUF path — the shared-cache key.
    model_path: PathBuf,
    /// Directory holding the GGUF (mistralrs' `GgufModelBuilder` takes a dir + filename list).
    model_dir: PathBuf,
    /// GGUF filename within `model_dir`.
    model_file: String,
}

impl MistralReasoner {
    /// Build a reasoner for the GGUF at `model_path`. CHEAP + non-blocking: it only splits the path —
    /// the multi-GB model load is deferred to first use (shared via [`model_cache`]) and the runtime is
    /// the shared [`brain_rt`], so this is safe to call on the startup path and per call. Returns `Err`
    /// (never panics) if the path has no parent/filename.
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
        Ok(Self {
            id: format!("mistralrs:{model_file}"),
            model_path,
            model_dir,
            model_file,
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
        // P0.3 — REFUSE-don't-OOM: gate the NEXT load on measured free RAM (the model is NOT yet
        // resident here — the early `find` returned an already-loaded engine). We refuse ONLY when we
        // AFFIRMATIVELY measure that this GGUF + KV headroom won't fit the currently-available RAM;
        // a failed probe or an unreadable file size fails OPEN (never blocks a working machine). This
        // catches the "load a 6.3 GB model on a 16 GB Mac already under pressure" swap-death without
        // false-refusing a healthy one (headroom is a conservative 1.5× of the on-disk size). Returns
        // `Unavailable` so the caller degrades to the deterministic floor / Cloud instead of OOM-ing.
        let disk = model_disk_bytes(&self.model_path);
        if let Some(bytes) = disk {
            if !ram_permits_load(available_ram_bytes(), bytes) {
                let gb = bytes as f64 / 1_073_741_824.0;
                tracing::warn!(
                    target: "reason",
                    model = %self.model_file,
                    model_gb = format_args!("{gb:.1}"),
                    "refusing on-device model load: insufficient free memory"
                );
                return Err(AppError::Unavailable(format!(
                    "not enough free memory to load this on-device model ({}, {gb:.1} GB) — \
                     switch the brain to Cloud in Settings or pick a smaller model",
                    self.model_file
                )));
            }
        }
        let builder = GgufModelBuilder::new(
            self.model_dir.to_string_lossy().to_string(),
            vec![self.model_file.clone()],
        )
        .with_logging();
        let rt = brain_rt().ok_or_else(|| AppError::Summarize("brain runtime unavailable".into()))?;
        // `build()` returns `anyhow::Result<Model>`; flatten the thread-join + the inner result.
        let model = block_on(rt, builder.build())?
            .map_err(|e| AppError::Summarize(format!("brain model load failed: {e}")))?;
        let arc = Arc::new(model);
        cache.push((self.model_path.clone(), arc.clone()));
        Ok(arc)
    }

    /// Free-form generation with explicit [`GenOptions`] — the load-bearing path. Two OPTIONAL
    /// bounds compose (Brain v2 P0.3, SCOPED per-call via the options — spec §P0.3 is LIVE-path
    /// discipline, not a blanket bound):
    /// - the token cap (`set_sampler_max_len`) bounds a runaway decode LENGTH;
    /// - `opts.timeout` (`Some` on the live/ask/extraction presets) bounds runaway decode TIME: the
    ///   blocking generation runs on a spawned worker thread ([`run_with_timeout`]) and a reply that
    ///   misses the deadline yields `AppError::Unavailable` while the worker is deliberately LEAKED
    ///   (an in-flight mistralrs generation is NOT cancellable; the model lives in the
    ///   process-global cache and is never dropped).
    ///
    /// `opts.timeout = None` (the `GenOptions::default()` path — note generation, the FullyLocal
    /// Ask floor, default agent steps) runs `generate_blocking` DIRECTLY on the current thread,
    /// UNBOUNDED — exactly the pre-P0 behavior. This is deliberate: FullyLocal note generation
    /// legitimately takes 30–90 s on Qwen3-4B, a huge Ask-floor prefill can too, and the FIRST
    /// generation on a CLT-only Mac pays the deferred Metal shader compile
    /// (`MISTRALRS_METAL_PRECOMPILE=0`) inside `send_chat_request` — a blanket 30 s timeout would
    /// kill all three.
    ///
    /// The MODEL LOAD (`self.model()`) deliberately stays OUTSIDE any timeout: a first-call
    /// multi-GB load is not a runaway generation, is already gated by the RAM guard, and may
    /// legitimately exceed 30 s on a cold disk.
    fn reason_with_opts(&self, system: &str, user: &str, opts: GenOptions) -> Result<String> {
        let model = self.model()?;
        let system = system.to_string();
        let user = user.to_string();
        run_bounded(opts.timeout, move || {
            generate_blocking(&model, &system, &user, opts)
        })
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
        let content = self.reason_with_opts(&sys, user, opts)?;
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
        self.reason_with_opts(system, user, GenOptions::default())
    }

    fn reason_with(&self, system: &str, user: &str, opts: GenOptions) -> Result<String> {
        self.reason_with_opts(system, user, opts)
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

/// The BLOCKING generation core: build one chat request over an already-resolved engine and drive
/// it to completion. Runs on the [`run_with_timeout`] worker thread — that thread has no ambient
/// runtime, so the inner scoped `block_on` is safe regardless of caller context. `enable_thinking`
/// / temperature / token cap come from the [`GenOptions`].
fn generate_blocking(
    model: &Arc<Model>,
    system: &str,
    user: &str,
    opts: GenOptions,
) -> Result<String> {
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
    let rt = brain_rt().ok_or_else(|| AppError::Summarize("brain runtime unavailable".into()))?;
    let resp = block_on(rt, model.send_chat_request(req))?
        .map_err(|e| AppError::Summarize(format!("brain generation failed: {e}")))?;
    MistralReasoner::first_content(resp)
}

/// Brain v2 P0.3 (revised) — the SCOPED bound seam: apply the wall-clock timeout ONLY when the
/// caller's [`GenOptions`] asked for one.
///
/// - `Some(timeout)` (the live / ask / light-extraction presets) ⇒ [`run_with_timeout`] on a
///   spawned worker thread — the live-path discipline.
/// - `None` (`GenOptions::default()` — note generation, the FullyLocal Ask floor, default agent
///   steps) ⇒ run `work` DIRECTLY on the current thread, UNBOUNDED — the pre-P0 behavior. Those
///   paths legitimately exceed 30 s (Qwen3-4B note-gen runs 30–90 s; the first generation on a
///   CLT-only Mac pays the deferred Metal shader compile), so they must never be timeout-killed.
///
/// Factored as a seam (not inlined in `reason_with_opts`) so the branch decision is unit-testable
/// headless, without a model.
fn run_bounded<T: Send + 'static>(
    timeout: Option<std::time::Duration>,
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    match timeout {
        Some(t) => run_with_timeout(t, work),
        None => work(),
    }
}

/// Brain v2 P0.3 — the testable WAIT MECHANIC of the generation timeout: run `work` on a spawned
/// (detached) OS thread and wait at most `timeout` for its result over an mpsc channel.
///
/// - In time ⇒ the worker's own `Result` is returned verbatim.
/// - TIMEOUT ⇒ `AppError::Unavailable` and the worker thread is deliberately LEAKED — the caller
///   must never block on (or cancel) an uncancellable mistralrs generation, and the model itself
///   is never dropped (it lives in the process-global [`model_cache`]). The eventual send into the
///   dropped channel is a no-op.
/// - Worker PANIC ⇒ the sender is dropped without a message ⇒ `Disconnected` ⇒ a graceful `Err`
///   (never a hang, never a propagated panic).
///
/// Factored generic over `work` precisely so the mechanics are unit-testable headless, without a
/// real model (`cargo test --lib` never runs a forward pass here — see the module header).
fn run_with_timeout<T: Send + 'static>(
    timeout: std::time::Duration,
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    let (tx, rx) = std::sync::mpsc::channel::<Result<T>>();
    std::thread::Builder::new()
        .name("murmur-brain-gen".into())
        .spawn(move || {
            let _ = tx.send(work());
        })
        .map_err(|e| AppError::Summarize(format!("brain generation worker spawn failed: {e}")))?;
    match rx.recv_timeout(timeout) {
        Ok(res) => res,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            // NO PII: duration only. The worker keeps running detached (leaked by design).
            tracing::warn!(
                target: "reason",
                timeout_s = timeout.as_secs(),
                "on-device brain generation timed out; leaking the worker (generations are uncancellable)"
            );
            Err(AppError::Unavailable(
                "on-device brain generation timed out".into(),
            ))
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Err(AppError::Summarize(
            "brain generation worker exited without a result".into(),
        )),
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

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1_073_741_824;

    /// P0.3 — the pure RAM decision. A HEALTHY machine (plenty of free RAM) permits loading a big
    /// model; a machine already under pressure (tiny free RAM vs a big model) REFUSES it.
    #[test]
    fn ram_permits_load_ok_when_free_and_refuses_under_pressure() {
        let big_model = 6 * GB + GB / 2; // ~6.3 GB heavy model (Bielik-class)

        // Plenty free (24 GB free): 6.3 GB × 1.5 = ~9.5 GB needed ≤ 24 GB → OK.
        assert!(
            ram_permits_load(Some(24 * GB), big_model),
            "a healthy machine with 24 GB free must permit a 6.3 GB model"
        );

        // Under pressure (only 4 GB free): 9.5 GB needed > 4 GB → REFUSE (the OOM case).
        assert!(
            !ram_permits_load(Some(4 * GB), big_model),
            "4 GB free must REFUSE a 6.3 GB model (would swap-death the machine)"
        );

        // Borderline: free exactly equals the headroom estimate → OK (>=, conservative-but-permits).
        let needed = big_model * MODEL_RAM_HEADROOM_NUM / MODEL_RAM_HEADROOM_DEN;
        assert!(ram_permits_load(Some(needed), big_model));
        assert!(!ram_permits_load(Some(needed - 1), big_model));
    }

    /// FAIL OPEN: a failed RAM probe (`None`) must NEVER block a load — we only refuse on an
    /// affirmative measurement of insufficient memory, never because the probe broke.
    #[test]
    fn ram_permits_load_fails_open_on_broken_probe() {
        let big_model = 9 * GB;
        assert!(
            ram_permits_load(None, big_model),
            "a broken RAM probe must fail OPEN (never break a working setup)"
        );
    }

    /// A SMALL model is never guarded (never the OOM driver) — it loads even with almost no free RAM,
    /// so the guard can't false-refuse a tiny model / embedder-sized GGUF.
    #[test]
    fn ram_permits_load_small_model_always_ok() {
        let tiny = MODEL_RAM_GUARD_MIN_DISK_BYTES - 1;
        assert!(
            ram_permits_load(Some(100 * 1024 * 1024), tiny),
            "a sub-1.5 GB model must always load regardless of pressure"
        );
        // ...but a model at/above the threshold IS guarded.
        assert!(!ram_permits_load(
            Some(1024 * 1024 * 1024),
            MODEL_RAM_GUARD_MIN_DISK_BYTES + GB
        ));
    }

    /// Brain v2 P0.3 — the timeout wait mechanic, fast path: a worker that finishes within the
    /// deadline returns its own result verbatim (Ok and Err alike).
    #[test]
    fn run_with_timeout_returns_fast_result() {
        let ok = run_with_timeout(std::time::Duration::from_secs(5), || Ok(42u32));
        assert!(matches!(ok, Ok(42)));

        let err: Result<u32> = run_with_timeout(std::time::Duration::from_secs(5), || {
            Err(AppError::Summarize("inner failure".into()))
        });
        assert!(
            matches!(err, Err(AppError::Summarize(_))),
            "a worker's own error must pass through verbatim"
        );
    }

    /// Brain v2 P0.3 — the timeout path: a worker that outlives the deadline yields
    /// `AppError::Unavailable` (the caller degrades to the floor / a lower tier) and is LEAKED,
    /// never joined — the call returns promptly instead of hanging on the slow worker.
    #[test]
    fn run_with_timeout_times_out_gracefully_and_leaks_the_worker() {
        let started = std::time::Instant::now();
        let res: Result<u32> = run_with_timeout(std::time::Duration::from_millis(50), || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            Ok(1)
        });
        match res {
            Err(AppError::Unavailable(msg)) => {
                assert!(
                    msg.contains("timed out"),
                    "the timeout error must say so: {msg}"
                );
            }
            other => panic!("timeout must be Unavailable, got {other:?}"),
        }
        // Prompt return: we did NOT wait for the 500 ms worker (leaked by design).
        assert!(
            started.elapsed() < std::time::Duration::from_millis(400),
            "the caller must not block on the leaked worker"
        );
    }

    /// Brain v2 P0.3 (revised) — DEFAULT options carry NO timeout, and the `None` branch of
    /// [`run_bounded`] runs the work INLINE on the CURRENT thread, unbounded — the pre-P0 behavior
    /// note generation / the FullyLocal Ask floor / first-gen shader compile depend on. A slow
    /// worker (well past what a live-preset deadline would allow) still completes.
    #[test]
    fn default_gen_options_do_not_timeout() {
        // The default preset requests no wall-clock bound...
        assert!(GenOptions::default().timeout.is_none());

        // ...and the None branch executes inline on the caller's thread (no worker, no deadline):
        // a 200 ms "generation" — 4× the 50 ms deadline the timeout test uses — returns Ok.
        let caller = std::thread::current().id();
        let res = run_bounded(None, move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            Ok(std::thread::current().id())
        });
        match res {
            Ok(id) => assert_eq!(
                id, caller,
                "the unbounded path must run on the current thread (no timeout worker)"
            ),
            Err(e) => panic!("the unbounded path must never time out, got {e:?}"),
        }

        // The Some branch still delegates to the timeout wrapper: a deadline miss is Unavailable.
        let bounded: Result<u32> = run_bounded(Some(std::time::Duration::from_millis(50)), || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            Ok(1)
        });
        assert!(
            matches!(bounded, Err(AppError::Unavailable(_))),
            "a Some(timeout) must still bound the work, got {bounded:?}"
        );
    }

    /// Brain v2 P0.3 — worker-panic safety: a panicking worker drops the channel sender, which
    /// surfaces as a graceful `Err` (never a hang until the deadline, never a propagated panic).
    #[test]
    fn run_with_timeout_surfaces_worker_panic_as_err() {
        let res: Result<u32> = run_with_timeout(std::time::Duration::from_secs(5), || {
            panic!("worker exploded")
        });
        assert!(
            matches!(res, Err(AppError::Summarize(_))),
            "a worker panic must become a graceful Err, got {res:?}"
        );
    }

    /// The best-effort `available_ram_bytes` probe is crash-safe: it returns EITHER `Some(>0)` on a
    /// real macOS `vm_stat`, OR `None` if the tool/parse fails — never a panic, never `Some(0)`.
    #[test]
    fn available_ram_probe_is_crash_safe() {
        // `None` (probe unavailable in this environment) is the fail-open path — fine, nothing to assert.
        if let Some(b) = available_ram_bytes() {
            assert!(b > 0, "a Some result must be a positive byte count");
        }
    }
}
