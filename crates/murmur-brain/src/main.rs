//! `meetnotes-brain` — Murmur's on-device GGUF LLM inference, isolated in a killable helper process.
//!
//! WHY this exists: mistralrs holds a multi-GB model resident, has documented drop-leaks (#723/#865)
//! so the in-process code could never evict, and its generation is not cancellable. Running it in a
//! child process turns all three into non-problems: the host reclaims ALL of this process's RAM by
//! simply killing it (idle-unload / per-request timeout / app-quit), and a hung generation is
//! cancelled by SIGKILL. This binary loads ONE model, answers NDJSON generation requests over
//! stdin/stdout, and self-exits after an idle window.
//!
//! PROTOCOL: see `brain_ipc.rs`. stdout carries ONLY NDJSON [`brain_ipc::ChildMsg`] lines; ALL
//! diagnostics go to stderr (stdout purity is load-bearing — a stray log line would corrupt the
//! protocol channel). The prompt/transcript arrives ONLY on stdin and is never logged or written to
//! disk.
//!
//! ## stdout purity (how it is held)
//! - Our own tracing subscriber (installed FIRST in `main`) writes to stderr.
//! - We DO NOT call mistralrs' `.with_logging()`. That builder method would enable mistralrs logging,
//!   whose model-load path calls `Content::print_metadata()` → `println!`s the GGUF metadata dump to
//!   STDOUT — which is our NDJSON protocol channel. Leaving it off keeps mistralrs at its default
//!   SILENT state, so it never `println!`s the metadata (nor anything else) to stdout.
//! - mistralrs' load-progress bars are `indicatif`, which defaults to STDERR — never stdout.
//! - Every `ChildMsg` write is serialized behind one `Stdout` mutex (heartbeat + result threads), so a
//!   line is never interleaved. `ChildMsg` NDJSON is the ONLY thing ever written to stdout.
//!
//! ## the mistralrs logic below is lifted from the app's `reason/mistral.rs`
//! INCLUDING the mandatory `.with_prefix_cache_n(None)` NaN-trap fix — a resident child answering MANY
//! requests is EXACTLY the multi-generation scenario that re-trips the shared-prefix-cache NaN/Inf
//! corruption. The load/generation/RAM-guard shapes and comments mirror that file verbatim so a
//! reviewer can diff them.

mod brain_ipc;

use std::io::{BufRead, Write};
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use brain_ipc::{ChildMsg, ErrorKind, GenOptsWire, HostMsg};

use mistralrs::{
    Constraint, GgufModelBuilder, Model, RequestBuilder, TextMessageRole,
};

/// KV / activation / runtime headroom factor applied to a GGUF's on-disk (weights) size to estimate
/// its true peak resident footprint (P0.3, perf-memory-audit §2). `1.5×` is DELIBERATELY CONSERVATIVE
/// (low, so we do not false-refuse a healthy machine). Integer-scaled as `× 3 / 2` to stay in `u64`.
/// (Lifted verbatim from `reason/mistral.rs`.)
const MODEL_RAM_HEADROOM_NUM: u64 = 3;
const MODEL_RAM_HEADROOM_DEN: u64 = 2;

/// Only guard loads for models at least this large on disk. Tiny GGUFs never move the needle on a
/// memory-pressure kill, so a small model always loads (fail-open by size).
const MODEL_RAM_GUARD_MIN_DISK_BYTES: u64 = 1_500_000_000; // ~1.5 GB

/// Brain v2 L3 — the TINY-schema ceiling for the flag-gated grammar constraint: only a schema whose
/// serialized form is under this many bytes may be decode-constrained (`Constraint::JsonSchema`).
/// (Lifted verbatim from `reason/mistral.rs`.)
const GRAMMAR_SCHEMA_MAX_BYTES: usize = 512;

/// Heartbeat cadence while a generation is running — the host's timeout is a liveness check, not a
/// blind wall-clock guillotine on a healthy long note-gen.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

// ---------------------------------------------------------------------------------------------------
// RAM self-check helpers — PURE, lifted verbatim from `reason/mistral.rs` (P0.3 refuse-don't-OOM).
// ---------------------------------------------------------------------------------------------------

