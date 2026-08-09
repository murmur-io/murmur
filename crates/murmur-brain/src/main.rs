//! `murmur-brain` — Murmur's on-device GGUF LLM inference, isolated in a killable helper process.
//!
//! WHY this exists: mistralrs holds a multi-GB model resident, has documented drop-leaks (#723/#865)
//! so the in-process code could never evict, and its generation is not cancellable. Running it in a
//! child process turns all three into non-problems: the host reclaims ALL of this process's RAM by
//! simply killing it (idle-unload / per-request timeout / app-quit), and a hung generation is
//! cancelled by SIGKILL. This binary loads ONE model, answers NDJSON generation requests over
//! stdin/stdout, self-exits after an idle window, and independently exits when its spawning Murmur
//! parent dies — including while synchronous generation is stuck.
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

use mistralrs::{Constraint, GgufModelBuilder, Model, RequestBuilder, TextMessageRole};

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

/// Heartbeat cadence while a generation is running. Beats are progress telemetry only: the host
/// enforces one immutable per-request wall-clock deadline and never extends it on a heartbeat.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

#[cfg(target_os = "macos")]
#[repr(C)]
struct KEvent {
    ident: usize,
    filter: i16,
    flags: u16,
    fflags: u32,
    data: isize,
    udata: *mut std::ffi::c_void,
}

#[cfg(target_os = "macos")]
#[repr(C)]
struct TimeSpec {
    tv_sec: i64,
    tv_nsec: i64,
}

#[cfg(target_os = "macos")]
const EVFILT_READ: i16 = -1;
#[cfg(target_os = "macos")]
const EV_ADD: u16 = 0x0001;
#[cfg(target_os = "macos")]
const EV_ENABLE: u16 = 0x0004;
#[cfg(target_os = "macos")]
const EV_CLEAR: u16 = 0x0020;
#[cfg(target_os = "macos")]
const EV_EOF: u16 = 0x8000;
#[cfg(target_os = "macos")]
const EV_ERROR: u16 = 0x4000;

#[cfg(target_os = "macos")]
fn stdin_watchdog_event_requires_exit(flags: u16) -> bool {
    flags & (EV_EOF | EV_ERROR) != 0
}

