//! Shared NDJSON wire protocol between the app (host, `meetnotes_lib`) and the on-device brain
//! sidecar (child, `meetnotes-brain`).
//!
//! ONE source of truth: this file has NO mistralrs / app dependency (only serde + serde_json), and
//! is compiled into BOTH crates — the child owns it as a normal `mod`, and the host `#[path]`-includes
//! THIS exact file (see `reason/sidecar.rs`) so a wire-format drift is a COMPILE error, not a runtime
//! desync.
//!
//! ## Transport & framing (see `reason/sidecar.rs` / `main.rs` for the loop)
//! - Transport: the child's stdin/stdout PIPES only — never a socket, so the channel is not
//!   addressable off-box (local-first by construction). The prompt/transcript travels ONLY on stdin
//!   and is NEVER written to a temp file, argv, or a log line.
//! - Framing: newline-delimited JSON — exactly one [`HostMsg`] or [`ChildMsg`] object per line, UTF-8.
//! - stdout carries ONLY these messages; the child sends ALL diagnostics/log output to stderr.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Host → child, one JSON line per message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMsg {
    /// Optional handshake probe; answered with [`ChildMsg::Ready`].
    ReadyProbe,
    /// A single generation request. `json_schema.is_some()` selects the grammar-constrained
    /// structured path (mistralrs `Constraint::JsonSchema`); `None` = free-form completion.
    Generate {
        /// Monotonic request id — echoed on every [`ChildMsg`] for this request so the host can
        /// match replies and ignore stray/duplicate lines.
        id: u64,
        system: String,
        user: String,
        opts: GenOptsWire,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        json_schema: Option<Value>,
    },
    /// Ask the child to finish the current request (if any) and exit cleanly. The host may still
    /// SIGKILL if the child does not exit promptly.
    Shutdown,
}

/// Child → host, one JSON line per message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChildMsg {
    /// Emitted once, after the model has loaded — the readiness handshake answer.
    Ready { model_id: String },
    /// Liveness beat emitted periodically WHILE a generation is running, so the host can distinguish
    /// "productively generating a long note" from "wedged" and only kill on true silence rather than
    /// guillotining a healthy long decode at the wall-clock cap.
    Heartbeat { id: u64 },
    /// Terminal success for request `id` — the whole-string result (the one-shot contract the
    /// `LocalReasoner` trait expects). `text` is either free-form text or a JSON string per the
    /// request's `json_schema`.
    Done { id: u64, text: String },
    /// RESERVED — a streamed token delta. NOT emitted by the one-shot protocol; kept in the enum so a
    /// future FE token-stream is an additive change (no breaking wire bump).
    Token { id: u64, delta: String },
    /// Terminal failure for request `id`. `kind` maps 1:1 to an `AppError` variant on the host, so the
    /// caller never has to parse a bare string.
    Error {
        id: u64,
        kind: ErrorKind,
        message: String,
    },
}

/// The failure domain of a [`ChildMsg::Error`], mapped 1:1 to `AppError` on the host side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// Degrade to the floor / Cloud (e.g. model unavailable, generation could not complete).
    Unavailable,
    /// A generation/summarize failure → `AppError::Summarize`.
    Summarize,
    /// A malformed request → `AppError::InvalidArg`.
    InvalidArg,
    /// The child measured insufficient RAM to load the model → host degrades (→ `AppError::Unavailable`).
    Oom,
}

/// Wire form of the host's `GenOptions` — ONLY the fields the child's sampler needs. The wall-clock
/// `timeout` is deliberately absent: it is enforced HOST-side by killing the child (true cancellation
/// of an otherwise-uncancellable mistralrs generation), never sent to the child. `transcript_compaction`
/// is a host-only agentic-loop concern and is likewise not on the wire.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
pub struct GenOptsWire {
    /// Hard decode cap (mistralrs `set_sampler_max_len`); `None` = model default.
    #[serde(default)]
    pub max_tokens: Option<usize>,
    /// Sampling temperature; `None` = model default.
    #[serde(default)]
    pub temperature: Option<f64>,
    /// Allow qwen3 "thinking" traces (no-op on non-thinking models).
    #[serde(default)]
    pub enable_thinking: bool,
    /// Opt into the tiny-schema grammar constraint on the structured path.
    #[serde(default)]
    pub use_grammar_constraint: bool,
}