/// Decide whether there is enough FREE system RAM to load a model of `model_disk_bytes`. PURE +
/// injectable (no OS probe here) so it is unit-testable.
///
/// - `free_bytes = None` ⇒ the OS probe FAILED — FAIL OPEN (return `true`).
/// - a small model (`< MODEL_RAM_GUARD_MIN_DISK_BYTES`) ⇒ always `true`.
/// - otherwise ⇒ `true` iff `free_bytes >= model_disk_bytes × headroom`.
fn ram_permits_load(free_bytes: Option<u64>, model_disk_bytes: u64) -> bool {
    if model_disk_bytes < MODEL_RAM_GUARD_MIN_DISK_BYTES {
        return true; // tiny model — never the OOM driver; don't risk a false refuse.
    }
    let Some(free) = free_bytes else {
        return true; // probe failed → fail OPEN (never break a working machine on a broken probe).
    };
    let needed = model_disk_bytes.saturating_mul(MODEL_RAM_HEADROOM_NUM) / MODEL_RAM_HEADROOM_DEN;
    free >= needed
}

/// Best-effort AVAILABLE (free + reclaimable) system RAM in bytes, macOS, via `vm_stat` (no new
/// crate/FFI). Sums the page classes macOS can hand to a new allocation WITHOUT swapping. Returns
/// `None` on ANY parse/exec failure so the caller FAILS OPEN. (Lifted verbatim from `reason/mistral.rs`.)
fn available_ram_bytes() -> Option<u64> {
    let out = std::process::Command::new("vm_stat").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let page_size = text
        .lines()
        .next()
        .and_then(|l| l.split("page size of ").nth(1))
        .and_then(|s| s.split_whitespace().next())
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(4096);
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
    if avail_pages == 0 {
        return None; // couldn't read any class → probe failure (fail open).
    }
    Some(avail_pages.saturating_mul(page_size))
}

/// The on-disk size (bytes) of the GGUF at `path`, or `None` if it can't be stat'd — a stat failure
/// yields `None` ⇒ the guard fails OPEN.
fn model_disk_bytes(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|m| m.len())
}

/// Brain v2 L3 — the PURE decision: constrain THIS structured call's decode? True only when the caller
/// opted in AND the serialized schema is tiny. (Lifted verbatim from `reason/mistral.rs`.)
fn grammar_constraint_applies(opted_in: bool, schema_bytes: usize) -> bool {
    opted_in && schema_bytes < GRAMMAR_SCHEMA_MAX_BYTES
}

// ---------------------------------------------------------------------------------------------------
// stdout — the NDJSON protocol channel. Every ChildMsg write goes through ONE mutex so the heartbeat
// thread and the result thread never interleave a line.
// ---------------------------------------------------------------------------------------------------

/// A serialized handle to the stdout protocol channel. `emit` is the ONLY way a `ChildMsg` reaches
/// stdout; nothing else is ever written there.
#[derive(Clone)]
struct Out(Arc<Mutex<std::io::Stdout>>);

impl Out {
    fn new() -> Self {
        Out(Arc::new(Mutex::new(std::io::stdout())))
    }

    /// Write one NDJSON `ChildMsg` line and flush. Serialization or lock failure is dropped silently —
    /// we NEVER print an error to stdout (that would itself corrupt the channel).
    fn emit(&self, msg: &ChildMsg) {
        let Ok(line) = serde_json::to_string(msg) else {
            return;
        };
        if let Ok(mut out) = self.0.lock() {
            let _ = writeln!(out, "{line}");
            let _ = out.flush();
        }
    }
}

// ---------------------------------------------------------------------------------------------------
// CLI args
// ---------------------------------------------------------------------------------------------------

/// CLI args passed by the host (never PII — a model path is not personal content).
struct Args {
    /// Absolute path to the GGUF model to load.
    model_path: Option<String>,
    /// Self-exit after this many seconds with no request (belt; the host is the authoritative killer).
    max_idle_seconds: u64,
}

fn parse_args() -> Args {
    let mut model_path = None;
    let mut max_idle_seconds = 600; // generous belt; host-side idle-kill is authoritative
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--model" => model_path = it.next(),
            "--max-idle-seconds" => {
                max_idle_seconds = it
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(max_idle_seconds)
            }
            _ => {}
        }
    }
    Args {
        model_path,
        max_idle_seconds,
    }
}

// ---------------------------------------------------------------------------------------------------
// Model load — eager, at startup. Mirrors `reason/mistral.rs::model()`.
// ---------------------------------------------------------------------------------------------------