#[cfg(target_os = "macos")]
fn wait_for_parent_pipe_close(fd: i32, timeout_ms: i32) -> std::io::Result<bool> {
    use std::os::fd::FromRawFd;

    extern "C" {
        fn kqueue() -> i32;
        fn kevent(
            kq: i32,
            changelist: *const KEvent,
            nchanges: i32,
            eventlist: *mut KEvent,
            nevents: i32,
            timeout: *const TimeSpec,
        ) -> i32;
    }

    // kqueue's EVFILT_READ reports EV_EOF independently of unread bytes. poll(2) cannot provide
    // that guarantee on macOS: a closed pipe with buffered NDJSON can remain merely readable until
    // somebody consumes it, which is exactly what this watchdog must never do.
    // SAFETY: kqueue(2) has no arguments and returns a fresh owned descriptor on success.
    let queue_fd = unsafe { kqueue() };
    if queue_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `queue_fd` is freshly owned and wrapped exactly once so every return path closes it.
    let _queue = unsafe { std::fs::File::from_raw_fd(queue_fd) };

    let change = KEvent {
        ident: fd as usize,
        filter: EVFILT_READ,
        flags: EV_ADD | EV_ENABLE | EV_CLEAR,
        fflags: 0,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    let deadline =
        (timeout_ms >= 0).then(|| Instant::now() + Duration::from_millis(timeout_ms as u64));
    let mut register = true;

    loop {
        let remaining = deadline.map(|limit| limit.saturating_duration_since(Instant::now()));
        let timeout = remaining.map(|duration| TimeSpec {
            tv_sec: duration.as_secs() as i64,
            tv_nsec: i64::from(duration.subsec_nanos()),
        });
        let timeout_ptr = timeout
            .as_ref()
            .map_or(std::ptr::null(), |value| value as *const TimeSpec);
        let mut event = KEvent {
            ident: 0,
            filter: 0,
            flags: 0,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: both optional changelist storage and event storage remain valid for the call;
        // kevent only observes `fd` and never consumes bytes from it. EV_CLEAR prevents buffered
        // protocol data from causing a busy loop while still delivering the later EV_EOF edge.
        let count = unsafe {
            kevent(
                queue_fd,
                if register { &change } else { std::ptr::null() },
                i32::from(register),
                &mut event,
                1,
                timeout_ptr,
            )
        };
        if count > 0 {
            register = false;
            if event.flags & EV_ERROR != 0 {
                return Err(std::io::Error::from_raw_os_error(event.data as i32));
            }
            if stdin_watchdog_event_requires_exit(event.flags) {
                return Ok(true);
            }
            if deadline.is_some_and(|limit| Instant::now() >= limit) {
                return Ok(false);
            }
            continue;
        }
        if count == 0 {
            return Ok(false);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(error);
    }
}

/// Install the child-owned parent-lifetime guard BEFORE loading the model. The host owns the sole
/// write end of this child's stdin pipe for exactly the sidecar lifetime. Watching fd 0 with
/// kqueue/EVFILT_READ never consumes or competes with the NDJSON reader, while EV_EOF is delivered
/// even when protocol bytes remain buffered. Closing/crashing the host therefore revokes this exact
/// capability without any pid lookup or reuse race. The dedicated waiter `_exit`s even if mistralrs
/// is wedged.
#[cfg(target_os = "macos")]
fn spawn_parent_watchdog() -> std::io::Result<()> {
    use std::os::fd::FromRawFd;
    use std::os::unix::fs::FileTypeExt;

    extern "C" {
        fn _exit(status: i32) -> !;
    }

    // `Command::stdin(Stdio::piped())` supplies a FIFO. Accept only that shipped transport: a
    // terminal, regular file, or arbitrary socket does not by itself prove one connected Murmur
    // owner's lifetime and could strand the model after the parent dies.
    // SAFETY: fd 0 belongs to the process. ManuallyDrop borrows it for metadata without closing it;
    // the blocking NDJSON reader and watchdog continue sharing the same open file description.
    let stdin = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(0) });
    let stdin_type = stdin.metadata()?.file_type();
    if !stdin_type.is_fifo() {
        return Err(std::io::Error::other(
            "brain stdin is not the parent-owned pipe",
        ));
    }

    // Fail closed before runtime/model allocation if stdin is already invalid or disconnected.
    if wait_for_parent_pipe_close(0, 0)? {
        return Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "brain parent stdin pipe is already closed",
        ));
    }

    let waiter = std::thread::Builder::new()
        .name("brain-parent-watchdog".into())
        .spawn(move || {
            let status = match wait_for_parent_pipe_close(0, -1) {
                Ok(true) => 0,
                Ok(false) | Err(_) => 1,
            };
            // `_exit`, not unwinding/destructors: inference may be wedged while holding arbitrary
            // model/runtime locks. The OS reclaims the entire address space.
            // SAFETY: deliberately terminates only THIS helper process.
            unsafe { _exit(status) }
        })?;

    // Dropping the JoinHandle detaches the process-lifetime watchdog. HUP state is persistent, so a
    // parent close between spawn and the first `poll` is observed immediately; there is no arm race.
    drop(waiter);
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn spawn_parent_watchdog() -> std::io::Result<()> {
    Err(std::io::Error::other(
        "brain parent-pipe watchdog is unsupported on this platform",
    ))
}

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
    let model_dir = path.parent().map(Path::to_path_buf).ok_or_else(|| {
        (
            ErrorKind::InvalidArg,
            "brain model path has no parent dir".into(),
        )
    })?;
    let model_file = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| {
            (
                ErrorKind::InvalidArg,
                "brain model path has no filename".into(),
            )
        })?
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
    .with_prefix_cache_n(None);

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
        let schema_bytes = serde_json::to_string(schema)
            .map(|s| s.len())
            .unwrap_or(usize::MAX);
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
        Err(_) => Err((ErrorKind::Summarize, "brain generation panicked".into())),
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

    // Install this BEFORE the runtime/model load: a parent crash during a slow Metal/model startup
    // must not strand the multi-GB helper any more than a crash during generation may.
    if let Err(error) = spawn_parent_watchdog() {
        tracing::error!(target: "brain", %error, "parent watchdog unavailable");
        out.emit(&ChildMsg::Error {
            id: 0,
            kind: ErrorKind::Unavailable,
            message: "brain parent watchdog unavailable".into(),
        });
        std::process::exit(1);
    }

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

    // Idle self-exit belt (resident-RAM hygiene) + heartbeat driver both watch shared atomics. The
    // independent parent watchdog above owns orphan safety even while generation is in flight.
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
    spawn_heartbeat(out.clone(), in_generation.clone(), current_id.clone());

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
/// ~`HEARTBEAT_INTERVAL` as progress telemetry. The host deliberately does not extend the request's
/// immutable hard deadline when it receives one.
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