/// Load the GGUF at `model_path` on `rt`, after a RAM self-check. `Ok(model)` on success; a RAM refusal
/// is `Err(ErrorKind::Oom, ..)` and any build failure is `Err(ErrorKind::Unavailable, ..)`. NO PII: the
/// only string is the model filename (not personal content) + coarse counts.
fn load_model(
    rt: &tokio::runtime::Runtime,
    model_path: &str,
) -> std::result::Result<(Model, String), (ErrorKind, String)> {
    let path = Path::new(model_path);
    let model_dir = path
        .parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| (ErrorKind::InvalidArg, "brain model path has no parent dir".into()))?;
    let model_file = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| (ErrorKind::InvalidArg, "brain model path has no filename".into()))?
        .to_string();

    // P0.3 — REFUSE-don't-OOM: gate the load on measured free RAM. We refuse ONLY when we AFFIRMATIVELY
    // measure that this GGUF + KV headroom won't fit; a failed probe or unreadable size fails OPEN.
    if let Some(bytes) = model_disk_bytes(path) {
        if !ram_permits_load(available_ram_bytes(), bytes) {
            let gb = bytes as f64 / 1_073_741_824.0;
            tracing::warn!(
                target: "brain",
                model = %model_file,
                model_gb = format_args!("{gb:.1}"),
                "refusing on-device model load: insufficient free memory"
            );
            return Err((
                ErrorKind::Oom,
                format!(
                    "not enough free memory to load this on-device model ({model_file}, {gb:.1} GB)"
                ),
            ));
        }
    }

    let builder = GgufModelBuilder::new(
        model_dir.to_string_lossy().to_string(),
        vec![model_file.clone()],
    )
    // NO `.with_logging()`. mistralrs' `with_logging()` calls `Content::print_metadata()`, which
    // `println!`s the GGUF metadata dump to STDOUT — and stdout is our NDJSON protocol channel, so
    // that would corrupt the wire. Omitting it leaves mistralrs SILENT (its default), so nothing
    // mistralrs-side ever reaches stdout; our own stderr `tracing_subscriber` still captures our logs.
    // MANDATORY — the NaN trap. DISABLE mistralrs sequence-level prefix caching
    // (`with_prefix_cache_n(None)` ⇒ engine `no_prefix_cache: true`; default is `Some(16)` = ON). It
    // reuses the KV-cache PREFIX across requests that share a leading system prompt — which EVERY
    // brain call does — and on the non-paged Metal GGUF path that cached prefix goes stale/corrupt
    // after the first few generations, so the 2nd+ call samples from NaN/Inf logits ("za drugim
    // razem"). A RESIDENT child answering MANY requests is PRECISELY that multi-generation scenario,
    // so this line is even more load-bearing here than in-process: every request gets a FRESH KV cache.
    .with_prefix_cache_n(None)
    // KV FOOTPRINT — clamp the scheduler to a SINGLE concurrent sequence. `GgufModelBuilder` defaults
    // `max_num_seqs` to 32 (mistralrs 0.8.1 `gguf.rs`), which sizes the scheduler — and the reserved
    // KV headroom — for 32 sequences this sidecar NEVER runs: generation is strictly one-request-at-a-
    // time (`generate_blocking` below; the app dispatches single-flight over NDJSON). Pinning it to 1
    // matches actual usage and removes the 32-slot concurrency reservation. Correct + harmless for BOTH
    // the light and heavy children (neither serves concurrent requests); it never constrains a lone
    // request's own prefill/decode. The exact resident-GB win is a Metal/KV-allocator property — MEASURE
    // it with `footprint` on a signed Mac (`scripts/measure-recording-ram.sh` shows the sidecar plateau).
    .with_max_num_seqs(1);

    // `build()` is async → drive it to completion on a scoped thread so it never trips the
    // nested-runtime panic. It returns `anyhow::Result<Model>`.
    let built = block_on(rt, builder.build());
    match built {
        Ok(Ok(model)) => Ok((model, format!("mistralrs:{model_file}"))),
        Ok(Err(e)) => Err((
            ErrorKind::Unavailable,
            format!("brain model load failed: {e}"),
        )),
        Err(msg) => Err((ErrorKind::Unavailable, msg)),
    }
}

// ---------------------------------------------------------------------------------------------------
// Generation — one request at a time. Mirrors `reason/mistral.rs::generate_blocking`.
// ---------------------------------------------------------------------------------------------------

/// Run ONE generation for `HostMsg::Generate`. Returns the whole-string result on success, or an
/// `(ErrorKind, message)` on failure. The `catch_unwind` wrapper means a panic in mistralrs NEVER
/// prints the prompt (the error carries counts/kind only, never prompt/response text).
///
/// Structured path: when `json_schema` is present AND the caller opted into the tiny-schema grammar
/// constraint AND the serialized schema is under the ceiling, FIRST try a `Constraint::JsonSchema`
/// decode; ANY failure falls back GRACEFULLY to schema-in-prompt (the SAME approach `reason/mistral.rs`
/// uses). Both paths return the raw text (the host's `LocalReasoner` recovers the object).
fn run_generation(
    rt: &tokio::runtime::Runtime,
    model: &Model,
    system: &str,
    user: &str,
    opts: &GenOptsWire,
    json_schema: Option<&serde_json::Value>,
) -> std::result::Result<String, (ErrorKind, String)> {
    // Structured with a constraint attempt?
    if let Some(schema) = json_schema {
        let schema_bytes = serde_json::to_string(schema).map(|s| s.len()).unwrap_or(usize::MAX);
        if grammar_constraint_applies(opts.use_grammar_constraint, schema_bytes) {
            let constraint = Constraint::JsonSchema(schema.clone());
            match generate_once(rt, model, system, user, opts, Some(constraint)) {
                Ok(text) => return Ok(text),
                Err((_, _)) => {
                    // NO PII: schema SIZE only — never the schema/prompt content.
                    tracing::debug!(
                        target: "brain",
                        schema_bytes,
                        "grammar-constrained decode failed; falling back to schema-in-prompt"
                    );
                }
            }
        }
        // Schema-in-prompt fallback (or the non-constrained structured path): instruct the schema in
        // the system prompt exactly as `reason/mistral.rs::structured_with_opts` does. The host runs
        // the robust JSON extractor on the returned text.
        let sys = format!(
            "{system}\n\nRespond with ONLY a single JSON object conforming to this schema: {schema}. \
             No prose, no markdown fences."
        );
        return generate_once(rt, model, &sys, user, opts, None);
    }
    // Free-form completion.
    generate_once(rt, model, system, user, opts, None)
}

/// The BLOCKING generation core: build one chat request and drive it to completion, wrapped in
/// `catch_unwind`. Mirrors `reason/mistral.rs::generate_blocking` (the exact 0.8.1 builder chain +
/// `send_chat_request`). Any panic becomes a graceful `Err` with NO prompt text.
fn generate_once(
    rt: &tokio::runtime::Runtime,
    model: &Model,
    system: &str,
    user: &str,
    opts: &GenOptsWire,
    constraint: Option<Constraint>,
) -> std::result::Result<String, (ErrorKind, String)> {
    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
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
        if let Some(c) = constraint {
            req = req.set_constraint(c);
        }
        // `send_chat_request` is async → drive it on a scoped thread (no ambient runtime there) so a
        // nested-runtime panic can't occur. It returns `mistralrs::Result<ChatCompletionResponse>`.
        block_on(rt, model.send_chat_request(req))
    }));

    match result {
        // catch_unwind Ok → the closure ran; unwrap the block_on + inner results.
        Ok(Ok(Ok(resp))) => resp
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| (ErrorKind::Summarize, "brain returned no content".into())),
        Ok(Ok(Err(e))) => Err((
            ErrorKind::Summarize,
            format!("brain generation failed: {e}"),
        )),
        // block_on's worker thread panicked (message is content-free).
        Ok(Err(msg)) => Err((ErrorKind::Summarize, msg)),
        // The generation closure itself panicked — NO PII: a fixed message, never the prompt.
        Err(_) => Err((
            ErrorKind::Summarize,
            "brain generation panicked".into(),
        )),
    }
}

/// Drive a future to completion on a freshly-spawned scoped OS thread, using `rt`. That thread has no
/// ambient Tokio runtime, so `rt.block_on` cannot trip the nested-runtime panic even when the caller is
/// already inside a runtime. A panic in the worker is converted to `Err(String)` (content-free).
/// Mirrors `reason/mistral.rs::block_on`.
fn block_on<F>(rt: &tokio::runtime::Runtime, fut: F) -> std::result::Result<F::Output, String>
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    std::thread::scope(|scope| scope.spawn(|| rt.block_on(fut)).join())
        .map_err(|_| "brain inference worker thread panicked".to_string())
}

// ---------------------------------------------------------------------------------------------------
// main — eager load, then the blocking stdin NDJSON loop. One generation at a time.
// ---------------------------------------------------------------------------------------------------