/// Spawn the idle self-exit watchdog (resident-RAM hygiene; the host is the authoritative killer).
/// After `max_idle_seconds` with no activity AND no generation in flight, `exit(0)`. The timer is
/// reset by the main loop on every received line; this thread only READS it. It MUST NOT exit while
/// `in_generation`; parent-pipe HUP independently handles an orphan during generation.
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
                        "idle self-exit (resident-RAM hygiene belt)"
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

    #[test]
    fn parent_pipe_watchdog_exits_only_on_terminal_kqueue_events() {
        assert!(stdin_watchdog_event_requires_exit(EV_EOF));
        assert!(stdin_watchdog_event_requires_exit(EV_ERROR));
        assert!(stdin_watchdog_event_requires_exit(EV_EOF | EV_ERROR));
        assert!(!stdin_watchdog_event_requires_exit(0));
        assert!(!stdin_watchdog_event_requires_exit(EV_CLEAR));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parent_pipe_watch_ignores_protocol_bytes_then_observes_writer_close() {
        use std::fs::File;
        use std::io::Write;
        use std::os::fd::{AsRawFd, FromRawFd};

        extern "C" {
            fn pipe(fds: *mut i32) -> i32;
        }

        // Match the shipped `Command::stdin(Stdio::piped())` transport exactly. A UnixStream is a
        // subtly wrong fixture on macOS: when unread bytes remain, a socket peer close may surface
        // as readable EOF rather than POLLHUP, while an anonymous pipe reports the terminal event
        // independently of POLLIN as the watchdog relies on.
        let mut fds = [-1_i32; 2];
        // SAFETY: `fds` is valid writable storage for both descriptors returned by pipe(2).
        assert_eq!(
            unsafe { pipe(fds.as_mut_ptr()) },
            0,
            "create watchdog fixture pipe"
        );
        // SAFETY: pipe(2) returned two fresh owned descriptors; each is wrapped exactly once.
        let reader = unsafe { File::from_raw_fd(fds[0]) };
        let mut sole_writer = unsafe { File::from_raw_fd(fds[1]) };

        sole_writer
            .write_all(b"protocol bytes stay reader-owned\n")
            .expect("write protocol fixture");
        assert!(
            !wait_for_parent_pipe_close(reader.as_raw_fd(), 0).expect("probe open parent pipe"),
            "unread protocol data must not wake the events=0 watchdog"
        );

        drop(sole_writer);
        assert!(
            wait_for_parent_pipe_close(reader.as_raw_fd(), 1_000)
                .expect("wait for parent-pipe close"),
            "closing the sole writer must surface HUP without consuming protocol bytes"
        );
    }

    /// The pure RAM decision: a healthy machine permits a big model; a pressured one refuses it.
    #[test]
    fn ram_permits_load_ok_when_free_and_refuses_under_pressure() {
        let big = 6 * GB + GB / 2; // ~6.3 GB
        assert!(
            ram_permits_load(Some(24 * GB), big),
            "24 GB free must permit a 6.3 GB model"
        );
        assert!(
            !ram_permits_load(Some(4 * GB), big),
            "4 GB free must refuse a 6.3 GB model"
        );
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
        assert!(!ram_permits_load(
            Some(GB),
            MODEL_RAM_GUARD_MIN_DISK_BYTES + GB
        ));
    }

    /// The grammar-constraint size-threshold decision (the only headless-testable part).
    #[test]
    fn grammar_constraint_applies_only_when_opted_in_and_tiny() {
        assert!(!grammar_constraint_applies(false, 0));
        assert!(!grammar_constraint_applies(false, 100));
        assert!(grammar_constraint_applies(true, 0));
        assert!(grammar_constraint_applies(
            true,
            GRAMMAR_SCHEMA_MAX_BYTES - 1
        ));
        assert!(!grammar_constraint_applies(true, GRAMMAR_SCHEMA_MAX_BYTES));
        assert!(!grammar_constraint_applies(
            true,
            GRAMMAR_SCHEMA_MAX_BYTES + 1
        ));
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