fn main() {
    // ALL logging to stderr; stdout stays pure NDJSON. Installed FIRST so mistralrs' own
    // `initialize_logging().try_init()` no-ops against this global subscriber.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let args = parse_args();
    let out = Out::new();

    // The child owns ONE process-wide multi-thread runtime driving every mistralrs async op (load +
    // generations). The stdin loop below stays BLOCKING; async lives entirely under `block_on`.
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(target: "brain", error = %e, "runtime build failed");
            out.emit(&ChildMsg::Error {
                id: 0,
                kind: ErrorKind::Unavailable,
                message: "brain runtime build failed".into(),
            });
            std::process::exit(1);
        }
    };

    // Eager load. Emit Ready ONLY after a successful load; on refusal/failure emit Error and exit
    // non-zero so the host degrades to Cloud/floor.
    let Some(model_path) = args.model_path.clone() else {
        out.emit(&ChildMsg::Error {
            id: 0,
            kind: ErrorKind::InvalidArg,
            message: "no --model path provided".into(),
        });
        std::process::exit(2);
    };
    let (model, model_id) = match load_model(&rt, &model_path) {
        Ok(pair) => pair,
        Err((kind, message)) => {
            out.emit(&ChildMsg::Error {
                id: 0,
                kind,
                message,
            });
            // Non-zero exit: the host reads the Error line, then sees the child gone, and degrades.
            std::process::exit(if kind == ErrorKind::Oom { 3 } else { 1 });
        }
    };
    let model = Arc::new(model);
    tracing::info!(target: "brain", "model loaded; ready");
    out.emit(&ChildMsg::Ready { model_id });

    // Idle self-exit belt (orphan safety) + heartbeat driver both watch shared atomics.
    // - `last_activity_ms`: monotonic-ish activity timestamp, reset on every received request.
    // - `in_generation`: true WHILE a generation runs — guards BOTH the idle-exit (never exit
    //   mid-generation) and the heartbeat thread (only beat while generating).
    // - `current_id`: the request id the heartbeat thread should stamp its beats with.
    let start = Instant::now();
    let last_activity_ms = Arc::new(AtomicU64::new(0));
    let in_generation = Arc::new(AtomicBool::new(false));
    let current_id = Arc::new(AtomicU64::new(0));

    spawn_idle_watchdog(
        args.max_idle_seconds,
        start,
        last_activity_ms.clone(),
        in_generation.clone(),
    );
    spawn_heartbeat(
        out.clone(),
        in_generation.clone(),
        current_id.clone(),
    );

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        // Any received line is activity — reset the idle belt (even a probe / an unparseable line).
        last_activity_ms.store(start.elapsed().as_millis() as u64, Ordering::SeqCst);

        match serde_json::from_str::<HostMsg>(&line) {
            Ok(HostMsg::ReadyProbe) => {
                out.emit(&ChildMsg::Ready {
                    model_id: "ready".into(),
                });
            }
            Ok(HostMsg::Generate {
                id,
                system,
                user,
                opts,
                json_schema,
            }) => {
                // Serialize: one generation at a time. Flag it so the idle belt won't exit and the
                // heartbeat thread beats for THIS id.
                current_id.store(id, Ordering::SeqCst);
                in_generation.store(true, Ordering::SeqCst);

                let result =
                    run_generation(&rt, &model, &system, &user, &opts, json_schema.as_ref());

                in_generation.store(false, Ordering::SeqCst);
                // Reset the idle timer again on COMPLETION (a long gen shouldn't leave us "idle" the
                // instant it ends).
                last_activity_ms.store(start.elapsed().as_millis() as u64, Ordering::SeqCst);

                match result {
                    Ok(text) => out.emit(&ChildMsg::Done { id, text }),
                    Err((kind, message)) => out.emit(&ChildMsg::Error { id, kind, message }),
                }
            }
            Ok(HostMsg::Shutdown) => break,
            Err(e) => {
                // NO PII: serde error only (structure, not the line's content-bearing fields).
                tracing::warn!(target: "brain", error = %e, "unparseable host line; skipping");
            }
        }
    }

    tracing::info!(target: "brain", "stdin closed / shutdown; exiting");
}

/// Spawn the heartbeat thread: while `in_generation`, emit `ChildMsg::Heartbeat{id}` every
/// ~`HEARTBEAT_INTERVAL` so the host treats its timeout as a liveness check, not a wall-clock
/// guillotine on a healthy long note-gen.
fn spawn_heartbeat(out: Out, in_generation: Arc<AtomicBool>, current_id: Arc<AtomicU64>) {
    std::thread::Builder::new()
        .name("brain-heartbeat".into())
        .spawn(move || loop {
            std::thread::sleep(HEARTBEAT_INTERVAL);
            if in_generation.load(Ordering::SeqCst) {
                let id = current_id.load(Ordering::SeqCst);
                out.emit(&ChildMsg::Heartbeat { id });
            }
        })
        .ok(); // a heartbeat-thread spawn failure is non-fatal (the host still has its own timeout).
}

/// Spawn the idle self-exit watchdog (orphan-safety belt; the host is the authoritative killer). After
/// `max_idle_seconds` with no activity AND no generation in flight, `exit(0)`. The timer is reset by the
/// main loop on every received line; this thread only READS it. It MUST NOT exit while `in_generation`.
fn spawn_idle_watchdog(
    max_idle_seconds: u64,
    start: Instant,
    last_activity_ms: Arc<AtomicU64>,
    in_generation: Arc<AtomicBool>,
) {
    // 0 (or absurdly small) disables the belt — the host is authoritative; never self-kill aggressively.
    if max_idle_seconds == 0 {
        return;
    }
    std::thread::Builder::new()
        .name("brain-idle-watchdog".into())
        .spawn(move || {
            let idle_ms = max_idle_seconds.saturating_mul(1000);
            // Poll at a fraction of the window, capped so we react promptly but never busy-spin.
            let poll = Duration::from_millis((idle_ms / 4).clamp(1000, 30_000));
            loop {
                std::thread::sleep(poll);
                if in_generation.load(Ordering::SeqCst) {
                    continue; // NEVER self-exit mid-generation.
                }
                let now_ms = start.elapsed().as_millis() as u64;
                let last = last_activity_ms.load(Ordering::SeqCst);
                if now_ms.saturating_sub(last) >= idle_ms {
                    tracing::info!(
                        target: "brain",
                        idle_s = max_idle_seconds,
                        "idle self-exit (orphan-safety belt)"
                    );
                    std::process::exit(0);
                }
            }
        })
        .ok(); // a watchdog spawn failure is non-fatal — the host's kill is authoritative anyway.
}

#[cfg(test)]
mod tests {
    use super::*;

    const GB: u64 = 1_073_741_824;

    /// The pure RAM decision: a healthy machine permits a big model; a pressured one refuses it.
    #[test]
    fn ram_permits_load_ok_when_free_and_refuses_under_pressure() {
        let big = 6 * GB + GB / 2; // ~6.3 GB
        assert!(ram_permits_load(Some(24 * GB), big), "24 GB free must permit a 6.3 GB model");
        assert!(!ram_permits_load(Some(4 * GB), big), "4 GB free must refuse a 6.3 GB model");
        let needed = big * MODEL_RAM_HEADROOM_NUM / MODEL_RAM_HEADROOM_DEN;
        assert!(ram_permits_load(Some(needed), big));
        assert!(!ram_permits_load(Some(needed - 1), big));
    }

    /// FAIL OPEN: a broken probe (`None`) must never block a load.
    #[test]
    fn ram_permits_load_fails_open_on_broken_probe() {
        assert!(ram_permits_load(None, 9 * GB));
    }

    /// A small model is never guarded — it loads even under pressure.
    #[test]
    fn ram_permits_load_small_model_always_ok() {
        let tiny = MODEL_RAM_GUARD_MIN_DISK_BYTES - 1;
        assert!(ram_permits_load(Some(100 * 1024 * 1024), tiny));
        assert!(!ram_permits_load(Some(GB), MODEL_RAM_GUARD_MIN_DISK_BYTES + GB));
    }

    /// The grammar-constraint size-threshold decision (the only headless-testable part).
    #[test]
    fn grammar_constraint_applies_only_when_opted_in_and_tiny() {
        assert!(!grammar_constraint_applies(false, 0));
        assert!(!grammar_constraint_applies(false, 100));
        assert!(grammar_constraint_applies(true, 0));
        assert!(grammar_constraint_applies(true, GRAMMAR_SCHEMA_MAX_BYTES - 1));
        assert!(!grammar_constraint_applies(true, GRAMMAR_SCHEMA_MAX_BYTES));
        assert!(!grammar_constraint_applies(true, GRAMMAR_SCHEMA_MAX_BYTES + 1));
        assert!(!grammar_constraint_applies(true, usize::MAX));
    }

    /// The RAM probe is crash-safe: `Some(>0)` or `None`, never a panic, never `Some(0)`.
    #[test]
    fn available_ram_probe_is_crash_safe() {
        if let Some(b) = available_ram_bytes() {
            assert!(b > 0, "a Some result must be a positive byte count");
        }
    }
}
