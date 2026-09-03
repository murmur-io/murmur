//! Localhost MCP server (HTTP, 127.0.0.1 only) exposing the user's meetings to MCP clients
//! (Claude Desktop / Code) with NO egress. Read-only tools over the SQLite DB. Implements the
//! MCP JSON-RPC essentials (initialize / tools/list / tools/call) over HTTP POST.

use std::collections::HashMap;
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddrV4, TcpListener, TcpStream};
#[cfg(test)]
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use subtle::ConstantTimeEq;
use tauri::{AppHandle, Manager};

use crate::state::AppState;
use crate::storage::Db;

/// Fixed localhost port for the MCP server.
pub const MCP_PORT: u16 = 8765;
/// The production listener has no configurable/wildcard bind. Keeping the literal loopback IP in a
/// typed socket address prevents DNS, proxy, environment, and config state from widening this
/// local-only disclosure boundary.
const MCP_BIND_IP: Ipv4Addr = Ipv4Addr::LOCALHOST;

fn mcp_listener_addr() -> SocketAddrV4 {
    SocketAddrV4::new(MCP_BIND_IP, MCP_PORT)
}

/// How long to wait before re-attempting a bind that lost the port to another process.
///
/// The port is FIXED (it is baked into [`ALLOWED_HOSTS`], the `Origin` allowlist, and the config
/// the user pastes into Claude), so "pick another port" is not available — the only recovery is to
/// take 8765 once whoever else has it lets go. Thirty seconds is far below a user's patience for
/// "I quit that app, why is it still broken" and far above anything that would show up as load.
const BIND_RETRY_INTERVAL: Duration = Duration::from_secs(30);

/// What the MCP listener is actually doing, for the one screen that tells the user about it.
///
/// # Why this exists
///
/// A bind failure used to be a `tracing::warn!` and a dead thread. Nothing else in the app knew,
/// so Settings kept saying "Murmur runs a small server on this Mac … at 127.0.0.1:8765" in the
/// present tense and kept offering a config to paste — while the user's Claude was talking to
/// whatever else held the port. Found 2026-08-28 on a real machine where an unrelated
/// `python -m http.server 8765` from another project had held it for two days: the SHIPPED app's
/// log carried the same warning, and nothing on screen said so.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpListenerState {
    /// The thread is up but has not bound yet (the first attempt is in flight).
    Starting,
    /// Bound and serving.
    Listening,
    /// Another process on this Mac holds the port. Retried every [`BIND_RETRY_INTERVAL`].
    PortInUse,
    /// A terminal failure that retrying cannot fix (no token, no response gate, a bind error that
    /// is not `AddrInUse`). Deliberately distinct from [`Self::PortInUse`]: the user action is
    /// different, and telling someone to close another app when the real cause is a Keychain
    /// refusal sends them hunting for a process that does not exist.
    Unavailable,
}

impl McpListenerState {
    fn as_code(self) -> u8 {
        match self {
            Self::Starting => 0,
            Self::Listening => 1,
            Self::PortInUse => 2,
            Self::Unavailable => 3,
        }
    }

    fn from_code(code: u8) -> Self {
        match code {
            1 => Self::Listening,
            2 => Self::PortInUse,
            3 => Self::Unavailable,
            _ => Self::Starting,
        }
    }

    /// The wire value the frontend branches on. camelCase to match every other IPC payload.
    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Listening => "listening",
            Self::PortInUse => "portInUse",
            Self::Unavailable => "unavailable",
        }
    }
}

/// Tauri-managed handle to [`McpListenerState`], written by the listener thread and read by
/// `commands::get_mcp_status`. An atomic rather than a mutex: the listener writes it from its own
/// thread while the command reads it from the IPC thread, and a lock here could only ever add a
/// way for the status read to block behind the retry loop.
#[derive(Debug, Default)]
pub struct McpListenerStatus(AtomicU8);

impl McpListenerStatus {
    pub fn get(&self) -> McpListenerState {
        McpListenerState::from_code(self.0.load(Ordering::Relaxed))
    }

    fn set(&self, state: McpListenerState) {
        self.0.store(state.as_code(), Ordering::Relaxed);
    }
}

/// Max request body we will read (E5). The MCP JSON-RPC requests are tiny; cap hard so a
/// malicious local client can't OOM us with an unbounded body.
const MAX_BODY_BYTES: u64 = 1 << 20; // 1 MiB
const MAX_HEADER_BYTES: usize = 32 << 10;
const MAX_REQUEST_LINE_BYTES: usize = 8 << 10;
const MAX_HEADER_COUNT: usize = 64;
const MAX_RESPONSE_BYTES: usize = 8 << 20;
// A JSON string byte can expand to six bytes (`\u00XX`). Keeping tool text to one MiB leaves
// ample room for that worst-case escaping plus the bounded JSON-RPC id and envelope.
const MAX_TOOL_TEXT_BYTES: usize = 1 << 20;
const MAX_TOOL_WINDOW_CHARS: usize = MAX_TOOL_TEXT_BYTES / 4;
const MAX_JSONRPC_ID_BYTES: usize = 256;
const RESPONSE_CHUNK_BYTES: usize = 4096;
const MAX_ACTIVE_CONNECTIONS: usize = 32;
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const REQUEST_DEADLINE: Duration = Duration::from_secs(10);

/// The only `Host` header values we accept (E2). A request whose Host is anything else — a DNS
/// name resolving to 127.0.0.1, a `0.0.0.0` rebinding, an external host — is rejected, which
/// blocks DNS-rebinding attacks against the localhost server.
const ALLOWED_HOSTS: &[&str] = &["127.0.0.1:8765", "localhost:8765"];

/// Content-free JSON-RPC refusal returned when the lock/session lifecycle changed after a tool
/// materialized content but before the server could send it. Never include the tool name, meeting
/// id, title, query, or any dispatch error here: the stale payload is discarded as a whole.
const VISIBILITY_RETRY_CODE: i64 = -32002;
const VISIBILITY_RETRY_MESSAGE: &str = "content visibility changed; retry the request";

#[derive(Debug, PartialEq, Eq)]
struct VisibilitySnapshot {
    seal_epoch: u64,
    unlocked_folders: HashSet<String>,
    ask_dispatch_generation: Option<i64>,
}

/// The exact shared visibility authority used for one MCP request. Production constructs this only
/// from Tauri's managed [`AppState`]; tests inject the same three content-free synchronization
/// primitives without needing a real application/runtime.
struct RpcContext<'a> {
    db: Option<&'a Db>,
    lifecycle: &'a Mutex<()>,
    seal_epoch: &'a AtomicU64,
    unlocked_folders: &'a Mutex<HashSet<String>>,
}

impl<'a> RpcContext<'a> {
    fn from_state(state: &'a AppState) -> Self {
        Self {
            db: Some(&state.db),
            lifecycle: &state.lifecycle,
            seal_epoch: &state.seal_epoch,
            unlocked_folders: state.unlocked_folders.as_ref(),
        }
    }

    /// Capture epoch + live unlock membership while excluding every lock lifecycle mutation.
    /// A poisoned unlock-set mutex fails closed instead of silently substituting an empty set.
    fn visibility_snapshot(
        &self,
        deadline: Instant,
    ) -> std::result::Result<VisibilitySnapshot, ()> {
        let _lifecycle = lock_before_deadline(self.lifecycle, deadline, true)?;
        let unlocked_folders =
            lock_before_deadline(self.unlocked_folders, deadline, false)?.clone();
        Ok(VisibilitySnapshot {
            seal_epoch: self.seal_epoch.load(Ordering::SeqCst),
            unlocked_folders,
            ask_dispatch_generation: match self.db {
                Some(db) => Some(db.ask_dispatch_generation().map_err(|_| ())?),
                None => None,
            },
        })
    }
}

/// Acquire a synchronization boundary without letting mutex contention silently extend the
/// connection's one absolute deadline. Lifecycle poisoning is recoverable because the mutex
/// carries no data; content-bearing mutexes fail closed on poison.
fn lock_before_deadline<'a, T>(
    mutex: &'a Mutex<T>,
    deadline: Instant,
    recover_poison: bool,
) -> std::result::Result<MutexGuard<'a, T>, ()> {
    loop {
        match mutex.try_lock() {
            Ok(guard) => return Ok(guard),
            Err(TryLockError::Poisoned(error)) if recover_poison => return Ok(error.into_inner()),
            Err(TryLockError::Poisoned(_)) => return Err(()),
            Err(TryLockError::WouldBlock) => {
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    return Err(());
                };
                if remaining.is_zero() {
                    return Err(());
                }
                std::thread::sleep(remaining.min(Duration::from_millis(1)));
            }
        }
    }
}

enum RpcReply {
    Immediate(Value),
    Content(PendingContentReply),
}

struct PendingContentReply {
    id: Value,
    snapshot: VisibilitySnapshot,
    outcome: std::result::Result<String, ToolError>,
}

#[derive(Serialize)]
struct BorrowedTextContent<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    text: &'a str,
}

#[derive(Serialize)]
struct BorrowedTextResult<'a> {
    content: [BorrowedTextContent<'a>; 1],
}

#[derive(Serialize)]
struct BorrowedTextResponse<'a> {
    jsonrpc: &'static str,
    id: &'a Value,
    result: BorrowedTextResult<'a>,
}

#[derive(Serialize)]
struct BorrowedRpcErrorBody<'a> {
    code: i64,
    message: &'a str,
}

#[derive(Serialize)]
struct BorrowedRpcErrorResponse<'a> {
    jsonrpc: &'static str,
    id: &'a Value,
    error: BorrowedRpcErrorBody<'a>,
}

struct ActiveResponse {
    stream: Arc<TcpStream>,
    cancellation: Arc<ResponseCancellation>,
}

struct ResponseCancellation {
    state: Mutex<ResponseWriteState>,
    drained: Condvar,
}

struct ResponseWriteState {
    cancelled: bool,
    in_flight_chunks: usize,
}

struct ResponseGateState {
    open: bool,
    revocations_in_flight: u64,
    next_id: u64,
    active: HashMap<u64, ActiveResponse>,
}

/// Tauri-managed cancellation authority for content-bearing MCP responses.
///
/// Registering a stream is a short mutex-only operation performed while the caller holds
/// [`AppState::lifecycle`]. Revocation flips every lease's content-free cancellation bit and takes
/// the cloned sockets while holding this mutex, then releases it before calling `shutdown(2)`.
/// Consequently neither the lifecycle mutex nor this gate mutex is ever held across network I/O.
///
/// A successful TCP write is the disclosure commit point: bytes already accepted by the kernel
/// were sent while the client was authorized and cannot be recalled. Revocation prevents any new
/// chunk syscall from starting and waits for every already-admitted syscall to return before the
/// logical visibility transition, so that transition linearizes after all pre-revocation commits.
pub(crate) struct McpResponseGate {
    inner: Mutex<ResponseGateState>,
}

impl McpResponseGate {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(ResponseGateState {
                open: true,
                revocations_in_flight: 0,
                next_id: 1,
                active: HashMap::new(),
            }),
        }
    }

    fn register(self: &Arc<Self>, stream: TcpStream) -> Option<ResponseLease> {
        let cancellation = Arc::new(ResponseCancellation {
            state: Mutex::new(ResponseWriteState {
                cancelled: false,
                in_flight_chunks: 0,
            }),
            drained: Condvar::new(),
        });
        let id = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !inner.open {
                return None;
            }
            let mut id = inner.next_id.max(1);
            while inner.active.contains_key(&id) {
                id = id.wrapping_add(1).max(1);
            }
            inner.next_id = id.wrapping_add(1).max(1);
            inner.active.insert(
                id,
                ActiveResponse {
                    stream: Arc::new(stream),
                    cancellation: Arc::clone(&cancellation),
                },
            );
            id
        };
        Some(ResponseLease {
            gate: Arc::clone(self),
            id,
            cancellation,
        })
    }

    /// Close admission and cancel every registered content response. Socket shutdown happens only
    /// after the gate mutex has been released, so a slow client can never pin a lock lifecycle.
    pub(crate) fn close_and_shutdown(&self) {
        let active = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            inner.revocations_in_flight = inner.revocations_in_flight.saturating_add(1);
            inner.open = false;
            inner
                .active
                .iter()
                .map(|(id, active)| {
                    (
                        *id,
                        Arc::clone(&active.stream),
                        Arc::clone(&active.cancellation),
                    )
                })
                .collect::<Vec<_>>()
        };
        // Cancel every response before potentially waiting on any one of them. Then shutdown all
        // socket clones so a checked write already inside the kernel returns promptly. Finally wait
        // until those checked chunks have left their syscalls. A successful pre-cancel syscall is
        // already a disclosure to the then-authorized client; TCP cannot retract it. Only after all
        // such syscalls drain may the caller perform the logical visibility transition.
        for (_, _, cancellation) in &active {
            cancellation.cancel();
        }
        for (_, stream, _) in &active {
            let _ = stream.shutdown(Shutdown::Both);
        }
        for (_, _, cancellation) in &active {
            cancellation.wait_until_drained();
        }
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (id, _, cancellation) in active {
            if inner
                .active
                .get(&id)
                .is_some_and(|active| Arc::ptr_eq(&active.cancellation, &cancellation))
            {
                inner.active.remove(&id);
            }
        }
    }

    /// Reopen content response admission after the caller completed logical visibility revocation.
    fn finish_revocation(&self) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.revocations_in_flight = inner.revocations_in_flight.saturating_sub(1);
        inner.open = inner.revocations_in_flight == 0;
    }

    #[cfg(test)]
    fn active_count(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active
            .len()
    }
}

struct ResponseLease {
    gate: Arc<McpResponseGate>,
    id: u64,
    cancellation: Arc<ResponseCancellation>,
}

/// One visibility-revocation admission lease. Dropping it without `complete` deliberately leaves
/// the gate closed (fail closed after a failed/poisoned logical revocation). Concurrent relocks
/// each own a lease, so one cannot reopen response admission while another still waits on the
/// lifecycle mutex.
pub(crate) struct VisibilityRevocation {
    gate: Option<Arc<McpResponseGate>>,
}

/// Every production lock entrypoint that can make previously readable local content invisible.
/// Passing the reason at the gate boundary makes the call-chain audit finite and testable: an
/// epoch bump that merely expands visibility (`remove_lock`) or preserves it (`move_note` into a
/// session-unlocked folder) is intentionally not in this list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VisibilityRevokingEntrypoint {
    LockFolder,
    RelockFolder,
    RelockAll,
}

impl VisibilityRevocation {
    pub(crate) fn complete(mut self) {
        if let Some(gate) = self.gate.take() {
            gate.finish_revocation();
        }
    }
}

impl ResponseLease {
    #[cfg(test)]
    fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    fn begin_chunk(&self) -> Option<ChunkWriteGuard> {
        self.cancellation.begin_chunk()
    }
}

impl Drop for ResponseLease {
    fn drop(&mut self) {
        let mut inner = self
            .gate
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if inner
            .active
            .get(&self.id)
            .is_some_and(|active| Arc::ptr_eq(&active.cancellation, &self.cancellation))
        {
            inner.active.remove(&self.id);
        }
        drop(inner);
        self.cancellation.cancel();
    }
}

struct ChunkWriteGuard {
    cancellation: Arc<ResponseCancellation>,
}

impl ResponseCancellation {
    fn begin_chunk(self: &Arc<Self>) -> Option<ChunkWriteGuard> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.cancelled {
            return None;
        }
        state.in_flight_chunks = state.in_flight_chunks.saturating_add(1);
        Some(ChunkWriteGuard {
            cancellation: Arc::clone(self),
        })
    }

    fn cancel(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cancelled = true;
    }

    #[cfg(test)]
    fn is_cancelled(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .cancelled
    }

    fn wait_until_drained(&self) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while state.in_flight_chunks != 0 {
            state = self
                .drained
                .wait(state)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

impl Drop for ChunkWriteGuard {
    fn drop(&mut self) {
        let mut state = self
            .cancellation
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.in_flight_chunks = state.in_flight_chunks.saturating_sub(1);
        if state.in_flight_chunks == 0 {
            self.cancellation.drained.notify_all();
        }
    }
}

struct ConnectionPermit {
    active: Arc<AtomicUsize>,
}

impl Drop for ConnectionPermit {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Debug)]
struct ParsedHttpRequest {
    method: String,
    headers: Vec<(String, String)>,
    body: String,
}

#[derive(Clone, Copy, Debug)]
struct HttpParseError {
    status: u16,
    message: &'static str,
}

/// The only `Origin` header values we accept when one is present (E5). A browser page on another
/// origin (or a `null` opaque origin) must not be able to script this server. Requests with NO
/// Origin (native MCP clients like Claude Desktop/Code) are allowed through — Origin is a
/// browser-set header. We never reflect the Origin back.
fn origin_allowed(origin: &str) -> bool {
    matches!(
        origin,
        "http://127.0.0.1:8765" | "http://localhost:8765" | "http://127.0.0.1" | "http://localhost"
    )
}

fn single_header<'a>(
    headers: &'a [(String, String)],
    name: &str,
) -> std::result::Result<Option<&'a str>, HttpParseError> {
    let mut values = headers
        .iter()
        .filter(|(field, _)| field.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str());
    let first = values.next();
    if values.next().is_some() {
        return Err(HttpParseError {
            status: 400,
            message: "duplicate security header",
        });
    }
    Ok(first)
}

/// Spawn the MCP server on a background thread. Best-effort: a bind failure is logged; the app
/// continues normally. The background thread resolves Tauri's managed [`AppState`] for every
/// request, so DB reads, the session unlock set, lifecycle guard, and seal epoch are the SAME
/// authority as the command surface. `require_token` gates `tools/call` behind a bearer token
/// (default ON —
/// `AppConfig::mcp_require_token` defaults `true`, and `lib.rs` fails CLOSED to `true` on a
/// poisoned/unreadable config).
pub fn spawn(app: AppHandle, require_token: bool) {
    let _ = std::thread::Builder::new()
        .name("murmur-mcp".into())
        .spawn(move || run(app, require_token));
}

fn run(app: AppHandle, require_token: bool) {
    let addr = mcp_listener_addr();
    let status = app
        .try_state::<Arc<McpListenerStatus>>()
        .map(|handle| Arc::clone(handle.inner()));
    let report = |state: McpListenerState| {
        if let Some(status) = status.as_ref() {
            status.set(state);
        }
    };
    report(McpListenerState::Starting);
    // RETRY, don't give up. The port is fixed, so losing it to another local process is not a
    // permanent condition — it lasts exactly as long as that process does. Before this loop the
    // thread returned on the first `AddrInUse` and MCP stayed dead until the user restarted
    // Murmur, which nothing on screen ever told them to do.
    let listener = loop {
        match TcpListener::bind(addr) {
            Ok(s) => break s,
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                report(McpListenerState::PortInUse);
                tracing::warn!(
                    target: "mcp",
                    "MCP server port {MCP_PORT} is held by another process on this Mac; retrying"
                );
                std::thread::sleep(BIND_RETRY_INTERVAL);
            }
            Err(e) => {
                report(McpListenerState::Unavailable);
                tracing::warn!(target: "mcp", error = %e, "MCP server failed to bind {addr}");
                return;
            }
        }
    };
    // The expected token, if enforcement is on. Minted/persisted in the Keychain on first use.
    let expected_token = if require_token {
        match crate::secrets::get_or_create_mcp_token() {
            Ok(t) => Some(t),
            Err(e) => {
                // FAIL CLOSED (E3): enforcement is required but the token could not be minted/read.
                // Do NOT fall back to an unauthenticated server — that would serve the whole tool
                // surface ungated. Refuse to start the MCP listener so the gate can never be
                // bypassed by a transient Keychain failure.
                report(McpListenerState::Unavailable);
                tracing::error!(target: "mcp", error = %e, "MCP token required but unavailable — refusing to start the MCP server (fail closed)");
                return;
            }
        }
    } else {
        None
    };
    let Some(gate) = app.try_state::<Arc<McpResponseGate>>() else {
        report(McpListenerState::Unavailable);
        tracing::error!(target: "mcp", "MCP response gate is unavailable — refusing to start");
        return;
    };
    let gate = Arc::clone(gate.inner());
    let expected_token = expected_token.map(Arc::new);
    let active_connections = Arc::new(AtomicUsize::new(0));
    report(McpListenerState::Listening);
    tracing::info!(target: "mcp", "MCP server listening on http://{addr}");
    loop {
        let app = app.clone();
        let gate = Arc::clone(&gate);
        let expected_token = expected_token.clone();
        let accepted = accept_bounded_connection(
            &listener,
            Arc::clone(&active_connections),
            REQUEST_DEADLINE,
            move |stream, deadline| {
                handle_connection(stream, app, gate, expected_token, deadline);
            },
            |job| {
                std::thread::Builder::new()
                    .name("murmur-mcp-connection".into())
                    .spawn(job)
                    .map(|_| ())
            },
        );
        if let Err(error) = accepted {
            tracing::warn!(target: "mcp", error = %error, "MCP listener/worker admission failed");
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectionAdmission {
    Spawned,
    RejectedOverload,
}

/// The production accept/admission boundary, factored only so the complete listener -> permit ->
/// worker wiring can be exercised on an ephemeral loopback port. The absolute deadline begins
/// immediately after `accept`, before overload handling or thread creation.
fn accept_bounded_connection<F, S>(
    listener: &TcpListener,
    active: Arc<AtomicUsize>,
    request_budget: Duration,
    handle: F,
    spawn: S,
) -> std::io::Result<ConnectionAdmission>
where
    F: FnOnce(TcpStream, Instant) + Send + 'static,
    S: FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
{
    let (mut stream, _) = listener.accept()?;
    let deadline = Instant::now() + request_budget;
    if !try_acquire_connection(&active) {
        reject_overloaded_connection(&mut stream, deadline)?;
        return Ok(ConnectionAdmission::RejectedOverload);
    }
    spawn_connection_worker(active, move || handle(stream, deadline), spawn)?;
    Ok(ConnectionAdmission::Spawned)
}

/// Keep the accept-loop overload path under the same per-syscall and absolute write deadline as
/// worker responses. `write_http_response` arms the remaining deadline before its first syscall.
fn reject_overloaded_connection(stream: &mut TcpStream, deadline: Instant) -> std::io::Result<()> {
    write_http_response(
        stream,
        503,
        "text/plain; charset=utf-8",
        b"too many active connections",
        None,
        deadline,
    )
}

fn spawn_connection_worker<F, S>(active: Arc<AtomicUsize>, job: F, spawn: S) -> std::io::Result<()>
where
    F: FnOnce() + Send + 'static,
    S: FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<()>,
{
    // Construct the permit before handing the closure to the spawner. If thread creation fails,
    // the rejected closure is dropped and the captured permit releases the already-counted slot.
    let permit = ConnectionPermit { active };
    spawn(Box::new(move || {
        let _permit = permit;
        job();
    }))
}

fn try_acquire_connection(active: &AtomicUsize) -> bool {
    let mut current = active.load(Ordering::SeqCst);
    loop {
        if current >= MAX_ACTIVE_CONNECTIONS {
            return false;
        }
        match active.compare_exchange(current, current + 1, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
}

fn handle_connection(
    stream: TcpStream,
    app: AppHandle,
    gate: Arc<McpResponseGate>,
    expected_token: Option<Arc<String>>,
    deadline: Instant,
) {
    let state = app.state::<AppState>();
    handle_connection_with_state(stream, state.inner(), gate, expected_token, deadline);
}

/// Production connection pipeline with the Tauri state lookup factored out. Keeping HTTP parsing,
/// host/origin/auth checks, JSON-RPC dispatch, visibility revalidation, and the response gate in one
/// function lets an ephemeral-loopback test exercise the real listener path without constructing a
/// GUI runtime.
fn handle_connection_with_state(
    mut stream: TcpStream,
    state: &AppState,
    gate: Arc<McpResponseGate>,
    expected_token: Option<Arc<String>>,
    deadline: Instant,
) {
    if stream.set_read_timeout(Some(READ_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(WRITE_TIMEOUT)).is_err()
    {
        return;
    }
    let request = match parse_http_request(&mut stream, deadline) {
        Ok(request) => request,
        Err(error) => {
            let _ = write_http_response(
                &mut stream,
                error.status,
                "text/plain; charset=utf-8",
                error.message.as_bytes(),
                None,
                deadline,
            );
            return;
        }
    };
    if request.method != "POST" {
        let _ = write_http_response(
            &mut stream,
            200,
            "text/plain; charset=utf-8",
            b"Murmur MCP server - POST JSON-RPC here.",
            None,
            deadline,
        );
        return;
    }

    match single_header(&request.headers, "Host") {
        Ok(Some(host)) if ALLOWED_HOSTS.contains(&host.trim()) => {}
        _ => {
            let _ = write_http_response(
                &mut stream,
                403,
                "text/plain; charset=utf-8",
                b"forbidden host",
                None,
                deadline,
            );
            return;
        }
    }
    match single_header(&request.headers, "Origin") {
        Ok(Some(origin)) if !origin_allowed(origin.trim()) => {
            let _ = write_http_response(
                &mut stream,
                403,
                "text/plain; charset=utf-8",
                b"forbidden origin",
                None,
                deadline,
            );
            return;
        }
        Err(_) => {
            let _ = write_http_response(
                &mut stream,
                400,
                "text/plain; charset=utf-8",
                b"duplicate security header",
                None,
                deadline,
            );
            return;
        }
        _ => {}
    }
    let auth = match single_header(&request.headers, "Authorization") {
        Ok(auth) => auth,
        Err(error) => {
            let _ = write_http_response(
                &mut stream,
                error.status,
                "text/plain; charset=utf-8",
                error.message.as_bytes(),
                None,
                deadline,
            );
            return;
        }
    };
    let context = RpcContext::from_state(state);
    let reply = handle_rpc(
        &context,
        &request.body,
        expected_token.as_deref().map(String::as_str),
        auth,
        deadline,
    );
    // Synchronous DB dispatch is not preemptible. If it returns after the transport's one absolute
    // deadline, discard the result before serialization, visibility admission, or any response
    // write can disclose it.
    match reply_before_deadline(reply, deadline) {
        Some(reply) => send_rpc_reply(&mut stream, reply, state, &gate, deadline),
        None => {
            let _ = write_http_response(&mut stream, 202, "application/json", b"", None, deadline);
        }
    }
}

fn reply_before_deadline(reply: Option<RpcReply>, deadline: Instant) -> Option<RpcReply> {
    (Instant::now() < deadline).then_some(reply).flatten()
}

/// Revalidate and register a content stream as one lifecycle-linearized operation, then release
/// every mutex before the first socket write. A concurrent revocation either closes admission
/// before registration or cancels and shuts down the registered clone.
fn send_rpc_reply(
    stream: &mut TcpStream,
    reply: RpcReply,
    state: &AppState,
    gate: &Arc<McpResponseGate>,
    deadline: Instant,
) {
    if Instant::now() >= deadline {
        return;
    }
    match reply {
        RpcReply::Immediate(response) => {
            let body = response.to_string();
            if Instant::now() >= deadline {
                return;
            }
            let _ = write_http_response(
                stream,
                200,
                "application/json",
                body.as_bytes(),
                None,
                deadline,
            );
        }
        RpcReply::Content(pending) => {
            let registration_stream = match stream.try_clone() {
                Ok(stream) => stream,
                Err(_) => return,
            };
            let Ok(lifecycle) = lock_before_deadline(&state.lifecycle, deadline, true) else {
                return;
            };
            if visibility_is_current(
                &pending.snapshot,
                &state.seal_epoch,
                state.unlocked_folders.as_ref(),
                deadline,
            ) != Ok(true)
                || pending.snapshot.ask_dispatch_generation
                    != state.db.ask_dispatch_generation().ok()
            {
                drop(lifecycle);
                let refusal = rpc_err(pending.id, VISIBILITY_RETRY_CODE, VISIBILITY_RETRY_MESSAGE)
                    .to_string();
                let _ = write_http_response(
                    stream,
                    200,
                    "application/json",
                    refusal.as_bytes(),
                    None,
                    deadline,
                );
                return;
            }
            let Some(lease) = gate.register(registration_stream) else {
                drop(lifecycle);
                let refusal = rpc_err(pending.id, VISIBILITY_RETRY_CODE, VISIBILITY_RETRY_MESSAGE)
                    .to_string();
                let _ = write_http_response(
                    stream,
                    200,
                    "application/json",
                    refusal.as_bytes(),
                    None,
                    deadline,
                );
                return;
            };
            drop(lifecycle);
            // Serialize only after deadline-bounded lifecycle admission. If admission expires, no
            // content body is rendered, no response lease exists, and no write syscall begins.
            let body = match content_reply_body(&pending) {
                Ok(body) => body,
                Err(_) => return,
            };
            if Instant::now() >= deadline {
                return;
            }
            if body.len() > MAX_RESPONSE_BYTES {
                // `handle_tool_call` bounds tool text and ids before this serialization, so this is
                // defense in depth for a future envelope change. The consumed id avoids cloning an
                // attacker-controlled JSON value merely to report a transport refusal.
                let refusal = rpc_err(
                    Value::Null,
                    -32000,
                    "MCP response exceeds the local transport limit",
                )
                .to_string();
                let _ = write_http_response(
                    stream,
                    200,
                    "application/json",
                    refusal.as_bytes(),
                    Some(&lease),
                    deadline,
                );
                return;
            }
            let _ = write_http_response(
                stream,
                200,
                "application/json",
                body.as_bytes(),
                Some(&lease),
                deadline,
            );
        }
    }
}

fn content_reply_body(
    pending: &PendingContentReply,
) -> std::result::Result<String, serde_json::Error> {
    match &pending.outcome {
        Ok(text) => serde_json::to_string(&BorrowedTextResponse {
            jsonrpc: "2.0",
            id: &pending.id,
            result: BorrowedTextResult {
                content: [BorrowedTextContent { kind: "text", text }],
            },
        }),
        Err((code, message)) => serde_json::to_string(&BorrowedRpcErrorResponse {
            jsonrpc: "2.0",
            id: &pending.id,
            error: BorrowedRpcErrorBody {
                code: *code,
                message,
            },
        }),
    }
}

#[cfg(test)]
fn content_reply_value(pending: PendingContentReply) -> Value {
    match pending.outcome {
        Ok(text) => text_result(pending.id, text),
        Err((code, message)) => rpc_err(pending.id, code, &message),
    }
}

fn visibility_is_current(
    snapshot: &VisibilitySnapshot,
    seal_epoch: &AtomicU64,
    unlocked_folders: &Mutex<HashSet<String>>,
    deadline: Instant,
) -> std::result::Result<bool, ()> {
    if seal_epoch.load(Ordering::SeqCst) != snapshot.seal_epoch {
        return Ok(false);
    }
    let live = lock_before_deadline(unlocked_folders, deadline, false)?;
    Ok(*live == snapshot.unlocked_folders)
}

#[cfg(test)]
fn finalize_content_reply(
    pending: PendingContentReply,
    seal_epoch: &AtomicU64,
    unlocked_folders: &Mutex<HashSet<String>>,
) -> Value {
    if visibility_is_current(
        &pending.snapshot,
        seal_epoch,
        unlocked_folders,
        Instant::now() + REQUEST_DEADLINE,
    ) != Ok(true)
    {
        return rpc_err(pending.id, VISIBILITY_RETRY_CODE, VISIBILITY_RETRY_MESSAGE);
    }
    content_reply_value(pending)
}

fn parse_http_request(
    stream: &mut TcpStream,
    deadline: Instant,
) -> std::result::Result<ParsedHttpRequest, HttpParseError> {
    parse_http_request_until(stream, deadline)
}

fn parse_http_request_until(
    stream: &mut TcpStream,
    deadline: Instant,
) -> std::result::Result<ParsedHttpRequest, HttpParseError> {
    let mut reader = BufReader::new(stream);
    let request_line = read_http_line(&mut reader, MAX_REQUEST_LINE_BYTES, deadline)?;
    let request_line = std::str::from_utf8(&request_line).map_err(|_| HttpParseError {
        status: 400,
        message: "request line is not ASCII",
    })?;
    if !request_line.is_ascii() {
        return Err(HttpParseError {
            status: 400,
            message: "request line is not ASCII",
        });
    }
    let mut parts = request_line.split(' ');
    let method = parts.next().unwrap_or_default();
    let target = parts.next().unwrap_or_default();
    let version = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || method.is_empty()
        || target.is_empty()
        || version != "HTTP/1.1"
        || !method.bytes().all(is_http_token_byte)
        || !target.starts_with('/')
        || !target.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(HttpParseError {
            status: 400,
            message: "malformed HTTP request line",
        });
    }

    let mut headers = Vec::new();
    let mut header_bytes = request_line.len() + 2;
    loop {
        let line = read_http_line(&mut reader, MAX_HEADER_BYTES, deadline)?;
        header_bytes = header_bytes.saturating_add(line.len() + 2);
        if header_bytes > MAX_HEADER_BYTES {
            return Err(HttpParseError {
                status: 431,
                message: "request headers too large",
            });
        }
        if line.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADER_COUNT {
            return Err(HttpParseError {
                status: 431,
                message: "too many request headers",
            });
        }
        let line = std::str::from_utf8(&line).map_err(|_| HttpParseError {
            status: 400,
            message: "request header is not valid ASCII",
        })?;
        if !line.is_ascii()
            || line
                .as_bytes()
                .first()
                .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
        {
            return Err(HttpParseError {
                status: 400,
                message: "malformed request header",
            });
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(HttpParseError {
                status: 400,
                message: "malformed request header",
            });
        };
        if name.is_empty() || !name.bytes().all(is_http_token_byte) {
            return Err(HttpParseError {
                status: 400,
                message: "malformed request header",
            });
        }
        let value = value.trim();
        if value
            .bytes()
            .any(|byte| (byte < 0x20 && byte != b'\t') || byte == 0x7f)
        {
            return Err(HttpParseError {
                status: 400,
                message: "malformed request header",
            });
        }
        headers.push((name.to_string(), value.to_string()));
    }

    if single_header(&headers, "Transfer-Encoding")?.is_some() {
        return Err(HttpParseError {
            status: 400,
            message: "transfer encoding is unsupported",
        });
    }
    let content_length = match single_header(&headers, "Content-Length")? {
        Some(raw) => {
            if raw.is_empty() || !raw.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(HttpParseError {
                    status: 400,
                    message: "invalid content length",
                });
            }
            raw.parse::<u64>().map_err(|_| HttpParseError {
                status: 400,
                message: "invalid content length",
            })?
        }
        None if method == "POST" => {
            return Err(HttpParseError {
                status: 411,
                message: "content length required",
            })
        }
        None => 0,
    };
    if content_length > MAX_BODY_BYTES {
        return Err(HttpParseError {
            status: 413,
            message: "request body too large",
        });
    }
    let body_len = usize::try_from(content_length).map_err(|_| HttpParseError {
        status: 413,
        message: "request body too large",
    })?;
    let mut body = vec![0_u8; body_len];
    let mut offset = 0;
    while offset < body.len() {
        arm_absolute_read_deadline(&mut reader, deadline)?;
        let read = reader
            .read(&mut body[offset..])
            .map_err(|error| request_read_error(error, deadline))?;
        if read == 0 {
            return Err(HttpParseError {
                status: 400,
                message: "incomplete request body",
            });
        }
        offset += read;
    }
    let body = String::from_utf8(body).map_err(|_| HttpParseError {
        status: 400,
        message: "request body is not UTF-8",
    })?;
    Ok(ParsedHttpRequest {
        method: method.to_string(),
        headers,
        body,
    })
}

fn read_http_line(
    reader: &mut BufReader<&mut TcpStream>,
    limit: usize,
    deadline: Instant,
) -> std::result::Result<Vec<u8>, HttpParseError> {
    let mut line = Vec::new();
    loop {
        arm_absolute_read_deadline(reader, deadline)?;
        let available = reader
            .fill_buf()
            .map_err(|error| request_read_error(error, deadline))?;
        if available.is_empty() {
            return Err(HttpParseError {
                status: 400,
                message: "malformed HTTP line ending",
            });
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(available.len());
        if line.len().saturating_add(take) > limit {
            return Err(HttpParseError {
                status: 431,
                message: "request headers too large",
            });
        }
        let terminated = available.get(take.saturating_sub(1)) == Some(&b'\n');
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if terminated {
            break;
        }
    }
    if !line.ends_with(b"\r\n") {
        return Err(HttpParseError {
            status: 400,
            message: "malformed HTTP line ending",
        });
    }
    line.truncate(line.len() - 2);
    Ok(line)
}

fn arm_absolute_read_deadline(
    reader: &mut BufReader<&mut TcpStream>,
    deadline: Instant,
) -> std::result::Result<(), HttpParseError> {
    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
        return Err(HttpParseError {
            status: 408,
            message: "request deadline exceeded",
        });
    };
    if remaining.is_zero() {
        return Err(HttpParseError {
            status: 408,
            message: "request deadline exceeded",
        });
    }
    reader
        .get_mut()
        .set_read_timeout(Some(READ_TIMEOUT.min(remaining)))
        .map_err(|_| HttpParseError {
            status: 400,
            message: "failed to configure request deadline",
        })
}

fn request_read_error(error: std::io::Error, deadline: Instant) -> HttpParseError {
    if matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) && Instant::now() >= deadline
    {
        HttpParseError {
            status: 408,
            message: "request deadline exceeded",
        }
    } else {
        HttpParseError {
            status: 400,
            message: "failed to read request",
        }
    }
}

fn is_http_token_byte(byte: u8) -> bool {
    matches!(
        byte,
        b'!' | b'#'
            | b'$'
            | b'%'
            | b'&'
            | b'\''
            | b'*'
            | b'+'
            | b'-'
            | b'.'
            | b'^'
            | b'_'
            | b'`'
            | b'|'
            | b'~'
            | b'0'..=b'9'
            | b'A'..=b'Z'
            | b'a'..=b'z'
    )
}

fn write_http_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    lease: Option<&ResponseLease>,
    deadline: Instant,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        403 => "Forbidden",
        408 => "Request Timeout",
        411 => "Length Required",
        413 => "Payload Too Large",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n\r\n",
        body.len()
    );
    write_cancellable_until(stream, headers.as_bytes(), lease, deadline)?;
    write_cancellable_until(stream, body, lease, deadline)?;
    flush_cancellable_until(stream, lease, deadline)?;
    let _ = stream.shutdown(Shutdown::Write);
    Ok(())
}

fn flush_cancellable_until(
    stream: &mut TcpStream,
    lease: Option<&ResponseLease>,
    deadline: Instant,
) -> std::io::Result<()> {
    flush_cancellable_until_with_io(stream, lease, deadline, || {}, TcpStream::flush)
}

fn flush_cancellable_until_with_io<B, F>(
    stream: &mut TcpStream,
    lease: Option<&ResponseLease>,
    deadline: Instant,
    before_flush: B,
    flush_once: F,
) -> std::io::Result<()>
where
    B: FnOnce(),
    F: FnOnce(&mut TcpStream) -> std::io::Result<()>,
{
    let syscall_guard = match lease {
        Some(lease) => Some(lease.begin_chunk().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Interrupted,
                "MCP response visibility revoked",
            )
        })?),
        None => None,
    };
    arm_absolute_write_deadline(stream, deadline)?;
    before_flush();
    let result = flush_once(stream);
    drop(syscall_guard);
    result?;
    ensure_before_deadline(deadline)
}

#[cfg(test)]
fn write_cancellable(
    stream: &mut TcpStream,
    bytes: &[u8],
    lease: Option<&ResponseLease>,
) -> std::io::Result<()> {
    write_cancellable_until(stream, bytes, lease, Instant::now() + REQUEST_DEADLINE)
}

fn write_cancellable_until(
    stream: &mut TcpStream,
    bytes: &[u8],
    lease: Option<&ResponseLease>,
    deadline: Instant,
) -> std::io::Result<()> {
    write_cancellable_until_with_io(
        stream,
        bytes,
        lease,
        deadline,
        |_| {},
        |_, _| {},
        TcpStream::write,
    )
}

#[cfg(test)]
fn write_cancellable_with_hook<F>(
    stream: &mut TcpStream,
    bytes: &[u8],
    lease: Option<&ResponseLease>,
    after_chunk_admitted: F,
) -> std::io::Result<()>
where
    F: FnMut(usize),
{
    write_cancellable_until_with_io(
        stream,
        bytes,
        lease,
        Instant::now() + REQUEST_DEADLINE,
        after_chunk_admitted,
        |_, _| {},
        TcpStream::write,
    )
}

#[cfg(test)]
fn write_cancellable_until_with_hook<F>(
    stream: &mut TcpStream,
    bytes: &[u8],
    lease: Option<&ResponseLease>,
    deadline: Instant,
    after_chunk_admitted: F,
) -> std::io::Result<()>
where
    F: FnMut(usize),
{
    write_cancellable_until_with_io(
        stream,
        bytes,
        lease,
        deadline,
        after_chunk_admitted,
        |_, _| {},
        TcpStream::write,
    )
}

fn write_cancellable_until_with_io<B, A, W>(
    stream: &mut TcpStream,
    bytes: &[u8],
    lease: Option<&ResponseLease>,
    deadline: Instant,
    mut before_write: B,
    mut after_write: A,
    mut write_once: W,
) -> std::io::Result<()>
where
    B: FnMut(usize),
    A: FnMut(usize, usize),
    W: FnMut(&mut TcpStream, &[u8]) -> std::io::Result<usize>,
{
    let mut syscall_index = 0;
    for chunk in bytes.chunks(RESPONSE_CHUNK_BYTES) {
        let mut offset = 0;
        while offset < chunk.len() {
            arm_absolute_write_deadline(stream, deadline)?;
            let syscall_guard = match lease {
                Some(lease) => Some(lease.begin_chunk().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "MCP response visibility revoked",
                    )
                })?),
                None => None,
            };
            // Test-only callers coordinate exact races immediately before and after one actual
            // write syscall. Production passes no-op hooks.
            before_write(syscall_index);
            let result = write_once(stream, &chunk[offset..]);
            drop(syscall_guard);
            match result {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "failed to write the complete MCP response",
                    ))
                }
                Ok(written) if written <= chunk.len() - offset => {
                    offset += written;
                    after_write(syscall_index, written);
                    ensure_before_deadline(deadline)?;
                }
                Ok(_) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "MCP response writer reported an invalid byte count",
                    ))
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
            syscall_index = syscall_index.saturating_add(1);
        }
    }
    Ok(())
}

fn arm_absolute_write_deadline(stream: &TcpStream, deadline: Instant) -> std::io::Result<()> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(request_deadline_error)?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT.min(remaining)))
}

fn ensure_before_deadline(deadline: Instant) -> std::io::Result<()> {
    if Instant::now() < deadline {
        Ok(())
    } else {
        Err(request_deadline_error())
    }
}

fn request_deadline_error() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "MCP absolute request deadline exceeded",
    )
}

/// Close all active content responses before a caller waits for the lock lifecycle. This helper is
/// intentionally content-free and no-op only before setup has managed the gate (when no MCP
/// listener can exist yet).
pub(crate) fn begin_visibility_revocation(
    app: &AppHandle,
    entrypoint: VisibilityRevokingEntrypoint,
) -> VisibilityRevocation {
    let gate = app
        .try_state::<Arc<McpResponseGate>>()
        .map(|gate| Arc::clone(gate.inner()));
    begin_visibility_revocation_for_gate(gate, entrypoint)
}

pub(crate) fn begin_visibility_revocation_for_gate(
    gate: Option<Arc<McpResponseGate>>,
    _entrypoint: VisibilityRevokingEntrypoint,
) -> VisibilityRevocation {
    if let Some(gate) = &gate {
        gate.close_and_shutdown();
    }
    VisibilityRevocation { gate }
}

/// Reopen content response admission only after the caller has advanced the seal epoch and revoked
/// the relevant session unlock membership.
pub(crate) fn finish_visibility_revocation(revocation: VisibilityRevocation) {
    revocation.complete();
}

/// Returns Some(response) for JSON-RPC requests, None for notifications.
/// `expected_token` is `Some` only when enforcement is on; `auth` is the raw Authorization header.
fn handle_rpc(
    context: &RpcContext<'_>,
    body: &str,
    expected_token: Option<&str>,
    auth: Option<&str>,
    deadline: Instant,
) -> Option<RpcReply> {
    let req: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => {
            return Some(RpcReply::Immediate(rpc_err(
                Value::Null,
                -32700,
                "parse error",
            )))
        }
    };
    // Notifications have no "id" → no response.
    let id = req.get("id")?;
    if !jsonrpc_id_is_bounded(id) {
        return Some(RpcReply::Immediate(rpc_err(
            Value::Null,
            -32600,
            "invalid request id",
        )));
    }
    let id = id.clone();
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");

    // E3: when enforcement is on, require a valid bearer token before ANY method — including
    // initialize / tools/list / ping. Discovery is no longer open: an unauthenticated local
    // process cannot even enumerate the tools. The check runs first, before any dispatch.
    if let Some(expected) = expected_token {
        if !bearer_ok(auth, expected) {
            return Some(RpcReply::Immediate(rpc_err(
                id,
                -32001,
                "unauthorized: bearer token required",
            )));
        }
    }

    let result = match method {
        "initialize" => json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "murmur", "version": env!("CARGO_PKG_VERSION") }
        }),
        "tools/list" => json!({ "tools": tools_spec() }),
        "ping" => json!({}),
        "tools/call" => {
            return Some(handle_tool_call(context, id, req.get("params"), deadline));
        }
        _ => return Some(RpcReply::Immediate(rpc_err(id, -32601, "method not found"))),
    };
    Some(RpcReply::Immediate(
        json!({ "jsonrpc": "2.0", "id": id, "result": result }),
    ))
}

/// Bearer-token check in CONSTANT TIME (E5): the Authorization header must be `Bearer <expected>`
/// and the token must equal `expected` byte-for-byte. The comparison uses `subtle::ConstantTimeEq`
/// over fixed-length byte slices so a timing side-channel cannot be used to recover the token a
/// prefix at a time. A length mismatch short-circuits to `false` WITHOUT a data-dependent compare
/// (lengths are not secret; the bytes are), and a non-matching length never feeds `ct_eq` mismatched
/// slices.
fn bearer_ok(auth: Option<&str>, expected: &str) -> bool {
    let Some(h) = auth else { return false };
    let Some(token) = h
        .strip_prefix("Bearer ")
        .or_else(|| h.strip_prefix("bearer "))
    else {
        return false;
    };
    let token = token.trim().as_bytes();
    let expected = expected.as_bytes();
    if token.len() != expected.len() {
        return false;
    }
    token.ct_eq(expected).into()
}

fn rpc_err(id: Value, code: i64, msg: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": msg } })
}

struct SerializedByteBudget {
    remaining: usize,
}

impl Write for SerializedByteBudget {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "serialized JSON value exceeds its byte budget",
            ));
        }
        self.remaining -= bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn jsonrpc_id_is_bounded(id: &Value) -> bool {
    match id {
        Value::Null | Value::Number(_) | Value::String(_) => {
            let mut budget = SerializedByteBudget {
                remaining: MAX_JSONRPC_ID_BYTES,
            };
            serde_json::to_writer(&mut budget, id).is_ok()
        }
        Value::Bool(_) | Value::Array(_) | Value::Object(_) => false,
    }
}

#[cfg(test)]
fn text_result(id: Value, text: String) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": { "content": [{ "type": "text", "text": text }] } })
}

/// The tool registry served by `tools/list`.
///
/// `pub(crate)` so the shipped-skill-catalog guard in `commands::lifecycle_tests` can read the
/// registry itself rather than keeping a second list that would drift from this one.
pub(crate) fn tools_spec() -> Value {
    json!([
        {
            "name": "search_meetings",
            "description": "Full-text search across your meeting titles, transcripts, notes, and imported documents/brain notes. Returns matching meetings and documents with snippets and ids. If nothing relevant turns up and you have joined an org, also try org_search — a colleague may have already shared the answer.",
            "inputSchema": { "type": "object", "properties": { "query": { "type": "string" } }, "required": ["query"] }
        },
        {
            "name": "search_transcript",
            "description": "Located lexical search inside visible transcript segments. Every hit includes a timestamp, stable stored segment id, speaker and character offsets in the exact structured transcript channel accepted by get_meeting. Counts are computed after channel projection within at most 20 matching meetings.",
            "inputSchema": { "type": "object", "properties": { "query": { "type": "string" }, "meetingId": { "type": "string", "description": "Optional meeting id scope." }, "limit": { "type": "number", "description": "Maximum hits overall (default 20, max 100)." }, "maxPerMeeting": { "type": "number", "description": "Maximum hits per meeting (default 5, max 20)." }, "channel": { "type": "string", "enum": ["merged", "mic", "system"], "description": "Structured transcript channel whose offsets are returned (default merged)." } }, "required": ["query"] }
        },
        {
            "name": "get_meeting",
            "description": "Get a meeting's AI note (summary) and transcript by id. The transcript is STRUCTURED by default — one line per segment, '[<start_s>–<end_s>] <Speaker>: <text>'. The response stamps format and channel because each pair has its own character-offset space. Merged is the canonical stored transcript (ingest removes echoes only when acoustic leak is measured); mic and system expose its stored capture lanes. The transcript is returned as a bounded, disclosed window and can be paged with offset + maxChars.",
            "inputSchema": { "type": "object", "properties": { "meetingId": { "type": "string" }, "transcriptFormat": { "type": "string", "enum": ["structured", "plain", "compact"], "description": "Transcript rendering (default structured). Each format is a different character space." }, "channel": { "type": "string", "enum": ["merged", "mic", "system"], "description": "Capture-lane projection (default merged). Keep this equal to the channel from search_transcript or get_meeting_chapters when using their offsets." }, "offset": { "type": "number", "description": "Chars to skip into the transcript, in the selected format and channel coordinate." }, "maxChars": { "type": "number", "description": "Max chars to return from offset (default: a bounded 6000-char window with the total disclosed). Bounds the NOTE section too." }, "includeNote": { "type": "boolean", "description": "Include the AI note (default true). Pass false for transcript only." } }, "required": ["meetingId"] }
        },
        {
            "name": "get_meeting_chapters",
            "description": "Get the visible timeline topic map for one meeting. Each topic carries a character range in the same structured transcript channel accepted by get_meeting, so a long meeting can be navigated without blind paging.",
            "inputSchema": { "type": "object", "properties": { "meetingId": { "type": "string" }, "channel": { "type": "string", "enum": ["merged", "mic", "system"], "description": "Structured transcript channel whose offsets are returned (default merged)." } }, "required": ["meetingId"] }
        },
        {
            "name": "get_document",
            "description": "Get the body of one standalone note or imported/uploaded document by id (from a search hit labelled 'document:...'). Use this — not get_meeting — for ids from the DOCUMENTS section of a search result. The body is returned as a WINDOW (default first 6000 chars) prefixed with 'TOTAL_CHARS: <N> (showing <start>..<end>)'; page a big document by passing offset + maxChars.",
            "inputSchema": { "type": "object", "properties": { "documentId": { "type": "string" }, "offset": { "type": "number", "description": "Chars to skip into the body (default 0)." }, "maxChars": { "type": "number", "description": "Max body chars to return from offset (default: a bounded 6000-char window with the total disclosed)." } }, "required": ["documentId"] }
        },
        {
            "name": "get_document_outline",
            "description": "Get the STRUCTURAL OUTLINE (heading/section map + page numbers) of one standalone note or imported/uploaded document by id (from a 'document:...' search hit). Use this on a BIG document BEFORE get_document: read the map, then fetch the section you need with get_document's offset + maxChars instead of paging blindly. Returns the section headings in document order; a flat/heading-less document has no outline. Sealed-and-locked documents return no outline.",
            "inputSchema": { "type": "object", "properties": { "documentId": { "type": "string" } }, "required": ["documentId"] }
        },
        {
            "name": "list_recent_meetings",
            "description": "List the most recent visible meetings for triage: title/date/status/id plus durationSeconds, transcriptChars, hasVisibleNote, and deterministic statusDetail for Error rows ('no transcript' or 'partial transcript'). Sealed-and-locked meetings are excluded.",
            "inputSchema": { "type": "object", "properties": { "limit": { "type": "number" } } }
        },
        {
            "name": "search_semantic",
            "description": "Semantic (meaning-based) search across your meeting notes and imported documents/brain notes, fused with full-text search. Finds relevant content even without the exact words. When semantic search is disabled in Murmur settings it falls back to keyword-only matching (the result says so). If nothing relevant turns up and you have joined an org, also try org_search — a colleague may have already shared the answer.",
            "inputSchema": { "type": "object", "properties": { "query": { "type": "string" } }, "required": ["query"] }
        },
        {
            "name": "get_open_commitments",
            "description": "Roll up every OPEN action item ('- [ ]', still open / not done) across your meetings, with each item's owner, due date and source meeting. Answers 'what did I promise / what is still open'. Optionally filter by owner (case-insensitive). Sealed-and-locked meetings are excluded.",
            "inputSchema": { "type": "object", "properties": { "owner": { "type": "string" } } }
        },
        {
            "name": "get_entity_dossier",
            "description": "Assemble a DOSSIER on one person or project across all your meetings: a timeline of mentions, the entity's open commitments, and co-occurring people/projects — each citing its source meeting [[Title]]. Pass an entity name (e.g. 'Anna' or 'Project Atlas') or id. Returns the gated source material for YOU to synthesize the 'state of [[entity]]' (Overview, Timeline, Open commitments, Last said / next step). Sealed-and-locked meetings are excluded.",
            "inputSchema": { "type": "object", "properties": { "entity": { "type": "string" }, "noteDetail": { "type": "string", "enum": ["none", "summary", "full"], "description": "How much of the meeting-note corpus to include. Default 'summary' (structured data + a bounded excerpt); 'none' for structured data only; 'full' for the bodies, pageable with offset/maxChars. Every mode discloses NOTES_TOTAL_CHARS so you can budget BEFORE asking for 'full'." }, "offset": { "type": "number", "description": "Chars to skip into the note corpus (noteDetail 'full')." }, "maxChars": { "type": "number", "description": "Max corpus chars to return." } }, "required": ["entity"] }
        },
        {
            "name": "knowledge_diff",
            "description": "The DECISION LEDGER for one person or project: what you knew about it changed over time (bitemporal facts). Pass an entity name (e.g. 'Anna' or 'Project Atlas') or id, plus two ISO-8601 instants 'from' and 'to' (e.g. '2026-06-01T00:00:00Z'). Returns what CHANGED between those two moments (added / removed / changed facts) PLUS the chronological supersession ledger. A separate bounded section may quote Decisions and Risks/Open Questions from currently visible mentioning-meeting notes; those items are explicitly historical source material, not bitemporal facts, current truth, or an open-risk ledger. Sealed-and-locked meetings are excluded.",
            "inputSchema": { "type": "object", "properties": { "entity": { "type": "string" }, "from": { "type": "string", "description": "ISO-8601 instant to snapshot the earlier state at." }, "to": { "type": "string", "description": "ISO-8601 instant to snapshot the later state at." } }, "required": ["entity", "from", "to"] }
        },
        {
            "name": "list_entities",
            "description": "List visible people and projects with their exact id, type, and visible meeting-mention count. Use this local discovery tool before get_entity_dossier or knowledge_diff instead of guessing a name. Optionally filter by a bounded case-insensitive substring. Results are newest-lock-snapshot safe, default to 40, and never exceed 100. Entities known only from sealed-and-locked meetings are excluded.",
            "inputSchema": { "type": "object", "properties": { "query": { "type": "string", "description": "Optional case-insensitive substring filter (first 128 characters are used)." }, "limit": { "type": "number", "description": "Maximum results (default 40, maximum 100)." } } }
        },
        {
            "name": "list_note_folders",
            "description": "List visible note folders with the exact id/name accepted by query_database, visible record count, and typed columns. Call this before guessing a folder. A sealed-and-not-session-unlocked folder is absent together with its name, id, count, and schema.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "list_workspace_hierarchy",
            "description": "The WORKSPACE HIERARCHY: every visible project, the folders inside it, and how many meetings / notes / tasks / dashboards each holds. This is where things LIVE, which is a different fact from what they say — a meeting in 'Acme / Weekly' and one in 'Personal' mean different things. Call it to ground a question about a project or a client in the container the user actually keeps it in, and to get the exact container id other tools accept. A sealed-and-not-session-unlocked container appears by name but reports no counts and no contents.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "list_dashboards",
            "description": "List the user's DASHBOARDS — boards they composed BY HAND out of meetings, notes, documents, people and derived views (drift lanes, promise ledgers, pulses). Returns title + id + tile kinds only, no content. A board is the user's own declaration of what belongs together, so it is better scope than a search guess: check here first for questions about a project, deal or topic they track.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "get_dashboard",
            "description": "Read one dashboard by id: every tile, already resolved — the notes and recordings on it, who is on it, which values drifted over time, what was promised and whether it landed, and what has gone quiet. A tile whose source is sealed-and-not-session-unlocked comes back redacted, exactly as it renders on screen. Use it to answer from precisely the context the user curated.",
            "inputSchema": { "type": "object", "properties": { "dashboardId": { "type": "string" } }, "required": ["dashboardId"] }
        },
        {
            "name": "list_tasks",
            "description": "List the user's SHARED ORG TASKS — the work items they and their colleagues track together inside an organization (title + id + status + due date + org). Tasks are NOT meeting notes: they are the explicit commitments the team is managing right now, so this is the right tool for 'what am I on the hook for', 'what is in progress', or 'what is overdue'. Use it to get the exact task id get_task takes. Only orgs the user has joined with context sharing enabled are listed; everything else is absent, id included.",
            "inputSchema": { "type": "object", "properties": {} }
        },
        {
            "name": "get_task",
            "description": "Read ONE shared org task by id — title, description, status, due date, assignee, subtask checklist, and the shared org notes it points at. The id is the one the app's header copy control puts on the clipboard, or one from list_tasks. A task in an organization whose context sharing is off reads exactly like a task that does not exist. This device's own private links from a task to a local board or note are deliberately not included.",
            "inputSchema": { "type": "object", "properties": { "taskId": { "type": "string" } }, "required": ["taskId"] }
        },
        {
            "name": "org_search",
            "description": "Fallback for when search_meetings / search_semantic find nothing relevant in your OWN vault and you have joined an org: search the ORGANIZATION brain — notes your colleagues explicitly shared to the shared org brain (synced + decrypted locally; no data leaves this device). Results are attributed '[org · <author>]' and MUST be cited as coming from that colleague. Only meaningful when you have joined an org and consented to org sharing (otherwise returns no results). Use for 'what does the team / someone else know or decide about X' questions.",
            "inputSchema": { "type": "object", "properties": { "query": { "type": "string" } }, "required": ["query"] }
        },
        {
            "name": "query_database",
            "description": "Query the TYPED PROPERTIES of a note-folder's notes as a small database (the folder's Table/Board columns: status, owner, due date, priority, etc.). Give the folder NAME (or id) and a filter: 'key op value' clauses joined by AND / OR, op ∈ = != > < >= <= or 'contains' (e.g. 'status=Done', 'openItems>3', 'owner contains ann', 'status=Open AND priority=High'). Empty filter = every row. Sealed-and-locked note folders are excluded. Use for 'which notes are still open', 'what does Anna own', 'high-priority items' questions over a note-folder's columns.",
            "inputSchema": { "type": "object", "properties": { "folder": { "type": "string" }, "filter": { "type": "string" } }, "required": ["folder"] }
        }
    ])
}

/// A tool-dispatch error mapped to a JSON-RPC `(code, message)`. Kept separate from the `Value`
/// builders so the dispatch logic is testable against an injected `Db` without the HTTP/JSON-RPC
/// envelope (and without `handle_tool_call`'s `Db::open` → Keychain).
type ToolError = (i64, String);

fn handle_tool_call(
    context: &RpcContext<'_>,
    id: Value,
    params: Option<&Value>,
    deadline: Instant,
) -> RpcReply {
    let name = params
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let args = params
        .and_then(|p| p.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    let snapshot = match context.visibility_snapshot(deadline) {
        Ok(snapshot) => snapshot,
        Err(()) => {
            return RpcReply::Immediate(rpc_err(
                id,
                VISIBILITY_RETRY_CODE,
                VISIBILITY_RETRY_MESSAGE,
            ))
        }
    };
    let Some(db) = context.db else {
        return RpcReply::Immediate(rpc_err(id, -32000, "database unavailable"));
    };
    let outcome = bound_tool_outcome(dispatch_tool(db, name, &args, &snapshot.unlocked_folders));
    RpcReply::Content(PendingContentReply {
        id,
        snapshot,
        outcome,
    })
}

fn bound_tool_outcome(
    outcome: std::result::Result<String, ToolError>,
) -> std::result::Result<String, ToolError> {
    match outcome {
        Ok(text) if text.len() > MAX_TOOL_TEXT_BYTES => Err((
            -32000,
            "MCP tool result exceeds the local transport limit; request a smaller page".into(),
        )),
        Err((code, message)) if message.len() > MAX_TOOL_TEXT_BYTES => Err((
            code,
            "MCP tool error exceeds the local transport limit".into(),
        )),
        other => other,
    }
}

/// Dispatch a `tools/call` against an OPEN `Db`. THIN MAPPER: parse the JSON-RPC tool name + args
/// into a transport-agnostic [`crate::tools::ToolCall`], then run it through the single gated
/// [`crate::tools::execute_tool`] seam (shared with the future local brain). Every read there is
/// visibility-gated against `unlocked_set` (`search_visible` / `meeting_is_visible` /
/// `get_note_if_visible` / `list_meetings_visible` / `search_hybrid_visible` / `build_dossier_data`),
/// so a sealed-and-not-unlocked meeting is invisible to all of them. JSON-RPC error codes for the
/// transport concerns (unknown tool, missing required arg) are produced HERE; runtime tool failures
/// map to `-32000` exactly as before. Returns the tool's text payload or a `(code, message)` error.
/// Brain v3 PR-2 — read an optional non-negative integer MCP arg (agent paging). Absent / non-numeric
/// → 0 (the DEFAULT, mapped to today's byte-identical behavior by the tool).
fn mcp_usize_arg(args: &Value, key: &str) -> usize {
    args.get(key)
        .and_then(Value::as_u64)
        .unwrap_or(0)
        .min(usize::MAX as u64) as usize
}

/// Brain v3 audit Fix 2 — the MCP `get_meeting`/`get_document` DEFAULT window. UNLIKE the in-app
/// agentic loop (which caps every tool result at `RESULT_BUDGET` before re-feeding it to the model),
/// a raw MCP `tools/call` returns the ENTIRE payload to the connected client — so a client that
/// omits paging on a multi-MB document/transcript would be flooded. When the MCP client passes NO
/// paging (both `offset` and `maxChars` absent/0) we substitute THIS bounded default `maxChars`, and
/// the tool returns a DISCLOSED window (`TOTAL_CHARS: <N> …`) so the client can see the full length
/// and page the rest with explicit `offset`. A client that DOES pass paging is honored verbatim.
/// DELIBERATE default change (documented): the pre-fix MCP default `(0,0)` returned the whole body.
const MCP_DEFAULT_WINDOW_CHARS: usize = 6000;
const MCP_ENTITY_FILTER_MAX_CHARS: usize = 128;

/// Resolve the MCP paging window for a body tool: honor an explicit `maxChars`, but whenever the
/// client gives no (or a zero) `maxChars`, bound it to [`MCP_DEFAULT_WINDOW_CHARS`] so a huge payload
/// is windowed + disclosed instead of flooding the client (a raw MCP tools/call has no RESULT_BUDGET).
/// `offset` is honored verbatim, so a client can still page a large body a window at a time. Returns
/// `(offset, max_chars)`.
fn mcp_body_window(args: &Value) -> (usize, usize) {
    let offset = mcp_usize_arg(args, "offset");
    let max_chars = mcp_usize_arg(args, "maxChars");
    // maxChars == 0 means "absent" (mcp_usize_arg's default) or an explicit unbounded request — both
    // are the flood case, so bound them to the default window. An explicit positive maxChars wins.
    let max_chars = if max_chars == 0 {
        MCP_DEFAULT_WINDOW_CHARS
    } else {
        max_chars.min(MAX_TOOL_WINDOW_CHARS)
    };
    (offset, max_chars)
}

fn mcp_transcript_channel(
    args: &Value,
) -> std::result::Result<crate::tools::TranscriptChannel, ToolError> {
    crate::tools::TranscriptChannel::parse(args.get("channel").and_then(Value::as_str))
        .map_err(|err| (-32602, err.to_string()))
}

fn dispatch_tool(
    db: &Db,
    name: &str,
    args: &Value,
    unlocked_set: &HashSet<String>,
) -> std::result::Result<String, ToolError> {
    use crate::tools::ToolCall;
    let call = match name {
        "search_meetings" => ToolCall::SearchMeetings {
            query: args
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        },
        "search_semantic" => ToolCall::SearchSemantic {
            query: args
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        },
        "list_entities" => ToolCall::ListEntities {
            query: args
                .get("query")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|query| !query.is_empty())
                .map(|query| query.chars().take(MCP_ENTITY_FILTER_MAX_CHARS).collect()),
            limit: args
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(40)
                .clamp(1, 100) as usize,
        },
        "list_note_folders" => ToolCall::ListNoteFolders,
        "list_workspace_hierarchy" => ToolCall::ListWorkspaceHierarchy,
        "list_dashboards" => ToolCall::ListDashboards,
        "get_dashboard" => ToolCall::GetDashboard {
            dashboard_id: args
                .get("dashboardId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        },
        "search_transcript" => {
            let query = args.get("query").and_then(Value::as_str).unwrap_or("");
            ToolCall::SearchTranscript {
                query: query.to_string(),
                meeting_id: args
                    .get("meetingId")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
                limit: args
                    .get("limit")
                    .and_then(Value::as_u64)
                    .unwrap_or(20)
                    .clamp(1, 100) as usize,
                max_per_meeting: args
                    .get("maxPerMeeting")
                    .and_then(Value::as_u64)
                    .unwrap_or(5)
                    .clamp(1, 20) as usize,
                channel: mcp_transcript_channel(args)?,
            }
        }
        "get_meeting" => ToolCall::GetMeeting {
            meeting_id: args
                .get("meetingId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            // Feature D: default to the STRUCTURED transcript. Only the exact literals "plain" and
            // "compact" select an alternative; an absent/other value routes to "structured".
            transcript_format: args
                .get("transcriptFormat")
                .and_then(Value::as_str)
                .filter(|f| *f == "plain" || *f == "compact")
                .unwrap_or("structured")
                .to_string(),
            channel: mcp_transcript_channel(args)?,
            include_speaker_map: true,
            // Brain v3 audit Fix 2 — bound + DISCLOSE the default MCP window (no paging args → a
            // 6000-char disclosed window, not the whole transcript) so a huge transcript can't flood
            // the client; explicit offset/maxChars are honored verbatim.
            offset: mcp_body_window(args).0,
            max_chars: mcp_body_window(args).1,
            // Absent ⇒ true, so an existing client's payload is unchanged.
            include_note: args
                .get("includeNote")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        },
        "get_meeting_chapters" => ToolCall::GetMeetingChapters {
            meeting_id: args
                .get("meetingId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            channel: mcp_transcript_channel(args)?,
        },
        "get_document" => ToolCall::GetDocument {
            document_id: args
                .get("documentId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            // Audit Fix 2 — same bounded + disclosed default window as get_meeting.
            offset: mcp_body_window(args).0,
            max_chars: mcp_body_window(args).1,
        },
        // Brain v3 audit Fix 3(b) — the document OUTLINE (heading map). Gated by
        // `get_document_outline_if_visible` inside `execute_tool` (a sealed-not-unlocked doc → empty).
        "get_document_outline" => ToolCall::GetDocumentOutline {
            document_id: args
                .get("documentId")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        },
        "list_recent_meetings" => ToolCall::ListRecentMeetings {
            limit: args
                .get("limit")
                .and_then(Value::as_i64)
                .unwrap_or(20)
                .clamp(1, 100),
        },
        "get_open_commitments" => ToolCall::GetOpenCommitments {
            owner: args
                .get("owner")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|o| !o.is_empty())
                .map(str::to_string),
        },
        "get_entity_dossier" => {
            let entity = args.get("entity").and_then(Value::as_str).unwrap_or("");
            if entity.trim().is_empty() {
                return Err((-32602, "missing required argument: entity".to_string()));
            }
            ToolCall::GetEntityDossier {
                entity: entity.to_string(),
                // #15 — DEFAULT to `summary`. The dossier was the one body tool with NO bound, so a
                // raw tools/call returned every mentioning meeting's note in full: measured at
                // ~37500 chars for a 3-meeting entity, and unpredictable before calling.
                note_detail: args
                    .get("noteDetail")
                    .and_then(Value::as_str)
                    .filter(|d| matches!(*d, "none" | "summary" | "full"))
                    .unwrap_or("summary")
                    .to_string(),
                offset: mcp_usize_arg(args, "offset"),
                max_chars: mcp_usize_arg(args, "maxChars").min(MAX_TOOL_WINDOW_CHARS),
            }
        }
        // LOCAL_LOOPBACK ONLY: KnowledgeDiff is absent from model-facing `tool_specs` and every
        // GatedToolExecutor scope. This mapper is reachable only through this module's fixed
        // 127.0.0.1 listener, exact Host/Origin/token gates, visibility snapshot, and response
        // revalidation/cancellation. The gated reader covers both fact rows and read-time note
        // context. `entity`, `from`, and `to` are all required.
        "knowledge_diff" => {
            let entity = args.get("entity").and_then(Value::as_str).unwrap_or("");
            if entity.trim().is_empty() {
                return Err((-32602, "missing required argument: entity".to_string()));
            }
            let from = args.get("from").and_then(Value::as_str).unwrap_or("");
            let to = args.get("to").and_then(Value::as_str).unwrap_or("");
            if from.trim().is_empty() {
                return Err((-32602, "missing required argument: from".to_string()));
            }
            if to.trim().is_empty() {
                return Err((-32602, "missing required argument: to".to_string()));
            }
            // B2 — this is UNTRUSTED MCP client input, so validate at the dispatch boundary that
            // BOTH `from` and `to` parse as RFC3339. `facts.rs::normalize_instant` returns an
            // unparseable string UNCHANGED and `cmp_instant` then compares it lexically, so a
            // garbage `from` sorts AFTER a real `to`, SWAPS the range, and yields a confident but
            // wrong "0 changes" with NO error. Reject the bad timestamp here (naming the offending
            // arg) rather than silently returning an empty window. `build_knowledge_diff`'s lenient
            // pass-through is left intact for the other in-app callers that rely on it.
            for (arg, value) in [("from", from), ("to", to)] {
                if chrono::DateTime::parse_from_rfc3339(value).is_err() {
                    return Err((
                        -32602,
                        format!("invalid ISO-8601 timestamp for '{arg}': {value}"),
                    ));
                }
            }
            ToolCall::KnowledgeDiff {
                entity: entity.to_string(),
                from: from.to_string(),
                to: to.to_string(),
            }
        }
        // Shared Brain — LOCAL, egress-free search of the org partition (synced colleagues' shares).
        // Untrusted multi-writer content: `execute_tool` provenance-labels + fence-neutralizes it. Not
        // folder-lock gated (org items live outside the lock domain), so `unlocked_set` is irrelevant
        // to it; when no org is joined/consented the partition is empty ⇒ "no results" (never a leak).
        // LOCAL_LOOPBACK ONLY, like `knowledge_diff` above: both task tools are absent from
        // model-facing `tool_specs` and every GatedToolExecutor scope, so a colleague's shared work
        // reaches a model only through this module's fixed 127.0.0.1 listener and its Host/Origin/
        // token gates. The read gate itself is in SQL (`org_state.context_enabled = 1`).
        "list_tasks" => ToolCall::ListTasks,
        "get_task" => {
            let task_id = args.get("taskId").and_then(Value::as_str).unwrap_or("");
            if task_id.trim().is_empty() {
                return Err((-32602, "missing required argument: taskId".to_string()));
            }
            ToolCall::GetTask {
                task_id: task_id.to_string(),
            }
        }
        "org_search" => ToolCall::OrgBrainSearch {
            query: args
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        },
        // Feature C — TYPED note-folder database query. `folder` is required (name or id); `filter`
        // is optional (empty = all rows). Gated by `list_notes_visible_typed` against `unlocked_set`,
        // so a sealed-not-unlocked note folder yields no rows here.
        "query_database" => {
            let folder = args.get("folder").and_then(Value::as_str).unwrap_or("");
            if folder.trim().is_empty() {
                return Err((-32602, "missing required argument: folder".to_string()));
            }
            ToolCall::QueryDatabase {
                folder: folder.to_string(),
                filter: args
                    .get("filter")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            }
        }
        other => return Err((-32602, format!("unknown tool: {other}"))),
    };
    // The `semantic_search_enabled` flag lives in the whole-DB-encrypted settings table; load it from
    // the SAME DB the MCP reader opened. On a load failure this degrades to `AppConfig::default()`,
    // whose Tier 1 default is now flag ON — harmless: with no e5 model the hybrid `search_semantic`
    // leg degenerates to the SAME gated FTS (no leak, no crash), and every leg stays visibility-gated.
    let config = crate::settings::AppConfig::load(db).unwrap_or_default();
    crate::tools::execute_tool(&call, db, unlocked_set, &config)
        .map_err(|e| (-32000, e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let client = TcpStream::connect(address).unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    fn parse_raw_request(raw: &[u8]) -> std::result::Result<ParsedHttpRequest, HttpParseError> {
        let (mut client, mut server) = tcp_pair();
        client.write_all(raw).unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        parse_http_request(&mut server, Instant::now() + REQUEST_DEADLINE)
    }

    fn rpc(body: &str) -> Option<Value> {
        rpc_auth(body, None, None)
    }

    fn finish_test_reply(context: &RpcContext<'_>, reply: RpcReply) -> Value {
        match reply {
            RpcReply::Immediate(response) => response,
            RpcReply::Content(pending) => {
                let _lifecycle = context
                    .lifecycle
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                finalize_content_reply(pending, context.seal_epoch, context.unlocked_folders)
            }
        }
    }

    #[test]
    fn initialize_returns_server_info() {
        let r = rpc(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#).unwrap();
        assert_eq!(r["result"]["serverInfo"]["name"], "murmur");
        assert_eq!(r["result"]["protocolVersion"], "2024-11-05");
    }

    #[test]
    fn tools_list_has_twenty_tools() {
        let r = rpc(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).unwrap();
        let tools = r["result"]["tools"].as_array().unwrap();
        // Twenty since `list_tasks` + `get_task` joined. The count is deliberately pinned: the
        // MCP catalogue is the loopback surface's whole contract, and a tool arriving or leaving
        // it unnoticed is precisely what this guard exists to make impossible. It did its job
        // here — the new tools were added and this test failed until the number was reconsidered.
        assert_eq!(tools.len(), 20);
        assert!(
            tools.iter().any(|t| t["name"] == "list_tasks")
                && tools.iter().any(|t| t["name"] == "get_task"),
            "a task id copied from the app header must be resolvable over local MCP"
        );
        assert!(
            tools.iter().any(|t| t["name"] == "list_workspace_hierarchy"),
            "the hierarchy must be discoverable over local MCP"
        );
        assert!(
            tools.iter().any(|t| t["name"] == "list_dashboards")
                && tools.iter().any(|t| t["name"] == "get_dashboard"),
            "the user's own curated boards must be reachable over local MCP"
        );
        assert!(
            tools.iter().any(|t| t["name"] == "list_entities"),
            "entity discovery must be advertised on local MCP"
        );
        assert!(
            tools.iter().any(|t| t["name"] == "list_note_folders"),
            "note-folder discovery must be advertised on local MCP"
        );
        // The Phase 2b semantic tool is advertised.
        assert!(tools.iter().any(|t| t["name"] == "search_semantic"));
        // The Phase 5a open-commitments rollup tool is advertised.
        assert!(tools.iter().any(|t| t["name"] == "get_open_commitments"));
        // The Phase 5b entity-dossier tool is advertised.
        assert!(tools.iter().any(|t| t["name"] == "get_entity_dossier"));
        // Shared Brain — the org partition search tool is advertised.
        assert!(tools.iter().any(|t| t["name"] == "org_search"));
        // Feature D — the full-note/document reader tool is advertised (the 8th tool).
        assert!(
            tools.iter().any(|t| t["name"] == "get_document"),
            "get_document must be advertised in the MCP tool catalog"
        );
        // Feature C — the typed note-folder database query tool is advertised (the 9th tool).
        assert!(
            tools.iter().any(|t| t["name"] == "query_database"),
            "query_database must be advertised in the MCP tool catalog"
        );
        // Brain v3 PR-6 — the knowledge-diff / decision-ledger tool is advertised (the 10th tool).
        assert!(
            tools.iter().any(|t| t["name"] == "knowledge_diff"),
            "knowledge_diff must be advertised in the MCP tool catalog"
        );
        // Brain v3 audit Fix 3(b) — the document-outline tool is advertised (the 11th tool), and its
        // args must be MIRRORED between the MCP tools list and the agentic tool surface (documentId).
        let outline = tools
            .iter()
            .find(|t| t["name"] == "get_document_outline")
            .expect("get_document_outline must be advertised in the MCP tool catalog");
        assert_eq!(
            outline["inputSchema"]["required"][0], "documentId",
            "the MCP outline tool must advertise the documentId arg (parity with the tool surface)"
        );
        assert!(
            tools.iter().any(|t| t["name"] == "search_transcript"),
            "located transcript search must be advertised"
        );
        assert!(
            tools.iter().any(|t| t["name"] == "get_meeting_chapters"),
            "chapter navigation must be advertised"
        );
    }

    /// A4 (RED-before-GREEN): the MCP catalog must steer callers toward `org_search` as a FALLBACK
    /// when `search_meetings`/`search_semantic` find nothing — and `org_search`'s own description
    /// must lead with that fallback framing, not present itself as an unrelated alternative.
    #[test]
    fn tool_catalog_nudges_org_search_as_a_fallback() {
        let r = rpc(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).unwrap();
        let tools = r["result"]["tools"].as_array().unwrap();
        let desc = |name: &str| -> String {
            tools
                .iter()
                .find(|t| t["name"] == name)
                .and_then(|t| t["description"].as_str())
                .unwrap_or_default()
                .to_string()
        };
        let search_meetings = desc("search_meetings");
        let search_semantic = desc("search_semantic");
        let org_search = desc("org_search");
        assert!(
            search_meetings.contains("org_search"),
            "search_meetings must mention org_search as a fallback: {search_meetings}"
        );
        assert!(
            search_semantic.contains("org_search"),
            "search_semantic must mention org_search as a fallback: {search_semantic}"
        );
        assert!(
            org_search.to_lowercase().starts_with("fallback"),
            "org_search's own description must LEAD with the fallback framing: {org_search}"
        );
    }

    #[test]
    fn notification_returns_none() {
        assert!(rpc(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none());
    }

    #[test]
    fn parse_error_and_unknown_method() {
        assert_eq!(rpc("not json").unwrap()["error"]["code"], -32700);
        assert_eq!(
            rpc(r#"{"jsonrpc":"2.0","id":3,"method":"bogus"}"#).unwrap()["error"]["code"],
            -32601
        );
    }

    fn rpc_auth(body: &str, expected: Option<&str>, auth: Option<&str>) -> Option<Value> {
        let lifecycle = Mutex::new(());
        let seal_epoch = AtomicU64::new(0);
        let unlocked_folders = Mutex::new(HashSet::new());
        let context = RpcContext {
            db: None,
            lifecycle: &lifecycle,
            seal_epoch: &seal_epoch,
            unlocked_folders: &unlocked_folders,
        };
        handle_rpc(
            &context,
            body,
            expected,
            auth,
            Instant::now() + REQUEST_DEADLINE,
        )
        .map(|reply| finish_test_reply(&context, reply))
    }

    #[test]
    fn token_disabled_keeps_discovery_open() {
        // With enforcement OFF (expected_token = None), discovery still works without a token —
        // this preserves the no-token local connection when the user hasn't enabled the gate.
        assert!(rpc(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).unwrap()["result"].is_object());
        assert!(
            rpc(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#).unwrap()["result"].is_object()
        );
    }

    #[test]
    fn token_required_gates_every_method() {
        // E3: with enforcement ON, EVERY method (initialize / tools/list / ping / tools/call)
        // requires a valid bearer token. NOTE: every assertion here STOPS before tool dispatch,
        // because the unauthorized branch returns early in `handle_rpc` — exactly the
        // security-critical path we want to prove.
        for method in ["initialize", "tools/list", "ping", "tools/call"] {
            let body = format!(
                r#"{{"jsonrpc":"2.0","id":7,"method":"{method}","params":{{"name":"list_recent_meetings","arguments":{{}}}}}}"#
            );
            // No Authorization header → unauthorized, BEFORE any dispatch/DB access.
            let unauth = rpc_auth(&body, Some("sekret"), None).unwrap();
            assert_eq!(
                unauth["error"]["code"], -32001,
                "method {method} must be gated"
            );
            // Wrong token → unauthorized, also before dispatch.
            let wrong = rpc_auth(&body, Some("sekret"), Some("Bearer nope")).unwrap();
            assert_eq!(
                wrong["error"]["code"], -32001,
                "method {method} wrong-token must be gated"
            );
        }
        // A CORRECT token lets discovery through (no DB access on initialize/tools/list/ping).
        let ok = rpc_auth(
            r#"{"jsonrpc":"2.0","id":8,"method":"tools/list"}"#,
            Some("sekret"),
            Some("Bearer sekret"),
        )
        .unwrap();
        assert!(ok["result"]["tools"].is_array());
        // The "correct token reaches the DB" path is intentionally NOT asserted here: it would
        // call `Db::open` → real Keychain. `bearer_ok` (below) proves the matcher in isolation.
    }

    #[test]
    fn bearer_ok_constant_time_matches_scheme_and_value() {
        assert!(bearer_ok(Some("Bearer abc"), "abc"));
        assert!(bearer_ok(Some("bearer abc"), "abc"));
        assert!(!bearer_ok(Some("Basic abc"), "abc"));
        assert!(!bearer_ok(Some("Bearer abc"), "abcd")); // length mismatch
        assert!(!bearer_ok(Some("Bearer abd"), "abc")); // same length, different bytes
        assert!(!bearer_ok(None, "abc"));
    }

    #[test]
    fn strict_http_parser_accepts_one_bounded_post() {
        let body = br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
        let request = format!(
            "POST /mcp HTTP/1.1\r\nHost: localhost:8765\r\nOrigin: http://localhost\r\nAuthorization: Bearer abc\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            std::str::from_utf8(body).unwrap()
        );
        let parsed = parse_raw_request(request.as_bytes()).unwrap();
        assert_eq!(parsed.method, "POST");
        assert_eq!(parsed.body.as_bytes(), body);
        assert_eq!(
            single_header(&parsed.headers, "host").unwrap(),
            Some("localhost:8765")
        );
        assert_eq!(
            single_header(&parsed.headers, "origin").unwrap(),
            Some("http://localhost")
        );
    }

    #[test]
    fn strict_http_parser_rejects_malformed_and_oversized_inputs() {
        let cases: Vec<(Vec<u8>, u16)> = vec![
            (
                b"POST / HTTP/1.0\r\nHost: localhost:8765\r\nContent-Length: 0\r\n\r\n"
                    .to_vec(),
                400,
            ),
            (
                b"POST / HTTP/1.1\nHost: localhost:8765\nContent-Length: 0\n\n".to_vec(),
                400,
            ),
            (
                b"POST / HTTP/1.1\r\nHost: localhost:8765\r\nTransfer-Encoding: chunked\r\nContent-Length: 0\r\n\r\n"
                    .to_vec(),
                400,
            ),
            (
                b"POST / HTTP/1.1\r\nHost: localhost:8765\r\nX-Bad: value\x7f\r\nContent-Length: 0\r\n\r\n"
                    .to_vec(),
                400,
            ),
            (
                b"POST / HTTP/1.1\r\nHost: localhost:8765\r\nContent-Length: +0\r\n\r\n"
                    .to_vec(),
                400,
            ),
            (
                b"POST / HTTP/1.1\r\nHost: localhost:8765\r\nContent-Length: \r\n\r\n"
                    .to_vec(),
                400,
            ),
            (
                format!(
                    "POST / HTTP/1.1\r\nHost: localhost:8765\r\nContent-Length: {}\r\n\r\n",
                    MAX_BODY_BYTES + 1
                )
                .into_bytes(),
                413,
            ),
            (
                b"POST / HTTP/1.1\r\nHost: localhost:8765\r\n\r\n".to_vec(),
                411,
            ),
        ];
        for (raw, expected_status) in cases {
            let error = parse_raw_request(&raw).unwrap_err();
            assert_eq!(error.status, expected_status, "raw={raw:?}");
        }

        let oversized_header = format!(
            "POST / HTTP/1.1\r\nHost: localhost:8765\r\nX-Fill: {}\r\nContent-Length: 0\r\n\r\n",
            "x".repeat(MAX_HEADER_BYTES)
        );
        assert_eq!(
            parse_raw_request(oversized_header.as_bytes())
                .unwrap_err()
                .status,
            431
        );
    }

    #[test]
    fn strict_http_security_headers_fail_closed_on_duplicates() {
        let headers = vec![
            ("Host".to_string(), "localhost:8765".to_string()),
            ("hOsT".to_string(), "evil.example:8765".to_string()),
        ];
        assert_eq!(single_header(&headers, "host").unwrap_err().status, 400);
        let origins = vec![
            ("Origin".to_string(), "http://localhost".to_string()),
            ("Origin".to_string(), "null".to_string()),
        ];
        assert_eq!(single_header(&origins, "origin").unwrap_err().status, 400);
    }

    #[test]
    fn failed_worker_spawn_releases_the_reserved_connection_slot() {
        let active = Arc::new(AtomicUsize::new(1));
        let result = spawn_connection_worker(
            Arc::clone(&active),
            || panic!("a rejected worker must never run"),
            |job| {
                drop(job);
                Err(std::io::Error::other("injected spawn failure"))
            },
        );
        assert!(result.is_err());
        assert_eq!(
            active.load(Ordering::SeqCst),
            0,
            "failed thread creation leaked a connection slot"
        );
    }

    #[test]
    fn absolute_request_deadline_evicts_all_slowloris_connection_slots() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let active = Arc::new(AtomicUsize::new(0));
        let started = Instant::now();
        let (result_tx, result_rx) = mpsc::channel();
        let mut workers = Vec::new();
        let mut drippers = Vec::new();
        for _ in 0..MAX_ACTIVE_CONNECTIONS {
            let mut client = TcpStream::connect(address).unwrap();
            let worker_tx = result_tx.clone();
            let mut worker = None;
            let admission = accept_bounded_connection(
                &listener,
                Arc::clone(&active),
                Duration::from_millis(250),
                move |mut server, deadline| {
                    worker_tx
                        .send(parse_http_request(&mut server, deadline))
                        .unwrap();
                },
                |job| {
                    worker = Some(std::thread::spawn(job));
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(admission, ConnectionAdmission::Spawned);
            workers.push(worker.expect("worker spawner must return its handle"));
            drippers.push(std::thread::spawn(move || {
                for byte in b"POST / HTTP/1.1\r\nHost: localhost:8765\r\n" {
                    if client.write_all(&[*byte]).is_err() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            }));
        }
        drop(result_tx);
        assert_eq!(active.load(Ordering::SeqCst), MAX_ACTIVE_CONNECTIONS);
        assert!(
            !try_acquire_connection(&active),
            "the bounded admission path accepted a 33rd live worker"
        );
        let mut overload_client = TcpStream::connect(address).unwrap();
        let overload = accept_bounded_connection(
            &listener,
            Arc::clone(&active),
            Duration::from_millis(250),
            |_, _| panic!("an overloaded connection must never reach its handler"),
            |_| panic!("an overloaded connection must never reach its worker spawner"),
        )
        .unwrap();
        assert_eq!(overload, ConnectionAdmission::RejectedOverload);
        let mut overload_response = String::new();
        overload_client
            .read_to_string(&mut overload_response)
            .unwrap();
        assert!(overload_response.starts_with("HTTP/1.1 503 Service Unavailable\r\n"));

        for _ in 0..MAX_ACTIVE_CONNECTIONS {
            let error = result_rx.recv().unwrap().unwrap_err();
            assert_eq!(error.status, 408);
        }
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(
            active.load(Ordering::SeqCst),
            0,
            "deadline-expired workers leaked their connection permits"
        );
        let _recovery_client = TcpStream::connect(address).unwrap();
        let mut recovery = None;
        let recovery_admission = accept_bounded_connection(
            &listener,
            Arc::clone(&active),
            Duration::from_millis(250),
            |_, _| {},
            |job| {
                recovery = Some(std::thread::spawn(job));
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(recovery_admission, ConnectionAdmission::Spawned);
        recovery
            .expect("recovery worker must be spawned")
            .join()
            .unwrap();
        assert_eq!(active.load(Ordering::SeqCst), 0);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "absolute deadline did not release all connection slots: {:?}",
            started.elapsed()
        );
        for dripper in drippers {
            dripper.join().unwrap();
        }
    }

    #[test]
    fn one_deadline_rejects_late_dispatch_and_bounds_cumulative_writes() {
        use std::time::Duration;

        let late = Some(RpcReply::Immediate(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "secret": "must be discarded" }
        })));
        assert!(
            reply_before_deadline(late, Instant::now() - Duration::from_millis(1)).is_none(),
            "a synchronous dispatch result survived past the absolute deadline"
        );

        let (_client, mut server) = tcp_pair();
        let mut admitted_chunks = 0;
        let error = write_cancellable_until_with_hook(
            &mut server,
            &vec![b'x'; RESPONSE_CHUNK_BYTES * 4],
            None,
            Instant::now() + Duration::from_millis(40),
            |_| {
                admitted_chunks += 1;
                std::thread::sleep(Duration::from_millis(25));
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(
            admitted_chunks < 4,
            "per-write timeouts replaced, instead of sharing, the absolute deadline"
        );
    }

    #[test]
    fn lifecycle_contention_expires_before_snapshot_registration_or_write() {
        use std::time::Duration;

        let lifecycle = Mutex::new(());
        let epoch = AtomicU64::new(0);
        let unlocked = Mutex::new(HashSet::new());
        let context = RpcContext {
            db: None,
            lifecycle: &lifecycle,
            seal_epoch: &epoch,
            unlocked_folders: &unlocked,
        };
        let lifecycle_guard = lifecycle.lock().unwrap();
        let began = Instant::now();
        assert!(
            context
                .visibility_snapshot(Instant::now() + Duration::from_millis(40))
                .is_err(),
            "snapshot admission ignored the absolute request deadline"
        );
        assert!(began.elapsed() < Duration::from_millis(200));
        drop(lifecycle_guard);

        let db_path =
            crate::storage::db::unique_temp_path("murmur-mcp-lifecycle-deadline", "sqlite");
        let state = AppState::init_at(&db_path, TEST_DEK).unwrap();
        let gate = Arc::new(McpResponseGate::new());
        let (mut client, mut server) = tcp_pair();
        let pending = PendingContentReply {
            id: json!(1),
            snapshot: VisibilitySnapshot {
                seal_epoch: state.seal_epoch.load(Ordering::SeqCst),
                unlocked_folders: HashSet::new(),
                ask_dispatch_generation: Some(state.db.ask_dispatch_generation().unwrap()),
            },
            outcome: Ok("SECRET_MUST_NOT_RENDER_OR_WRITE".into()),
        };
        let state_lifecycle = state.lifecycle.lock().unwrap();
        let began = Instant::now();
        send_rpc_reply(
            &mut server,
            RpcReply::Content(pending),
            &state,
            &gate,
            Instant::now() + Duration::from_millis(40),
        );
        assert!(
            began.elapsed() < Duration::from_millis(200),
            "content admission waited beyond its absolute deadline: {:?}",
            began.elapsed()
        );
        assert_eq!(
            gate.active_count(),
            0,
            "expired admission registered a lease"
        );
        drop(state_lifecycle);

        client.set_nonblocking(true).unwrap();
        let mut leaked = [0_u8; 1];
        assert!(
            matches!(
                client.read(&mut leaked),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
            ),
            "expired lifecycle admission started a response write"
        );
        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn overload_rejection_arms_the_same_absolute_write_deadline() {
        use std::time::Duration;

        let (mut client, mut server) = tcp_pair();
        let deadline = Instant::now() + Duration::from_millis(250);
        reject_overloaded_connection(&mut server, deadline).unwrap();
        let configured = server
            .write_timeout()
            .unwrap()
            .expect("overload response must arm a write timeout");
        assert!(configured > Duration::ZERO);
        assert!(configured <= Duration::from_millis(250));

        let mut response = String::new();
        client.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 503 Service Unavailable\r\n"));

        let (mut late_client, mut late_server) = tcp_pair();
        let error = reject_overloaded_connection(
            &mut late_server,
            Instant::now() - Duration::from_millis(1),
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        late_client.set_nonblocking(true).unwrap();
        let mut leaked = [0_u8; 1];
        assert!(
            matches!(
                late_client.read(&mut leaked),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
            ),
            "an already-expired overload response started a write syscall"
        );
    }

    #[test]
    fn revocation_prevents_a_new_flush_or_drains_an_admitted_flush() {
        use std::sync::mpsc;
        use std::time::Duration;

        let gate = Arc::new(McpResponseGate::new());
        let (_client, mut server) = tcp_pair();
        let lease = gate.register(server.try_clone().unwrap()).unwrap();
        gate.close_and_shutdown();
        let prevented_calls = AtomicUsize::new(0);
        let error = flush_cancellable_until_with_io(
            &mut server,
            Some(&lease),
            Instant::now() + Duration::from_secs(1),
            || panic!("a cancelled flush must not pass admission"),
            |stream| {
                let _ = stream;
                prevented_calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert_eq!(prevented_calls.load(Ordering::SeqCst), 0);

        let gate = Arc::new(McpResponseGate::new());
        let (_client, mut server) = tcp_pair();
        let lease = gate.register(server.try_clone().unwrap()).unwrap();
        let flush_calls = Arc::new(AtomicUsize::new(0));
        let writer_calls = Arc::clone(&flush_calls);
        let (admitted_tx, admitted_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            flush_cancellable_until_with_io(
                &mut server,
                Some(&lease),
                Instant::now() + Duration::from_secs(2),
                || {
                    admitted_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                },
                |stream| {
                    let _ = stream;
                    writer_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            )
        });
        admitted_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let revoke_gate = Arc::clone(&gate);
        let (revoked_tx, revoked_rx) = mpsc::channel();
        let revoker = std::thread::spawn(move || {
            revoke_gate.close_and_shutdown();
            revoked_tx.send(()).unwrap();
        });
        assert!(
            revoked_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "revocation returned while an admitted flush was in flight"
        );
        release_tx.send(()).unwrap();
        writer.join().unwrap().unwrap();
        revoked_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        revoker.join().unwrap();
        assert_eq!(flush_calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn revocation_prevents_a_second_syscall_after_a_partial_write() {
        use std::sync::mpsc;
        use std::time::Duration;

        const PAYLOAD: &[u8] = b"A_SECRET_AFTER_FIRST_PARTIAL_WRITE";
        let gate = Arc::new(McpResponseGate::new());
        let (mut client, mut server) = tcp_pair();
        let lease = gate.register(server.try_clone().unwrap()).unwrap();
        let writes = Arc::new(AtomicUsize::new(0));
        let writer_writes = Arc::clone(&writes);
        let (first_write_tx, first_write_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();

        let writer = std::thread::spawn(move || {
            let mut first_write_tx = Some(first_write_tx);
            let mut release_rx = Some(release_rx);
            write_cancellable_until_with_io(
                &mut server,
                PAYLOAD,
                Some(&lease),
                Instant::now() + Duration::from_secs(2),
                |_| {},
                |syscall_index, _| {
                    if syscall_index == 0 {
                        first_write_tx.take().unwrap().send(()).unwrap();
                        release_rx.take().unwrap().recv().unwrap();
                    }
                },
                move |stream, remaining| {
                    let call = writer_writes.fetch_add(1, Ordering::SeqCst);
                    let write_len = if call == 0 { 1 } else { remaining.len() };
                    stream.write(&remaining[..write_len])
                },
            )
        });
        first_write_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let revoke_gate = Arc::clone(&gate);
        let (revoked_tx, revoked_rx) = mpsc::channel();
        let revoker = std::thread::spawn(move || {
            revoke_gate.close_and_shutdown();
            revoked_tx.send(()).unwrap();
        });
        revoked_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        release_tx.send(()).unwrap();
        assert!(
            writer.join().unwrap().is_err(),
            "the writer continued after response revocation"
        );
        revoker.join().unwrap();
        assert_eq!(
            writes.load(Ordering::SeqCst),
            1,
            "a second write syscall started after response revocation"
        );

        client
            .set_read_timeout(Some(Duration::from_millis(250)))
            .unwrap();
        let mut delivered = Vec::new();
        let _ = client.read_to_end(&mut delivered);
        assert_eq!(delivered, b"A");
        assert!(!delivered
            .windows(b"SECRET".len())
            .any(|window| window == b"SECRET"));
    }

    #[test]
    fn tool_payload_is_bounded_before_json_rendering() {
        let oversized = "x".repeat(MAX_TOOL_TEXT_BYTES + 1);
        let error = bound_tool_outcome(Ok(oversized)).unwrap_err();
        assert_eq!(error.0, -32000);
        assert!(error.1.contains("smaller page"));

        let args = json!({ "maxChars": u64::MAX });
        assert_eq!(mcp_body_window(&args).1, MAX_TOOL_WINDOW_CHARS);

        // Worst-case JSON escaping remains below the transport cap without cloning the text into
        // an intermediate `Value`.
        let pending = PendingContentReply {
            id: json!("bounded-id"),
            snapshot: VisibilitySnapshot {
                seal_epoch: 0,
                unlocked_folders: HashSet::new(),
                ask_dispatch_generation: None,
            },
            outcome: Ok("\u{0001}".repeat(MAX_TOOL_TEXT_BYTES)),
        };
        let body = content_reply_body(&pending).unwrap();
        assert!(body.len() <= MAX_RESPONSE_BYTES);
    }

    #[test]
    fn oversized_or_structured_jsonrpc_ids_are_rejected_before_dispatch() {
        let oversized = "x".repeat(MAX_JSONRPC_ID_BYTES + 1);
        let response = rpc(&json!({
            "jsonrpc": "2.0",
            "id": oversized,
            "method": "tools/call",
            "params": { "name": "search_meetings", "arguments": { "query": "x" } }
        })
        .to_string())
        .unwrap();
        assert_eq!(response["error"]["code"], -32600);
        assert!(response["id"].is_null());

        let response =
            rpc(r#"{"jsonrpc":"2.0","id":{"nested":"value"},"method":"initialize"}"#).unwrap();
        assert_eq!(response["error"]["code"], -32600);

        // A long numeric token may normalize into serde_json's compact Number representation.
        // The security boundary is the representation we will actually clone/render: prove it is
        // measured and remains within budget even when the raw token was much longer.
        let oversized_number = "9".repeat(MAX_JSONRPC_ID_BYTES + 1);
        let response = rpc(&format!(
            r#"{{"jsonrpc":"2.0","id":{oversized_number},"method":"initialize"}}"#
        ))
        .unwrap();
        assert!(response["id"].is_number());
        assert!(
            serde_json::to_string(&response["id"]).unwrap().len() <= MAX_JSONRPC_ID_BYTES,
            "an accepted numeric id rendered beyond its pre-clone byte budget"
        );
        assert!(jsonrpc_id_is_bounded(&json!(u64::MAX)));
    }

    #[test]
    fn response_gate_register_vs_revoke_never_leaves_a_live_lease() {
        use std::sync::Barrier;

        for _ in 0..128 {
            let gate = Arc::new(McpResponseGate::new());
            let (_client, server) = tcp_pair();
            let barrier = Arc::new(Barrier::new(3));
            let register_gate = Arc::clone(&gate);
            let register_barrier = Arc::clone(&barrier);
            let register = std::thread::spawn(move || {
                register_barrier.wait();
                register_gate.register(server)
            });
            let revoke_gate = Arc::clone(&gate);
            let revoke_barrier = Arc::clone(&barrier);
            let revoke = std::thread::spawn(move || {
                revoke_barrier.wait();
                revoke_gate.close_and_shutdown();
            });
            barrier.wait();
            let lease = register.join().unwrap();
            revoke.join().unwrap();
            assert_eq!(gate.active_count(), 0);
            assert!(lease
                .as_ref()
                .map(ResponseLease::is_cancelled)
                .unwrap_or(true));
            assert!(gate.register(tcp_pair().1).is_none());
        }
    }

    #[test]
    fn response_gate_ids_wrap_without_overwriting_or_cross_dropping_leases() {
        let gate = Arc::new(McpResponseGate::new());
        let first = gate.register(tcp_pair().1).unwrap();
        assert_eq!(first.id, 1);
        gate.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .next_id = u64::MAX;

        let at_max = gate.register(tcp_pair().1).unwrap();
        let after_wrap = gate.register(tcp_pair().1).unwrap();
        assert_eq!(at_max.id, u64::MAX);
        assert_eq!(after_wrap.id, 2, "wrapped allocation reused live id 1");
        assert_eq!(gate.active_count(), 3);

        drop(first);
        assert_eq!(
            gate.active_count(),
            2,
            "dropping the old id removed a different live response"
        );
        assert!(!after_wrap.is_cancelled());
        drop(at_max);
        drop(after_wrap);
        assert_eq!(gate.active_count(), 0);
    }

    #[test]
    fn every_visibility_revoking_epoch_entrypoint_cancels_active_leases() {
        for entrypoint in [
            VisibilityRevokingEntrypoint::LockFolder,
            VisibilityRevokingEntrypoint::RelockFolder,
            VisibilityRevokingEntrypoint::RelockAll,
        ] {
            let gate = Arc::new(McpResponseGate::new());
            let (_client, server) = tcp_pair();
            let lease = gate
                .register(server)
                .expect("test response must register before revocation");
            let revocation =
                begin_visibility_revocation_for_gate(Some(Arc::clone(&gate)), entrypoint);
            assert!(lease.is_cancelled(), "{entrypoint:?} left a live response");
            assert_eq!(gate.active_count(), 0, "{entrypoint:?} left a socket");
            assert!(
                gate.register(tcp_pair().1).is_none(),
                "{entrypoint:?} reopened before logical revocation"
            );
            finish_visibility_revocation(revocation);
            assert!(
                gate.register(tcp_pair().1).is_some(),
                "{entrypoint:?} did not reopen after logical revocation"
            );
        }
    }

    #[test]
    fn concurrent_revocations_cannot_reopen_each_other() {
        let gate = Arc::new(McpResponseGate::new());
        gate.close_and_shutdown();
        gate.close_and_shutdown();
        gate.finish_revocation();
        assert!(
            gate.register(tcp_pair().1).is_none(),
            "one completed relock must not reopen while another revoke is in flight"
        );
        gate.finish_revocation();
        let lease = gate
            .register(tcp_pair().1)
            .expect("the final completed revocation reopens admission");
        drop(lease);
    }

    #[test]
    fn concurrent_revocations_both_wait_for_the_same_admitted_chunk() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let gate = Arc::new(McpResponseGate::new());
        let (_client, server) = tcp_pair();
        let lease = gate.register(server).unwrap();
        let chunk = lease.begin_chunk().unwrap();
        let cancellation = Arc::clone(&lease.cancellation);
        let mut completions = Vec::new();
        let mut revokers = Vec::new();
        for _ in 0..2 {
            let revoke_gate = Arc::clone(&gate);
            let (done_tx, done_rx) = mpsc::channel();
            completions.push(done_rx);
            revokers.push(std::thread::spawn(move || {
                revoke_gate.close_and_shutdown();
                done_tx.send(()).unwrap();
            }));
        }
        let deadline = Instant::now() + Duration::from_secs(1);
        while !cancellation.is_cancelled() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(cancellation.is_cancelled());
        for completion in &completions {
            assert!(
                completion.try_recv().is_err(),
                "a concurrent revoke returned before the shared chunk drained"
            );
        }
        drop(chunk);
        for completion in completions {
            completion.recv_timeout(Duration::from_secs(1)).unwrap();
        }
        for revoker in revokers {
            revoker.join().unwrap();
        }
    }

    #[test]
    fn incomplete_logical_revocation_leaves_admission_closed_fail_closed() {
        let gate = Arc::new(McpResponseGate::new());
        let revocation = begin_visibility_revocation_for_gate(
            Some(Arc::clone(&gate)),
            VisibilityRevokingEntrypoint::LockFolder,
        );
        drop(revocation);
        assert!(
            gate.register(tcp_pair().1).is_none(),
            "a failed lock must not silently reopen content admission"
        );
    }

    #[test]
    fn revocation_drains_admitted_chunk_before_visibility_can_change() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        const MARKER: &[u8] = b"SECRET_AT_REVOKE_BOUNDARY";
        for (target_chunk, marker_offset) in [
            (0, 0),
            (0, RESPONSE_CHUNK_BYTES - MARKER.len()),
            (1, 0),
            (1, RESPONSE_CHUNK_BYTES - MARKER.len()),
        ] {
            let gate = Arc::new(McpResponseGate::new());
            let (mut client, mut server) = tcp_pair();
            server
                .set_write_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let lease = gate
                .register(server.try_clone().unwrap())
                .expect("boundary response must register");
            let cancellation = Arc::clone(&lease.cancellation);
            let mut payload = vec![b'B'; (target_chunk + 1) * RESPONSE_CHUNK_BYTES];
            let marker_start = target_chunk * RESPONSE_CHUNK_BYTES + marker_offset;
            payload[marker_start..marker_start + MARKER.len()].copy_from_slice(MARKER);

            let (admitted_tx, admitted_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            let writer = std::thread::spawn(move || {
                let mut admitted_tx = Some(admitted_tx);
                let mut release_rx = Some(release_rx);
                write_cancellable_with_hook(&mut server, &payload, Some(&lease), |chunk_index| {
                    if chunk_index == target_chunk {
                        admitted_tx
                            .take()
                            .expect("target chunk admitted once")
                            .send(())
                            .unwrap();
                        release_rx
                            .take()
                            .expect("target chunk released once")
                            .recv()
                            .unwrap();
                    }
                })
            });
            admitted_rx.recv_timeout(Duration::from_secs(1)).unwrap();

            let revoke_gate = Arc::clone(&gate);
            let (revoked_tx, revoked_rx) = mpsc::channel();
            let revoker = std::thread::spawn(move || {
                revoke_gate.close_and_shutdown();
                revoked_tx.send(()).unwrap();
            });
            let cancellation_deadline = Instant::now() + Duration::from_secs(1);
            while !cancellation.is_cancelled() && Instant::now() < cancellation_deadline {
                std::thread::yield_now();
            }
            assert!(
                cancellation.is_cancelled(),
                "revocation never cancelled chunk {target_chunk}"
            );
            assert!(
                revoked_rx.try_recv().is_err(),
                "revocation returned while admitted chunk {target_chunk} was still in flight"
            );

            release_tx.send(()).unwrap();
            assert!(
                writer.join().unwrap().is_err(),
                "a chunk admitted before cancellation wrote after socket shutdown"
            );
            revoked_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            revoker.join().unwrap();

            client
                .set_read_timeout(Some(Duration::from_millis(250)))
                .unwrap();
            let mut delivered = Vec::new();
            let _ = client.read_to_end(&mut delivered);
            assert!(
                !delivered
                    .windows(MARKER.len())
                    .any(|window| window == MARKER),
                "secret at chunk {target_chunk} offset {marker_offset} crossed revocation"
            );
        }
    }

    #[test]
    fn slow_reader_is_cancelled_before_lifecycle_wait_and_receives_no_secret_tail() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let gate = Arc::new(McpResponseGate::new());
        let (mut client, mut server) = tcp_pair();
        server
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let lifecycle = Arc::new(Mutex::new(()));
        let lease = {
            let _lifecycle = lifecycle.lock().unwrap();
            gate.register(server.try_clone().unwrap()).unwrap()
        };
        let (started_tx, started_rx) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            let first = vec![b'A'; RESPONSE_CHUNK_BYTES];
            write_cancellable(&mut server, &first, Some(&lease)).unwrap();
            started_tx.send(()).unwrap();
            let mut rest = vec![b'B'; 32 << 20];
            rest.extend_from_slice(b"SECRET_TAIL_AFTER_REVOKE");
            let _ = write_cancellable(&mut server, &rest, Some(&lease));
        });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let began = Instant::now();
        gate.close_and_shutdown();
        let _lifecycle = lifecycle.lock().unwrap();
        assert!(
            began.elapsed() < Duration::from_millis(250),
            "slow MCP reader delayed lifecycle acquisition: {:?}",
            began.elapsed()
        );
        drop(_lifecycle);
        writer.join().unwrap();

        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut delivered = Vec::new();
        let _ = client.read_to_end(&mut delivered);
        assert!(
            !delivered
                .windows(b"SECRET_TAIL_AFTER_REVOKE".len())
                .any(|window| window == b"SECRET_TAIL_AFTER_REVOKE"),
            "a payload tail was written after response revocation"
        );
        assert_eq!(gate.active_count(), 0);
    }

    #[test]
    fn initial_lock_cancels_slow_response_before_lifecycle_and_leaks_no_tail() {
        use crate::storage::models::Folder;
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let db_path = crate::storage::db::unique_temp_path("murmur-mcp-initial-lock", "sqlite");
        let state = AppState::init_at(&db_path, TEST_DEK).unwrap();
        state
            .db
            .insert_folder(&Folder {
                id: "private".into(),
                name: "Private".into(),
                path: "Private".into(),
                parent_id: None,
                locked: false,
                created_at: "2026-07-28T00:00:00Z".into(),
            })
            .unwrap();
        *state.master_kek.lock().unwrap() = Some(zeroize::Zeroizing::new([0x42; 32]));

        let gate = Arc::new(McpResponseGate::new());
        let (mut client, mut server) = tcp_pair();
        server
            .set_write_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let lease = gate.register(server.try_clone().unwrap()).unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let writer = std::thread::spawn(move || {
            write_cancellable(&mut server, &vec![b'A'; RESPONSE_CHUNK_BYTES], Some(&lease))
                .unwrap();
            started_tx.send(()).unwrap();
            let mut rest = vec![b'B'; 32 << 20];
            rest.extend_from_slice(b"SECRET_TAIL_AFTER_INITIAL_LOCK");
            let _ = write_cancellable(&mut server, &rest, Some(&lease));
        });
        started_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let began = Instant::now();
        let revocation = begin_visibility_revocation_for_gate(
            Some(Arc::clone(&gate)),
            VisibilityRevokingEntrypoint::LockFolder,
        );
        crate::commands::lock_folder_with_visibility_revocation(&state, "private", revocation)
            .unwrap();
        assert!(
            began.elapsed() < Duration::from_secs(1),
            "slow MCP reader delayed initial lock: {:?}",
            began.elapsed()
        );
        assert!(state.db.folder_by_id("private").unwrap().unwrap().locked);
        writer.join().unwrap();

        client
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut delivered = Vec::new();
        let _ = client.read_to_end(&mut delivered);
        assert!(
            !delivered
                .windows(b"SECRET_TAIL_AFTER_INITIAL_LOCK".len())
                .any(|window| window == b"SECRET_TAIL_AFTER_INITIAL_LOCK"),
            "a payload tail was delivered after initial folder lock"
        );
        assert_eq!(gate.active_count(), 0);
        drop(state);
        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn host_allow_list_is_loopback_only() {
        let production_addr = mcp_listener_addr();
        assert_eq!(*production_addr.ip(), Ipv4Addr::LOCALHOST);
        assert!(
            production_addr.ip().is_loopback(),
            "the production listener address cannot be widened by config or DNS"
        );
        // E2: only the two exact loopback authorities are accepted.
        assert!(ALLOWED_HOSTS.contains(&"127.0.0.1:8765"));
        assert!(ALLOWED_HOSTS.contains(&"localhost:8765"));
        // Anything else (rebinding host, external name, bare host w/o port) is NOT in the list.
        for bad in [
            "evil.example.com:8765",
            "0.0.0.0:8765",
            "127.0.0.1",
            "localhost",
            "127.0.0.1:9999",
        ] {
            assert!(!ALLOWED_HOSTS.contains(&bad), "{bad} must not be allowed");
        }
    }

    // ── Phase 2b: search_semantic MCP tool (gated) ─────────────────────────────────────────────

    const TEST_DEK: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn temp_db() -> (Db, PathBuf) {
        let p = crate::storage::db::unique_temp_path("murmur-mcp-test", "sqlite");
        let db = Db::open_with_key(&p, TEST_DEK).unwrap();
        (db, p)
    }

    fn seed(db: &Db, mid: &str, title: &str, md: &str, folder: Option<&str>) {
        use crate::storage::models::{Meeting, MeetingStatus, NoteRecord};
        db.insert_meeting(&Meeting {
            id: mid.to_string(),
            started_at: "2026-06-27T09:00:00Z".to_string(),
            ended_at: None,
            title: Some(title.to_string()),
            duration_s: 60,
            audio_path: None,
            status: MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&NoteRecord {
            meeting_id: mid.to_string(),
            provider_id: "claude_code".to_string(),
            markdown: md.to_string(),
            created_at: "2026-06-27T09:05:00Z".to_string(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.set_note_folder(mid, folder).unwrap();
    }

    /// Once a content-bearing result has been materialized, a relock/epoch change before JSON-RPC
    /// response production discards the whole payload and emits only the generic retry error.
    #[test]
    fn content_response_revalidation_discards_materialized_secret_after_epoch_bump() {
        use crate::storage::models::Folder;

        let (db, p) = temp_db();
        db.insert_folder(&Folder {
            id: "f-lock".into(),
            name: "Private".into(),
            path: "Private".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-28T00:00:00Z".into(),
        })
        .unwrap();
        seed(
            &db,
            "secret-meeting",
            "SECRET lifecycle title",
            "SECRET lifecycle body",
            Some("f-lock"),
        );
        db.set_folder_locked("f-lock", true, None).unwrap();

        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        let lifecycle = Mutex::new(());
        let epoch = AtomicU64::new(0);
        let unlocked = Mutex::new(unlocked);
        let context = RpcContext {
            db: Some(&db),
            lifecycle: &lifecycle,
            seal_epoch: &epoch,
            unlocked_folders: &unlocked,
        };
        let params = json!({
            "name": "search_meetings",
            "arguments": { "query": "SECRET" }
        });
        let pending = match handle_tool_call(
            &context,
            json!(1),
            Some(&params),
            Instant::now() + REQUEST_DEADLINE,
        ) {
            RpcReply::Content(pending) => pending,
            RpcReply::Immediate(response) => {
                panic!("content call unexpectedly finalized early: {response}")
            }
        };
        assert!(
            pending
                .outcome
                .as_ref()
                .is_ok_and(|text| text.contains("SECRET")),
            "the regression must materialize secret content before simulating relock"
        );

        {
            let _lifecycle = lifecycle.lock().unwrap();
            epoch.fetch_add(1, Ordering::SeqCst);
            unlocked.lock().unwrap().clear();
        }
        let response = {
            let _lifecycle = lifecycle.lock().unwrap();
            finalize_content_reply(pending, &epoch, &unlocked)
        };
        assert!(
            !response.to_string().contains("SECRET"),
            "a response produced after the visibility epoch changed leaked materialized content"
        );
        assert_eq!(response["error"]["code"], VISIBILITY_RETRY_CODE);
        assert_eq!(
            response["error"]["message"],
            Value::String(VISIBILITY_RETRY_MESSAGE.into())
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn content_response_revalidation_discards_materialized_living_answer_after_ask_generation_bump()
    {
        const MARKER: &str = "OLD_LIVING_ANSWER_MUST_NOT_REACH_MCP";

        let db_path =
            crate::storage::db::unique_temp_path("murmur-mcp-living-answer-generation", "sqlite");
        let state = AppState::init_at(&db_path, TEST_DEK).unwrap();
        state
            .db
            .insert_dashboard(
                "board-ask-generation",
                "Ask generation board",
                None,
                None,
                "2026-08-13T10:00:00Z",
            )
            .unwrap();
        state
            .db
            .insert_dashboard_living_answer_tile(
                "tile-ask-generation",
                "board-ask-generation",
                4,
                "What changed?",
                "[]",
                "2026-08-13T10:00:01Z",
            )
            .unwrap();
        let budget = {
            let config = state.config.lock().unwrap();
            crate::commands::resolved_ask_corpus_budget(&config)
        };
        let composite = crate::commands::living_answer_composite_context(
            &state.db,
            "board-ask-generation",
            &HashSet::new(),
            budget,
        )
        .unwrap();
        assert!(state
            .db
            .store_dashboard_living_answer_cas(
                "tile-ask-generation",
                "board-ask-generation",
                "What changed?",
                MARKER,
                "2026-08-13T10:01:00Z",
                "[]",
                composite.witness.generation,
                &composite.witness.input_digest,
                budget,
            )
            .unwrap());

        let context = RpcContext::from_state(&state);
        let params = json!({
            "name": "get_dashboard",
            "arguments": { "dashboardId": "board-ask-generation" }
        });
        let pending = match handle_tool_call(
            &context,
            json!(81),
            Some(&params),
            Instant::now() + REQUEST_DEADLINE,
        ) {
            RpcReply::Content(pending) => pending,
            RpcReply::Immediate(response) => {
                panic!("get_dashboard unexpectedly finalized early: {response}")
            }
        };
        assert!(
            pending
                .outcome
                .as_ref()
                .is_ok_and(|text| text.contains(MARKER)),
            "the regression must materialize the valid old-generation cache before the race"
        );

        {
            let _lifecycle = state
                .lifecycle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.db.advance_ask_dispatch_generation().unwrap();
        }
        let gate = Arc::new(McpResponseGate::new());
        let (mut client, mut server) = tcp_pair();
        send_rpc_reply(
            &mut server,
            RpcReply::Content(pending),
            &state,
            &gate,
            Instant::now() + REQUEST_DEADLINE,
        );
        drop(server);
        let mut wire = String::new();
        client.read_to_string(&mut wire).unwrap();
        let (_, body) = wire.split_once("\r\n\r\n").expect("complete HTTP reply");
        let response: Value = serde_json::from_str(body).expect("JSON-RPC response");
        assert_eq!(response["error"]["code"], VISIBILITY_RETRY_CODE);
        assert_eq!(
            response["error"]["message"],
            Value::String(VISIBILITY_RETRY_MESSAGE.into())
        );
        assert!(
            !response.to_string().contains(MARKER),
            "the response gate leaked a Living Answer materialized under an old Ask generation"
        );
        assert_eq!(gate.active_count(), 0);

        drop(state);
        let _ = std::fs::remove_file(db_path);
    }

    /// KnowledgeDiff note context is reachable only through the fixed loopback MCP catalog, and a
    /// relock after materialization but before response production discards content AND bounded
    /// source-count metadata. The model-facing/GatedToolExecutor boundary is asserted separately in
    /// `tools::tests::local_mcp_only_tools_remain_outside_cloud_agent_catalogs`.
    #[test]
    fn knowledge_diff_note_context_is_loopback_only_and_revoked_after_relock() {
        use crate::storage::models::{EntityKind, Folder};

        assert!(mcp_listener_addr().ip().is_loopback());
        assert_eq!(*mcp_listener_addr().ip(), Ipv4Addr::LOCALHOST);
        assert!(
            tools_spec()
                .as_array()
                .is_some_and(|tools| tools.iter().any(|tool| tool["name"] == "knowledge_diff")),
            "knowledge_diff must stay in the fixed local MCP catalog"
        );
        assert!(
            crate::tools::tool_specs()
                .iter()
                .all(|tool| tool.name != "knowledge_diff"),
            "knowledge_diff note context must not enter a model-facing tool catalog"
        );

        let (db, p) = temp_db();
        db.insert_folder(&Folder {
            id: "f-kd-race".into(),
            name: "Private knowledge diff".into(),
            path: "Private knowledge diff".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-29T00:00:00Z".into(),
        })
        .unwrap();
        let entity_id = db.upsert_entity("Atlas Race", EntityKind::Project).unwrap();
        for index in 0..=100 {
            let meeting_id = format!("m-kd-race-{index:03}");
            seed(
                &db,
                &meeting_id,
                &format!("PRIVATE_KD_TITLE_{index:03}"),
                &format!("## Decisions\n- PRIVATE_KD_CONTENT_{index:03}.\n"),
                Some("f-kd-race"),
            );
            db.add_mention(&entity_id, &meeting_id).unwrap();
        }
        db.set_folder_locked("f-kd-race", true, None).unwrap();

        let mut unlocked = HashSet::new();
        unlocked.insert("f-kd-race".to_string());
        let lifecycle = Mutex::new(());
        let epoch = AtomicU64::new(0);
        let unlocked = Mutex::new(unlocked);
        let context = RpcContext {
            db: Some(&db),
            lifecycle: &lifecycle,
            seal_epoch: &epoch,
            unlocked_folders: &unlocked,
        };
        let params = json!({
            "name": "knowledge_diff",
            "arguments": {
                "entity": "Atlas Race",
                "from": "2026-01-01T00:00:00Z",
                "to": "2026-12-31T23:59:59Z"
            }
        });
        let pending = match handle_tool_call(
            &context,
            json!(71),
            Some(&params),
            Instant::now() + REQUEST_DEADLINE,
        ) {
            RpcReply::Content(pending) => pending,
            RpcReply::Immediate(response) => {
                panic!("knowledge_diff unexpectedly finalized early: {response}")
            }
        };
        let materialized = pending
            .outcome
            .as_ref()
            .unwrap_or_else(|error| panic!("knowledge_diff failed before relock: {error:?}"));
        for expected in [
            "PRIVATE_KD_TITLE_100",
            "PRIVATE_KD_CONTENT_100",
            "source:m-kd-race-100",
            "HISTORICAL NOTE CONTEXT TRUNCATED",
            "newest 100 visible mentioning meetings scanned",
        ] {
            assert!(
                materialized.contains(expected),
                "race fixture did not materialize {expected:?}: {materialized}"
            );
        }

        {
            let _lifecycle = lifecycle.lock().unwrap();
            epoch.fetch_add(1, Ordering::SeqCst);
            unlocked.lock().unwrap().clear();
        }
        let response = {
            let _lifecycle = lifecycle.lock().unwrap();
            finalize_content_reply(pending, &epoch, &unlocked)
        };
        assert_eq!(response["error"]["code"], VISIBILITY_RETRY_CODE);
        assert_eq!(
            response["error"]["message"],
            Value::String(VISIBILITY_RETRY_MESSAGE.into())
        );
        let serialized = response.to_string();
        for forbidden in [
            "PRIVATE_KD_TITLE",
            "PRIVATE_KD_CONTENT",
            "m-kd-race",
            "Atlas Race",
            "HISTORICAL NOTE CONTEXT",
            "meeting",
            "scanned",
            "TRUNCATED",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "knowledge_diff leaked materialized content/count metadata after relock: {serialized}"
            );
        }
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn transcript_navigation_response_gate_discards_search_and_chapters_after_relock() {
        use crate::storage::models::{Folder, MeetingTimeline, TopicSpan};
        use crate::transcribe::types::Segment;

        const FOLDER_ID: &str = "f-nav-race";
        const MEETING_ID: &str = "m-nav-race";
        const TITLE: &str = "RACE_PRIVATE_TITLE";
        const SECRET: &str = "race private transcript needle";
        const ORDINARY_SPEECH: &str = "race private ordinary speaker";
        const TOPIC: &str = "RACE_PRIVATE_CHAPTER";
        const ENROLLED_NAME: &str = "RACE_PRIVATE_ENROLLED_NAME";

        let db_path = crate::storage::db::unique_temp_path("murmur-mcp-nav-race", "sqlite");
        let state = AppState::init_at(&db_path, TEST_DEK).unwrap();
        state
            .db
            .insert_folder(&Folder {
                id: FOLDER_ID.into(),
                name: "Private navigation race".into(),
                path: "Private navigation race".into(),
                parent_id: None,
                locked: false,
                created_at: "2026-07-28T00:00:00Z".into(),
            })
            .unwrap();
        seed(
            &state.db,
            MEETING_ID,
            TITLE,
            "private note",
            Some(FOLDER_ID),
        );
        state
            .db
            .insert_segments(
                MEETING_ID,
                &[
                    Segment {
                        idx: 17,
                        start_s: 5.0,
                        end_s: 8.0,
                        text: SECRET.into(),
                        speaker: Some("others-0".into()),
                        confidence: None,
                    },
                    Segment {
                        idx: 18,
                        start_s: 9.0,
                        end_s: 11.0,
                        text: ORDINARY_SPEECH.into(),
                        speaker: Some("others".into()),
                        confidence: None,
                    },
                ],
            )
            .unwrap();
        state
            .db
            .insert_voiceprint(
                "vp-nav-race",
                MEETING_ID,
                0,
                Some(ENROLLED_NAME),
                &[0.1, 0.2],
                "2026-07-28T00:00:00Z",
            )
            .unwrap();
        state
            .db
            .set_timeline_data(
                MEETING_ID,
                &serde_json::to_string(&MeetingTimeline {
                    speakers: Vec::new(),
                    topics: vec![TopicSpan {
                        label: TOPIC.into(),
                        start_s: 4.5,
                        end_s: 11.5,
                    }],
                })
                .unwrap(),
            )
            .unwrap();
        state.db.set_folder_locked(FOLDER_ID, true, None).unwrap();
        let gate = Arc::new(McpResponseGate::new());

        let cases = [
            (
                "search_transcript",
                json!({
                    "query": "race private",
                    "meetingId": MEETING_ID,
                    "channel": "system"
                }),
                SECRET,
            ),
            (
                "get_meeting_chapters",
                json!({ "meetingId": MEETING_ID, "channel": "system" }),
                TOPIC,
            ),
        ];
        for (case_index, (name, arguments, materialized_marker)) in cases.into_iter().enumerate() {
            {
                let _lifecycle = state
                    .lifecycle
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state
                    .unlocked_folders
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(FOLDER_ID.into());
            }
            let context = RpcContext::from_state(&state);
            let request = json!({
                "jsonrpc": "2.0",
                "id": 90 + case_index,
                "method": "tools/call",
                "params": { "name": name, "arguments": arguments }
            })
            .to_string();
            let deadline = Instant::now() + REQUEST_DEADLINE;
            let reply = handle_rpc(&context, &request, None, None, deadline)
                .expect("navigation tool must produce a response");
            match &reply {
                RpcReply::Content(pending) => {
                    let text = pending
                        .outcome
                        .as_ref()
                        .unwrap_or_else(|error| panic!("{name} failed before relock: {error:?}"));
                    assert!(
                        text.contains(materialized_marker),
                        "{name} must materialize protected content before the deterministic relock"
                    );
                    if name == "search_transcript" {
                        for expected in [
                            SECRET,
                            ORDINARY_SPEECH,
                            TITLE,
                            ENROLLED_NAME,
                            "Speaker 1",
                            "Others",
                            "shown=2",
                            "total=2",
                            "candidateMeetings",
                            "seg 17",
                            "seg 18",
                            "@00:05",
                            "@00:09",
                            "(5.0s)",
                            "(9.0s)",
                            "offset ",
                            "channel=system",
                        ] {
                            assert!(
                                text.contains(expected),
                                "search race fixture did not materialize {expected:?}: {text}"
                            );
                        }
                    } else {
                        for expected in [
                            TOPIC,
                            ENROLLED_NAME,
                            "Speaker 1",
                            "offset ",
                            "channel=system",
                        ] {
                            assert!(
                                text.contains(expected),
                                "chapter race fixture did not materialize {expected:?}: {text}"
                            );
                        }
                    }
                }
                RpcReply::Immediate(response) => {
                    panic!("{name} unexpectedly bypassed the response gate: {response}")
                }
            }

            {
                let _lifecycle = state
                    .lifecycle
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                state.seal_epoch.fetch_add(1, Ordering::SeqCst);
                state
                    .unlocked_folders
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(FOLDER_ID);
            }

            let (mut client, mut server) = tcp_pair();
            send_rpc_reply(&mut server, reply, &state, &gate, deadline);
            drop(server);
            let mut wire = String::new();
            client.read_to_string(&mut wire).unwrap();
            let (_, body) = wire.split_once("\r\n\r\n").expect("complete HTTP reply");
            let response: Value = serde_json::from_str(body).expect("JSON-RPC response");
            assert_eq!(
                response["error"]["code"], VISIBILITY_RETRY_CODE,
                "{name} must fail closed after relock: {response}"
            );
            let serialized = response.to_string();
            for forbidden in [
                SECRET,
                ORDINARY_SPEECH,
                TITLE,
                TOPIC,
                ENROLLED_NAME,
                "Speaker 1",
                "Others",
                "shown=",
                "total=",
                "counted=",
                "candidateMeetings",
                "scanTruncated",
                "[meeting:m-nav-race]",
                "seg 17",
                "seg 18",
                "@00:05",
                "@00:09",
                "(5.0s)",
                "(9.0s)",
                "offset",
                "channel=",
            ] {
                assert!(
                    !serialized.contains(forbidden),
                    "{name} leaked materialized navigation metadata after relock: {serialized}"
                );
            }
        }

        drop(state);
        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn visibility_epoch_prevents_unlock_set_aba() {
        let mut original = HashSet::new();
        original.insert("f-lock".to_string());
        let snapshot = VisibilitySnapshot {
            seal_epoch: 41,
            unlocked_folders: original.clone(),
            ask_dispatch_generation: None,
        };
        let epoch = AtomicU64::new(41);
        let unlocked = Mutex::new(original);
        {
            let mut folders = unlocked.lock().unwrap();
            folders.clear();
            epoch.fetch_add(1, Ordering::SeqCst);
            folders.insert("f-lock".to_string());
        }
        assert!(
            visibility_is_current(
                &snapshot,
                &epoch,
                &unlocked,
                Instant::now() + REQUEST_DEADLINE,
            ) == Ok(false),
            "same-looking unlock membership must not hide an intervening revoke"
        );
    }

    #[test]
    fn content_response_revalidation_returns_materialized_result_without_lifecycle_change() {
        let (db, p) = temp_db();
        seed(
            &db,
            "visible-meeting",
            "SECRET unchanged title",
            "SECRET unchanged body",
            None,
        );
        let lifecycle = Mutex::new(());
        let epoch = AtomicU64::new(7);
        let unlocked = Mutex::new(HashSet::new());
        let context = RpcContext {
            db: Some(&db),
            lifecycle: &lifecycle,
            seal_epoch: &epoch,
            unlocked_folders: &unlocked,
        };
        let params = json!({
            "name": "search_meetings",
            "arguments": { "query": "SECRET" }
        });
        let pending = match handle_tool_call(
            &context,
            json!(2),
            Some(&params),
            Instant::now() + REQUEST_DEADLINE,
        ) {
            RpcReply::Content(pending) => pending,
            RpcReply::Immediate(response) => {
                panic!("content call unexpectedly finalized early: {response}")
            }
        };
        let response = {
            let _lifecycle = lifecycle.lock().unwrap();
            finalize_content_reply(pending, &epoch, &unlocked)
        };
        assert!(
            response.to_string().contains("SECRET unchanged title"),
            "an unchanged lifecycle must return the gated result: {response}"
        );
        assert!(response.get("result").is_some());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn org_read_gates_hold_through_the_production_tcp_response_sink() {
        use crate::storage::OrgState;

        const QUERY: &str = "quartz permission boundary";
        const STALE_MARKER: &str = "STALE_ORG_SECRET_MARKER";

        let db_path = crate::storage::db::unique_temp_path("murmur-mcp-org-tcp-gates", "sqlite");
        let state = AppState::init_at(&db_path, TEST_DEK).unwrap();
        state
            .db
            .set_setting("semantic_search_enabled", "false")
            .unwrap();
        let gate = Arc::new(McpResponseGate::new());

        let rpc_over_tcp = |query: &str| {
            let context = RpcContext::from_state(&state);
            let request = json!({
                "jsonrpc": "2.0",
                "id": 17,
                "method": "tools/call",
                "params": {
                    "name": "org_search",
                    "arguments": { "query": query }
                }
            })
            .to_string();
            let deadline = Instant::now() + REQUEST_DEADLINE;
            let reply = handle_rpc(&context, &request, None, None, deadline)
                .expect("org_search request must produce a response");
            assert!(
                matches!(&reply, RpcReply::Content(_)),
                "org_search must traverse the content-response gate"
            );
            let (mut client, mut server) = tcp_pair();
            send_rpc_reply(&mut server, reply, &state, &gate, deadline);
            let mut wire = String::new();
            client.read_to_string(&mut wire).unwrap();
            assert!(wire.starts_with("HTTP/1.1 200 OK\r\n"));
            wire
        };

        // Simulate a stale/unauthorized decrypted replica row written outside the normal
        // member-gated feed commit. With no local org_state membership it must still disappear at
        // the authoritative reader and therefore at the new TCP sink.
        state
            .db
            .upsert_org_item(
                "org-stale",
                "org-1",
                1,
                "mallory",
                STALE_MARKER,
                QUERY,
                "2026-07-28T00:00:00Z",
                1,
                1,
                &[0x31; 32],
                None,
                None,
                Some(&crate::embed::StubEmbedder),
            )
            .unwrap();
        let non_member = rpc_over_tcp(QUERY);
        assert!(
            !non_member.contains(STALE_MARKER) && !non_member.contains(QUERY),
            "a stale replica bypassed local membership at the TCP sink: {non_member}"
        );

        state
            .db
            .upsert_org_state(&OrgState {
                org_id: "org-1".into(),
                name: "Acme".into(),
                role: "member".into(),
                joined_at: "2026-07-28T00:00:00Z".into(),
                consented: false,
                last_seq: 0,
                generation: 1,
                context_enabled: true,
            })
            .unwrap();
        let joined = rpc_over_tcp(QUERY);
        assert!(
            joined.contains(STALE_MARKER) && joined.contains("[org · mallory]"),
            "joined+enabled control did not reach the production TCP sink: {joined}"
        );

        state.db.set_org_context_enabled("org-1", false).unwrap();
        let disabled = rpc_over_tcp(QUERY);
        assert!(
            !disabled.contains(STALE_MARKER) && !disabled.contains(QUERY),
            "context-disabled org content reached the TCP sink: {disabled}"
        );

        state.db.set_org_context_enabled("org-1", true).unwrap();
        state.db.tombstone_org_item("org-stale").unwrap();
        let tombstoned = rpc_over_tcp(QUERY);
        assert!(
            !tombstoned.contains(STALE_MARKER) && !tombstoned.contains(QUERY),
            "tombstoned org content reached the TCP sink: {tombstoned}"
        );

        // The authoritative org reader pushes down a 20-hit bound, and the generic MCP transport
        // applies its independent byte cap before disclosure.
        for index in 0..25_u8 {
            let item_id = format!("org-bounded-{index:02}");
            let title = format!("Bounded sink item {index:02}");
            state
                .db
                .upsert_org_item(
                    &item_id,
                    "org-1",
                    u64::from(index) + 10,
                    "bounded-author",
                    &title,
                    "bounded sink marker safe shared content",
                    "2026-07-28T00:00:00Z",
                    1,
                    1,
                    &[index; 32],
                    None,
                    None,
                    Some(&crate::embed::StubEmbedder),
                )
                .unwrap();
        }
        let bounded = rpc_over_tcp("bounded sink marker");
        let disclosed_hits = bounded.matches("[org · bounded-author]").count();
        assert!(
            (1..=20).contains(&disclosed_hits),
            "org reader did not preserve its 20-hit bound at the TCP sink: {disclosed_hits}"
        );
        assert!(
            bounded.len() <= MAX_RESPONSE_BYTES,
            "bounded org results exceeded the transport response cap"
        );

        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn transcript_navigation_round_trips_through_production_listener_and_session_visibility() {
        use crate::storage::models::{Folder, MeetingTimeline, TopicSpan};
        use crate::transcribe::types::Segment;

        const MEETING_ID: &str = "mcp-nav-private";
        const FOLDER_ID: &str = "mcp-nav-folder";
        const SECRET: &str = "unique navigation needle";

        let db_path = crate::storage::db::unique_temp_path("murmur-mcp-nav-listener", "sqlite");
        let state = Arc::new(AppState::init_at(&db_path, TEST_DEK).unwrap());
        state
            .db
            .insert_folder(&Folder {
                id: FOLDER_ID.into(),
                name: "Private navigation".into(),
                path: "Private navigation".into(),
                parent_id: None,
                locked: false,
                created_at: "2026-07-28T00:00:00Z".into(),
            })
            .unwrap();
        seed(
            &state.db,
            MEETING_ID,
            "Private navigation meeting",
            "private note",
            Some(FOLDER_ID),
        );
        let segments = vec![
            Segment {
                idx: 4,
                start_s: 2.0,
                end_s: 3.0,
                text: "mic lane preface".into(),
                speaker: Some("me".into()),
                confidence: None,
            },
            Segment {
                idx: 8,
                start_s: 5.0,
                end_s: 8.0,
                text: SECRET.into(),
                speaker: Some("others".into()),
                confidence: None,
            },
        ];
        state.db.insert_segments(MEETING_ID, &segments).unwrap();
        state
            .db
            .set_timeline_data(
                MEETING_ID,
                &serde_json::to_string(&MeetingTimeline {
                    speakers: Vec::new(),
                    topics: vec![TopicSpan {
                        label: "Navigation topic".into(),
                        start_s: 4.5,
                        end_s: 8.5,
                    }],
                })
                .unwrap(),
            )
            .unwrap();
        state.db.set_folder_locked(FOLDER_ID, true, None).unwrap();

        let gate = Arc::new(McpResponseGate::new());
        let active = Arc::new(AtomicUsize::new(0));
        let rpc_over_production_listener = |name: &str, arguments: Value| -> String {
            let listener = TcpListener::bind(SocketAddrV4::new(MCP_BIND_IP, 0))
                .expect("ephemeral production-loopback listener");
            let mut client =
                TcpStream::connect(listener.local_addr().unwrap()).expect("loopback client");
            client
                .set_read_timeout(Some(READ_TIMEOUT))
                .expect("client read timeout");
            let body = json!({
                "jsonrpc": "2.0",
                "id": 73,
                "method": "tools/call",
                "params": { "name": name, "arguments": arguments }
            })
            .to_string();
            let request = format!(
                "POST /mcp HTTP/1.1\r\nHost: localhost:8765\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            client.write_all(request.as_bytes()).unwrap();
            client.shutdown(Shutdown::Write).unwrap();

            let worker_state = Arc::clone(&state);
            let worker_gate = Arc::clone(&gate);
            let admission = accept_bounded_connection(
                &listener,
                Arc::clone(&active),
                REQUEST_DEADLINE,
                move |stream, deadline| {
                    handle_connection_with_state(
                        stream,
                        worker_state.as_ref(),
                        worker_gate,
                        None,
                        deadline,
                    );
                },
                |job| {
                    job();
                    Ok(())
                },
            )
            .expect("production listener admission");
            assert_eq!(admission, ConnectionAdmission::Spawned);

            let mut wire = String::new();
            client.read_to_string(&mut wire).unwrap();
            let (headers, body) = wire.split_once("\r\n\r\n").expect("complete HTTP response");
            assert!(headers.starts_with("HTTP/1.1 200 OK"), "{headers}");
            let payload: Value = serde_json::from_str(body).expect("JSON-RPC response");
            payload["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or_else(|| panic!("missing MCP text content: {payload}"))
                .to_string()
        };

        let get_args = json!({
            "meetingId": MEETING_ID,
            "transcriptFormat": "structured",
            "channel": "system",
            "includeNote": false
        });
        let search_args = json!({
            "query": "navigation needle",
            "meetingId": MEETING_ID,
            "channel": "system"
        });
        let chapters_args = json!({ "meetingId": MEETING_ID, "channel": "system" });

        let locked_meeting = rpc_over_production_listener("get_meeting", get_args.clone());
        let locked_search = rpc_over_production_listener("search_transcript", search_args.clone());
        let locked_chapters =
            rpc_over_production_listener("get_meeting_chapters", chapters_args.clone());
        assert_eq!(locked_meeting, format!("No data for meeting {MEETING_ID}."));
        assert_eq!(
            locked_search,
            "No transcript passages match \"navigation needle\"."
        );
        assert_eq!(
            locked_chapters,
            format!("No chapter map for meeting {MEETING_ID}.")
        );
        for masked in [&locked_meeting, &locked_search, &locked_chapters] {
            assert!(
                !masked.contains(SECRET) && !masked.contains("Navigation topic"),
                "sealed content crossed the production connection path: {masked}"
            );
        }

        // Mirror the final visibility transition of `unlock_folder`: the folder remains sealed on
        // disk but becomes readable for this process session.
        {
            let _lifecycle = state
                .lifecycle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state
                .unlocked_folders
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(FOLDER_ID.into());
        }
        let structured_system = "[5–8] Others: unique navigation needle";
        let expected_range = format!("offset 0..{}", structured_system.chars().count());
        let open_meeting = rpc_over_production_listener("get_meeting", get_args.clone());
        let open_search = rpc_over_production_listener("search_transcript", search_args.clone());
        let open_chapters =
            rpc_over_production_listener("get_meeting_chapters", chapters_args.clone());
        assert!(
            open_meeting.contains("format=structured, channel=system")
                && open_meeting.contains(structured_system),
            "channel-specific get_meeting did not traverse the full connection path: {open_meeting}"
        );
        assert!(
            open_search.contains("channel=system")
                && open_search.contains(SECRET)
                && open_search.contains(&expected_range),
            "search offset did not address the selected production MCP channel: {open_search}"
        );
        assert!(
            open_chapters.contains("channel=system")
                && open_chapters.contains("Navigation topic")
                && open_chapters.contains(&expected_range),
            "chapter offset did not address the selected production MCP channel: {open_chapters}"
        );

        {
            let _lifecycle = state
                .lifecycle
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.seal_epoch.fetch_add(1, Ordering::SeqCst);
            state
                .unlocked_folders
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(FOLDER_ID);
        }
        let relocked_search = rpc_over_production_listener("search_transcript", search_args);
        assert_eq!(
            relocked_search,
            "No transcript passages match \"navigation needle\"."
        );
        assert!(
            !relocked_search.contains(SECRET),
            "relocked transcript leaked through the production listener: {relocked_search}"
        );

        drop(state);
        let _ = std::fs::remove_file(db_path);
    }

    /// Flag OFF (the default): `search_semantic` DEGRADES to gated keyword (FTS/BM25) matching —
    /// no vector read ever runs — and the output is HONESTLY labelled as keyword matching, so the
    /// MCP client is never told a semantic search happened. Content stays reachable on the default
    /// install (the PR B write-only-memory fix).
    #[test]
    fn search_semantic_flag_off_degrades_to_labelled_keyword_match() {
        let (db, p) = temp_db();
        seed(&db, "m1", "Budget", "budget planning hiring quarter", None);
        // Tier 1 flipped the semantic default ON; this test covers the flag-OFF keyword-degradation
        // labelling, so pin the flag it asserts about explicitly (it used to rely on the old default).
        db.set_setting("semantic_search_enabled", "false").unwrap();
        let out = dispatch_tool(
            &db,
            "search_semantic",
            &json!({ "query": "budget" }),
            &HashSet::new(),
        )
        .unwrap();
        assert!(
            out.contains("semantic search is off"),
            "flag-off semantic tool must label its keyword degradation, got: {out}"
        );
        assert!(
            out.contains("Budget"),
            "flag-off fallback must still surface the gated keyword hit, got: {out}"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// Flag ON: both the real-model hybrid route and its model-unavailable keyword fallback apply
    /// the SAME visibility gate as `search_meetings`. Unit tests deliberately take the fallback so
    /// a developer's installed Metal model is never loaded; the DB-level test exercises the vector
    /// gate directly. A sealed-and-not-unlocked meeting is excluded and reappears after unlock.
    #[test]
    fn search_semantic_is_visibility_gated_when_enabled() {
        use crate::storage::models::Folder;
        let (db, p) = temp_db();
        // Enable the flag in the settings table.
        let cfg = crate::settings::AppConfig {
            semantic_search_enabled: true,
            ..Default::default()
        };
        cfg.save(&db).unwrap();

        db.insert_folder(&Folder {
            id: "f-lock".to_string(),
            name: "Secret".to_string(),
            path: "Secret".to_string(),
            parent_id: None,
            locked: false, // index while visible, then lock.
            created_at: "2026-06-26T00:00:00Z".to_string(),
        })
        .unwrap();
        seed(
            &db,
            "open",
            "Open",
            "budget planning hiring quarter apollo",
            None,
        );
        seed(
            &db,
            "sealed",
            "Sealed",
            "budget planning hiring quarter secret",
            Some("f-lock"),
        );

        // Seed deterministic vector rows directly. Holding an admitted active-model handle across
        // `dispatch_tool` would also hold the model-selection read barrier while AppConfig::load
        // republishes that selection, creating a same-thread read→write deadlock. MCP behavior is
        // what this test owns; real-model loading is covered by the embedder tests / Mac bake-off.
        let emb = crate::embed::StubEmbedder;
        db.index_meeting_chunks("open", &[], &emb).unwrap();
        db.index_meeting_chunks("sealed", &[], &emb).unwrap();
        // Seal the folder AFTER indexing (a stray vec row now exists for a sealed meeting).
        db.set_folder_locked("f-lock", true, None).unwrap();

        let args = json!({ "query": "budget planning hiring quarter" });

        // Not unlocked → sealed meeting must NOT appear.
        let out = dispatch_tool(&db, "search_semantic", &args, &HashSet::new()).unwrap();
        assert!(
            out.contains("semantic model is not installed"),
            "unit tests must stay on the bounded model-free fallback: {out}"
        );
        assert!(out.contains("id:open"), "open meeting must surface");
        assert!(
            !out.contains("id:sealed"),
            "sealed-not-unlocked meeting leaked through search_semantic (gate violation)"
        );

        // Session-unlock → sealed meeting reappears.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        let out2 = dispatch_tool(&db, "search_semantic", &args, &unlocked).unwrap();
        assert!(
            out2.contains("id:sealed"),
            "unlocked meeting must reappear in semantic results"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// Phase 5a: `get_open_commitments` is visibility-gated exactly like the other tools. A sealed-
    /// and-not-unlocked meeting's open action items NEVER appear; they reappear once the folder is
    /// session-unlocked. The payload renders owner · due · "text" · [[Title]].
    #[test]
    fn get_open_commitments_is_visibility_gated() {
        use crate::storage::models::Folder;
        let (db, p) = temp_db();
        db.insert_folder(&Folder {
            id: "f-lock".to_string(),
            name: "Secret".to_string(),
            path: "Secret".to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-06-26T00:00:00Z".to_string(),
        })
        .unwrap();
        seed(
            &db,
            "open",
            "Open Sync",
            "## Action items\n- [ ] Anna — ship the deck 2026-07-01\n- [x] Bob — already done\n",
            None,
        );
        seed(
            &db,
            "sealed",
            "Secret Sync",
            "## Action items\n- [ ] Carol — sign the contract 2026-07-05\n",
            Some("f-lock"),
        );
        db.set_folder_locked("f-lock", true, None).unwrap();

        // Not unlocked → only the open meeting's open item; sealed item invisible; done item dropped.
        let out = dispatch_tool(&db, "get_open_commitments", &json!({}), &HashSet::new()).unwrap();
        assert!(
            out.contains("ship the deck"),
            "open commitment must surface"
        );
        assert!(out.contains("[[Open Sync]]"), "source title must render");
        assert!(out.contains("due 2026-07-01"), "due date must render");
        assert!(out.contains("Anna"), "owner must render");
        assert!(
            !out.contains("already done"),
            "checked-off item must not be a commitment"
        );
        assert!(
            !out.contains("sign the contract") && !out.contains("Secret Sync"),
            "sealed-not-unlocked meeting's commitments leaked (gate violation)"
        );

        // Session-unlock → the sealed meeting's commitment reappears.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        let out2 = dispatch_tool(&db, "get_open_commitments", &json!({}), &unlocked).unwrap();
        assert!(
            out2.contains("sign the contract"),
            "unlocked commitment must reappear"
        );

        // Owner filter (case-insensitive).
        let out3 = dispatch_tool(
            &db,
            "get_open_commitments",
            &json!({ "owner": "anna" }),
            &unlocked,
        )
        .unwrap();
        assert!(
            out3.contains("ship the deck"),
            "owner filter must keep Anna's item"
        );
        assert!(
            !out3.contains("sign the contract"),
            "owner filter must drop Carol's item"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// Feature C: `query_database` is visibility-gated exactly like the other read tools (mirror of
    /// `get_open_commitments_is_visibility_gated`). A typed note in a sealed-and-not-unlocked
    /// note-folder is INVISIBLE to the query (its title + typed values never surface), and reappears
    /// once the folder is session-unlocked. RED if the tool bypasses `list_notes_visible_typed`.
    #[test]
    fn query_database_is_visibility_gated() {
        use crate::storage::models::{NoteFolder, PropertyKind, PropertySchemaField};
        let (db, p) = temp_db();
        // A LOCKED note-folder with a typed `status` schema and one typed note.
        db.insert_note_folder(
            &NoteFolder {
                id: "nf-lock".into(),
                name: "Secret Tasks".into(),
                path: "Notes/Secret Tasks".into(),
                parent_id: None,
                locked: false,
                unlocked: false,
                is_root: false,
                kind: "note".into(),
            },
            "2026-07-14T00:00:00Z",
        )
        .unwrap();
        db.set_note_folder_schema(
            "nf-lock",
            &[PropertySchemaField {
                key: "status".into(),
                kind: PropertyKind::Select,
                options: vec!["Open".into(), "Done".into()],
            }],
        )
        .unwrap();
        // Note title deliberately DISJOINT from the folder name so a substring test can't collide.
        db.insert_note(
            "n-secret",
            "nf-lock",
            "launch-plan",
            "Launch Plan",
            "---\nstatus: Open\n---\nbody",
            1_000,
        )
        .unwrap();
        db.set_folder_locked("nf-lock", true, Some(b"wrapped"))
            .unwrap();

        // Not unlocked → the sealed folder's typed row is invisible; the row's title never leaks.
        let args = json!({ "folder": "Secret Tasks", "filter": "status=Open" });
        let out = dispatch_tool(&db, "query_database", &args, &HashSet::new()).unwrap();
        assert!(
            !out.contains("Launch Plan"),
            "sealed-not-unlocked note-folder's typed row leaked (gate violation): {out}"
        );

        // Session-unlock → the typed row reappears in the query result.
        let mut unlocked = HashSet::new();
        unlocked.insert("nf-lock".to_string());
        let out2 = dispatch_tool(&db, "query_database", &args, &unlocked).unwrap();
        assert!(
            out2.contains("[[Launch Plan]]"),
            "unlocked typed row must reappear in query_database: {out2}"
        );

        // Missing required `folder` arg is an InvalidArg (JSON-RPC -32602), never a silent all-rows.
        let bad = dispatch_tool(
            &db,
            "query_database",
            &json!({ "filter": "x=y" }),
            &unlocked,
        );
        assert!(bad.is_err(), "query_database requires a folder argument");
        let _ = std::fs::remove_file(&p);
    }

    /// R2 folder discovery uses one gated catalog for listing, exact name/id resolution,
    /// alternatives, visible record count, and schema. A locked exact name/id must therefore be
    /// byte-identical to an absent lookup and leak none of those fields.
    #[test]
    fn note_folder_discovery_and_exact_lookup_share_one_visibility_gate() {
        use crate::storage::models::{NoteFolder, PropertyKind, PropertySchemaField};

        let (db, p) = temp_db();
        for (id, name, locked) in [
            ("nf-open", "Roadmap", false),
            ("nf-secret", "Secret Salaries", true),
        ] {
            db.insert_note_folder(
                &NoteFolder {
                    id: id.into(),
                    name: name.into(),
                    path: format!("Notes/{name}"),
                    parent_id: None,
                    locked,
                    unlocked: false,
                    is_root: false,
                    kind: "note".into(),
                },
                "2026-07-29T00:00:00Z",
            )
            .unwrap();
            db.set_note_folder_schema(
                id,
                &[PropertySchemaField {
                    key: if locked { "compensation" } else { "status" }.into(),
                    kind: PropertyKind::Select,
                    options: vec!["Open".into()],
                }],
            )
            .unwrap();
            db.insert_note(
                &format!("note-{id}"),
                id,
                &format!("slug-{id}"),
                if locked {
                    "Secret Pay Plan"
                } else {
                    "Public Plan"
                },
                "---\nstatus: Open\n---\nbody",
                1_000,
            )
            .unwrap();
        }

        let listed = dispatch_tool(&db, "list_note_folders", &json!({}), &HashSet::new()).unwrap();
        assert!(
            listed.contains("Roadmap")
                && listed.contains("id:nf-open")
                && listed.contains("visibleRecords:1")
                && listed.contains("typedColumns:status:select"),
            "visible folder metadata must be discoverable: {listed}"
        );
        assert!(
            !listed.contains("Secret")
                && !listed.contains("nf-secret")
                && !listed.contains("compensation"),
            "sealed folder name/id/count/schema leaked: {listed}"
        );

        let lookup = |folder: &str| {
            dispatch_tool(
                &db,
                "query_database",
                &json!({ "folder": folder, "filter": "" }),
                &HashSet::new(),
            )
            .unwrap()
        };
        let locked_name = lookup("Secret Salaries");
        let locked_id = lookup("nf-secret");
        let absent = lookup("definitely-absent");
        assert_eq!(locked_name, absent);
        assert_eq!(locked_id, absent);
        assert!(
            absent.contains("Available: Roadmap")
                && !absent.contains("Secret")
                && !absent.contains("nf-secret"),
            "only visible alternatives may appear: {absent}"
        );

        let mut unlocked = HashSet::new();
        unlocked.insert("nf-secret".to_string());
        let unlocked_list = dispatch_tool(&db, "list_note_folders", &json!({}), &unlocked).unwrap();
        assert!(
            unlocked_list.contains("Secret Salaries")
                && unlocked_list.contains("compensation:select"),
            "session-unlocked folder must become discoverable: {unlocked_list}"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// The hierarchy tool names a sealed container but discloses NOTHING inside it.
    ///
    /// A container's existence is not the secret — a lock the user cannot see is worse than one
    /// they can, and hiding the row entirely would make a locked project look deleted. What must
    /// not escape is what is INSIDE: the titles, and the counts that say how much there is.
    ///
    /// The tool reads the same gated assembly the sidebar does, which already empties a sealed
    /// container's type groups. This pins that dependency: if the reader ever started reporting
    /// totals for a sealed container, the sidebar would look no different and this is the only
    /// place that would notice.
    #[test]
    fn workspace_hierarchy_names_a_sealed_container_but_never_its_contents() {
        let p = crate::storage::db::unique_temp_path("murmur-mcp-hierarchy", "db");
        let db = Db::open_with_key(&p, TEST_DEK).unwrap();

        // Under the ADOPTED project, not beside it. A fresh database is never projectless — the
        // hierarchy migration adopts pre-existing folders into a default "Workspace" project —
        // and a folder with no parent is not part of the forest the tree renders. Getting this
        // wrong made the first run of this test assert against a vault it had not actually built.
        let project = db.workspace_project_id().unwrap().expect("a project is adopted");
        db.insert_folder(&crate::storage::models::Folder {
            id: "p-open".into(),
            name: "Acme".into(),
            path: "Acme".into(),
            parent_id: Some(project.clone()),
            locked: false,
            created_at: "2026-08-23T10:00:00Z".into(),
        })
        .unwrap();
        db.insert_folder(&crate::storage::models::Folder {
            id: "p-secret".into(),
            name: "Layoffs Q3".into(),
            path: "Layoffs Q3".into(),
            parent_id: Some(project),
            locked: true,
            created_at: "2026-08-23T10:00:00Z".into(),
        })
        .unwrap();

        // One meeting in each, so a leak would have something concrete to leak.
        for (mid, title, folder) in [
            ("m-open", "Standup", "p-open"),
            ("m-secret", "Severance list", "p-secret"),
        ] {
            db.insert_meeting(&crate::storage::models::Meeting {
                id: mid.into(),
                started_at: "2026-08-23T09:00:00Z".into(),
                ended_at: None,
                title: Some(title.into()),
                duration_s: 600,
                audio_path: None,
                status: crate::storage::models::MeetingStatus::Summarized,
                folder_id: None,
            })
            .unwrap();
            db.upsert_note(&crate::storage::models::NoteRecord {
                meeting_id: mid.into(),
                provider_id: "claude_code".into(),
                markdown: "body".into(),
                created_at: "2026-08-23T09:05:00Z".into(),
                exported_path: None,
                model_requested: None,
                model_served: None,
                gateway_host: None,
            })
            .unwrap();
            db.set_meeting_folder(mid, Some(folder)).unwrap();
        }

        let listed =
            dispatch_tool(&db, "list_workspace_hierarchy", &json!({}), &HashSet::new()).unwrap();

        // The open container reports itself AND what it holds.
        assert!(
            listed.contains("Acme") && listed.contains("id:p-open"),
            "a visible container must be discoverable: {listed}"
        );
        assert!(
            listed.contains("meeting:1"),
            "an open container must report its counts: {listed}"
        );

        // The sealed one reports itself, and that it is locked, and nothing else.
        assert!(
            listed.contains("Layoffs Q3") && listed.contains("locked"),
            "a sealed container must still be visible as locked: {listed}"
        );
        assert!(
            !listed.contains("Severance list"),
            "a sealed container disclosed a title: {listed}"
        );

        // The COUNT is the subtle half: "meeting:1" beside a locked project tells a reader there
        // is exactly one meeting in there, which is a fact about sealed content.
        let sealed_line = listed
            .lines()
            .find(|line| line.contains("Layoffs Q3"))
            .expect("the sealed container must appear");
        assert!(
            sealed_line.contains("empty"),
            "a sealed container disclosed how much it holds: {sealed_line}"
        );

        let _ = std::fs::remove_file(&p);
    }

    /// Entity discovery and suggestions must come only from `list_entities_visible`. Suggestions
    /// recover prefix/substring/initialism/typo misses without weakening the exact resolver itself.
    #[test]
    fn entity_discovery_and_did_you_mean_exclude_sealed_entities() {
        use crate::storage::models::{EntityKind, Folder};

        let (db, p) = temp_db();
        db.insert_folder(&Folder {
            id: "f-secret".into(),
            name: "Private".into(),
            path: "Private".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-29T00:00:00Z".into(),
        })
        .unwrap();
        seed(&db, "m-open", "Visible", "KO project", None);
        seed(
            &db,
            "m-secret",
            "Secret",
            "Classified Phoenix",
            Some("f-secret"),
        );
        let ko = db.upsert_entity("KO", EntityKind::Project).unwrap();
        let connect = db.upsert_entity("Connect", EntityKind::Project).unwrap();
        let phoenix = db.upsert_entity("Phoenix", EntityKind::Project).unwrap();
        db.add_mention(&ko, "m-open").unwrap();
        db.add_mention(&connect, "m-open").unwrap();
        for idx in 0..6 {
            let candidate = db
                .upsert_entity(&format!("Concord {idx}"), EntityKind::Project)
                .unwrap();
            db.add_mention(&candidate, "m-open").unwrap();
        }
        db.add_mention(&phoenix, "m-secret").unwrap();
        db.set_folder_locked("f-secret", true, None).unwrap();

        let listed = dispatch_tool(&db, "list_entities", &json!({}), &HashSet::new()).unwrap();
        assert!(
            listed.contains("KO")
                && listed.contains("type:project")
                && listed.contains("visibleMentions:1")
        );
        assert!(
            !listed.contains("Phoenix"),
            "sealed-only entity leaked through discovery: {listed}"
        );

        let miss = dispatch_tool(
            &db,
            "get_entity_dossier",
            &json!({ "entity": "Kong Operator", "noteDetail": "none" }),
            &HashSet::new(),
        )
        .unwrap();
        assert!(
            miss.starts_with("No visible entity matching")
                && miss.contains("Did you mean: KO?")
                && !miss.contains("DOSSIER"),
            "initialism must suggest, not weaken exact resolution: {miss}"
        );
        let diff_miss = dispatch_tool(
            &db,
            "knowledge_diff",
            &json!({
                "entity": "Kong Operator",
                "from": "2026-07-01T00:00:00Z",
                "to": "2026-07-29T00:00:00Z"
            }),
            &HashSet::new(),
        )
        .unwrap();
        assert!(
            diff_miss.contains("Did you mean: KO?"),
            "knowledge_diff must use the same suggestion path: {diff_miss}"
        );
        let typo = dispatch_tool(
            &db,
            "get_entity_dossier",
            &json!({ "entity": "Conect", "noteDetail": "none" }),
            &HashSet::new(),
        )
        .unwrap();
        assert!(
            typo.contains("Did you mean: Connect?"),
            "edit distance <=2 should recover a typo: {typo}"
        );
        let bounded = dispatch_tool(
            &db,
            "get_entity_dossier",
            &json!({ "entity": "Conc", "noteDetail": "none" }),
            &HashSet::new(),
        )
        .unwrap();
        let suggestions = bounded
            .split_once("Did you mean: ")
            .map(|(_, names)| names.trim_end_matches('?'))
            .expect("prefix suggestions");
        assert_eq!(
            suggestions.split(", ").count(),
            5,
            "suggestions must be capped at five: {bounded}"
        );
        let sealed_miss = dispatch_tool(
            &db,
            "get_entity_dossier",
            &json!({ "entity": "Phoenx", "noteDetail": "none" }),
            &HashSet::new(),
        )
        .unwrap();
        assert!(
            !sealed_miss.contains("Phoenix"),
            "typo suggestions became a sealed-name oracle: {sealed_miss}"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn entity_discovery_defaults_to_forty_and_clamps_to_one_hundred() {
        use crate::storage::models::EntityKind;

        let (db, p) = temp_db();
        seed(&db, "m-catalog", "Catalog", "catalog body", None);
        for idx in 0..105 {
            let id = db
                .upsert_entity(&format!("Entity {idx:03}"), EntityKind::Project)
                .unwrap();
            db.add_mention(&id, "m-catalog").unwrap();
        }
        let listed =
            |args: Value| dispatch_tool(&db, "list_entities", &args, &HashSet::new()).unwrap();
        assert_eq!(listed(json!({})).lines().count(), 40);
        assert_eq!(listed(json!({ "limit": 999 })).lines().count(), 100);
        assert_eq!(listed(json!({ "limit": 0 })).lines().count(), 1);
        let filtered = listed(json!({ "query": "entity 042", "limit": 100 }));
        assert_eq!(filtered.lines().count(), 1);
        assert!(filtered.contains("Entity 042"));
        let _ = std::fs::remove_file(&p);
    }

    /// Meeting triage is one visibility-gated aggregate. Error detail is deterministic from the
    /// visible transcript size, and no sealed row contributes any metadata.
    #[test]
    fn recent_meeting_triage_reports_sizes_and_error_detail_without_sealed_rows() {
        use crate::storage::models::{Folder, MeetingStatus};
        use crate::transcribe::types::Segment;

        let (db, p) = temp_db();
        db.insert_folder(&Folder {
            id: "f-secret".into(),
            name: "Private".into(),
            path: "Private".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-29T00:00:00Z".into(),
        })
        .unwrap();
        seed(&db, "m-empty", "Broken empty", "visible note", None);
        seed(&db, "m-partial", "Broken partial", "visible note", None);
        seed(
            &db,
            "m-secret",
            "Secret broken",
            "sealed note",
            Some("f-secret"),
        );
        for id in ["m-empty", "m-partial", "m-secret"] {
            db.update_meeting_status(id, MeetingStatus::Error).unwrap();
        }
        for id in ["m-partial", "m-secret"] {
            db.insert_segments(
                id,
                &[Segment {
                    idx: 0,
                    start_s: 0.0,
                    end_s: 1.0,
                    text: "partial words".into(),
                    speaker: Some("others".into()),
                    confidence: None,
                }],
            )
            .unwrap();
        }
        db.set_folder_locked("f-secret", true, None).unwrap();

        let out = dispatch_tool(
            &db,
            "list_recent_meetings",
            &json!({ "limit": 20 }),
            &HashSet::new(),
        )
        .unwrap();
        let empty = out
            .lines()
            .find(|line| line.contains("Broken empty"))
            .expect("empty error row");
        assert!(
            empty.contains("status:Error")
                && empty.contains("statusDetail:no transcript")
                && empty.contains("durationSeconds:60")
                && empty.contains("transcriptChars:0")
                && empty.contains("hasVisibleNote:true"),
            "empty error triage fields: {empty}"
        );
        let partial = out
            .lines()
            .find(|line| line.contains("Broken partial"))
            .expect("partial error row");
        assert!(
            partial.contains("statusDetail:partial transcript")
                && partial.contains("transcriptChars:13"),
            "partial error triage fields: {partial}"
        );
        assert!(
            !out.contains("Secret broken") && !out.contains("m-secret"),
            "sealed meeting metadata leaked through triage: {out}"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// Phase 5b: `get_entity_dossier` is visibility-gated AND egress-free. A sealed-and-not-unlocked
    /// mentioning meeting contributes nothing to the dossier payload (no title, no note body), and
    /// reappears once the folder is session-unlocked. The dispatch builds GATED STRUCTURED DATA only
    /// — it never constructs a provider or makes a cloud call (the whole `dispatch_tool` path has no
    /// `make_provider`/`complete`), so the MCP server stays read-only + egress-free.
    #[test]
    fn get_entity_dossier_is_visibility_gated_and_egress_free() {
        use crate::storage::models::{EntityKind, Folder};
        let (db, p) = temp_db();
        db.insert_folder(&Folder {
            id: "f-lock".to_string(),
            name: "Secret".to_string(),
            path: "Secret".to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-06-26T00:00:00Z".to_string(),
        })
        .unwrap();
        seed(
            &db,
            "open",
            "Kickoff",
            "## Action items\n- [ ] Anna — draft Atlas spec 2026-07-01\n",
            None,
        );
        seed(
            &db,
            "sealed",
            "Secret Atlas Review",
            "LOCKED Atlas acquisition price\n## Action items\n- [ ] Carol — sign 2026-07-09\n",
            Some("f-lock"),
        );
        let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
        db.add_mention(&atlas, "open").unwrap();
        db.add_mention(&atlas, "sealed").unwrap();
        db.set_folder_locked("f-lock", true, None).unwrap();

        // Not unlocked → the dossier resolves Atlas by NAME, includes the open meeting [[Title]]
        // and its commitment, and EXCLUDES the sealed meeting's title, note body, and commitment.
        let args = json!({ "entity": "Atlas" });
        let out = dispatch_tool(&db, "get_entity_dossier", &args, &HashSet::new()).unwrap();
        assert!(
            out.contains("DOSSIER for [[Atlas]]"),
            "overview header must render"
        );
        assert!(out.contains("[[Kickoff]]"), "visible meeting must be cited");
        assert!(
            out.contains("draft Atlas spec"),
            "visible open commitment must surface"
        );
        assert!(
            !out.contains("Secret Atlas Review") && !out.contains("LOCKED Atlas acquisition"),
            "sealed-not-unlocked meeting leaked into the dossier (gate violation)"
        );
        assert!(
            !out.contains("sign"),
            "sealed commitment leaked into the dossier"
        );

        // Session-unlock → the sealed meeting + its content reappear.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        let out2 = dispatch_tool(&db, "get_entity_dossier", &args, &unlocked).unwrap();
        assert!(
            out2.contains("[[Secret Atlas Review]]"),
            "unlocked meeting must reappear"
        );
        assert!(
            out2.contains("LOCKED Atlas acquisition"),
            "unlocked content must reappear"
        );

        // Unknown entity → a friendly, non-leaking message (never an error).
        let none = dispatch_tool(
            &db,
            "get_entity_dossier",
            &json!({ "entity": "Nonexistent" }),
            &HashSet::new(),
        )
        .unwrap();
        assert!(
            none.contains("No visible entity"),
            "unknown entity → friendly message"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// Brain v3 PR-6 (RED-before-GREEN gate): the MCP `knowledge_diff` dispatch is visibility-gated.
    /// A fact whose SOURCE meeting is in a sealed-and-not-session-unlocked folder must be ABSENT from
    /// the diff AND the decision ledger — its object never renders — and reappear once the folder is
    /// session-unlocked. Routes through the SAME gated reader (`list_facts_visible`) as the dossier.
    /// EGRESS-FREE: `dispatch_tool` builds gated structured text only, never a provider/cloud call.
    #[test]
    fn mcp_knowledge_diff_is_visibility_gated() {
        use crate::facts::{FactOp, NewFact};
        use crate::storage::models::{EntityKind, Folder};
        let (db, p) = temp_db();
        db.insert_folder(&Folder {
            id: "f-lock".to_string(),
            name: "Secret".to_string(),
            path: "Secret".to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-06-26T00:00:00Z".to_string(),
        })
        .unwrap();
        // OPEN meetings carry a supersession (Atlas.status in-progress → shipped) the ledger surfaces.
        seed(&db, "m_open1", "Kickoff", "Atlas kickoff\n", None);
        seed(&db, "m_open2", "Ship Review", "Atlas shipped\n", None);
        // SEALED meeting carries a fact whose OBJECT must never leak while the folder is sealed.
        seed(
            &db,
            "m_sealed",
            "Secret Atlas Review",
            "Atlas secret budget\n",
            Some("f-lock"),
        );
        let atlas = db.upsert_entity("Atlas", EntityKind::Project).unwrap();
        db.add_mention(&atlas, "m_open1").unwrap();
        db.add_mention(&atlas, "m_open2").unwrap();
        db.add_mention(&atlas, "m_sealed").unwrap();

        let add = |predicate: &str, object: &str, vf: &str, meeting: &str| {
            FactOp::Add(NewFact {
                entity_id: atlas.clone(),
                subject: "Atlas".to_string(),
                predicate: predicate.to_string(),
                object: object.to_string(),
                valid_from: vf.to_string(),
                recorded_at: vf.to_string(),
                confidence: 1.0,
                meeting_id: Some(meeting.to_string()),
            })
        };
        // Seed Atlas.status = in-progress (open) on m_open1.
        db.apply_fact_ops(&[add(
            "status",
            "in-progress",
            "2026-06-01T00:00:00Z",
            "m_open1",
        )])
        .unwrap();
        // The reconcile-style change: close in-progress @2026-06-20 (Invalidate the minted row) and
        // open shipped on m_open2 — a real supersession, built exactly like `apply_fact_ops` does.
        let ip_id = db
            .facts_for_entities(std::slice::from_ref(&atlas))
            .unwrap()
            .into_iter()
            .find(|f| f.object == "in-progress")
            .expect("in-progress row exists")
            .id;
        db.apply_fact_ops(&[
            FactOp::Invalidate {
                id: ip_id,
                valid_to: "2026-06-20T00:00:00Z".to_string(),
            },
            add("status", "shipped", "2026-06-20T00:00:00Z", "m_open2"),
        ])
        .unwrap();
        // The SEALED-source fact whose object must never leak while the folder is sealed.
        db.apply_fact_ops(&[add(
            "budget",
            "SECRET-42M",
            "2026-06-15T00:00:00Z",
            "m_sealed",
        )])
        .unwrap();

        db.set_folder_locked("f-lock", true, None).unwrap();

        let args = json!({
            "entity": "Atlas",
            "from": "2026-06-10T00:00:00Z",
            "to": "2026-06-25T00:00:00Z"
        });

        // NOT unlocked: the open supersession (in-progress → shipped) renders; the SEALED fact's
        // object (SECRET-42M) and predicate (budget) never appear anywhere in the payload.
        let out = dispatch_tool(&db, "knowledge_diff", &args, &HashSet::new()).unwrap();
        assert!(
            out.contains("in-progress") && out.contains("shipped"),
            "the open-source supersession must render: {out}"
        );
        assert!(
            !out.contains("SECRET-42M") && !out.contains("budget"),
            "a sealed-not-unlocked meeting's fact leaked into the knowledge diff (gate violation): {out}"
        );

        // Session-unlock the folder → the sealed fact (budget = SECRET-42M) reappears.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        let out2 = dispatch_tool(&db, "knowledge_diff", &args, &unlocked).unwrap();
        assert!(
            out2.contains("SECRET-42M"),
            "unlocked sealed fact must reappear in the diff: {out2}"
        );

        // Missing required args are InvalidArg (-32602), never a silent all-facts read.
        assert!(dispatch_tool(
            &db,
            "knowledge_diff",
            &json!({ "from": "x", "to": "y" }),
            &unlocked
        )
        .is_err());
        assert!(dispatch_tool(
            &db,
            "knowledge_diff",
            &json!({ "entity": "Atlas", "to": "y" }),
            &unlocked
        )
        .is_err());
        assert!(dispatch_tool(
            &db,
            "knowledge_diff",
            &json!({ "entity": "Atlas", "from": "x" }),
            &unlocked
        )
        .is_err());

        // Unknown entity → friendly non-leaking message (never an error). Uses VALID RFC3339
        // bounds so it reaches the entity-resolution path (B2 rejects malformed timestamps at the
        // dispatch boundary before the entity is ever resolved).
        let none = dispatch_tool(
            &db,
            "knowledge_diff",
            &json!({ "entity": "Nonexistent", "from": "2026-06-10T00:00:00Z", "to": "2026-06-25T00:00:00Z" }),
            &HashSet::new(),
        )
        .unwrap();
        assert!(
            none.contains("No visible entity"),
            "unknown entity → friendly message"
        );

        let _ = std::fs::remove_file(&p);
    }

    /// B2 (RED-before-GREEN) — the MCP `knowledge_diff` dispatch validates that BOTH `from` and `to`
    /// parse as RFC3339. An unparseable `from` used to pass through (`normalize_instant` returns it
    /// UNCHANGED, `cmp_instant` compares lexically), SWAP the range, and yield a confident but wrong
    /// "0 changes" with NO error. Now it is a -32602 that names the offending argument; a well-formed
    /// pair proceeds past validation. RED on the pre-fix code (which returned Ok with an empty window).
    #[test]
    fn mcp_knowledge_diff_rejects_unparseable_timestamp() {
        let (db, p) = temp_db();

        // A garbage `from` (valid `to`) → -32602 naming `from`, never a silent "0 changes".
        let err = dispatch_tool(
            &db,
            "knowledge_diff",
            &json!({ "entity": "Atlas", "from": "not-a-date", "to": "2026-06-25T00:00:00Z" }),
            &HashSet::new(),
        )
        .unwrap_err();
        assert_eq!(
            err.0, -32602,
            "malformed timestamp must be InvalidArg: {err:?}"
        );
        assert!(
            err.1.contains("from"),
            "the error must name the offending argument (from): {}",
            err.1
        );

        // Well-formed RFC3339 bounds do NOT error at the validation step — dispatch proceeds to
        // `execute_tool` (an unknown entity there is a friendly Ok message, not a validation error).
        let ok = dispatch_tool(
            &db,
            "knowledge_diff",
            &json!({ "entity": "Atlas", "from": "2026-06-10T00:00:00Z", "to": "2026-06-25T00:00:00Z" }),
            &HashSet::new(),
        );
        assert!(
            ok.is_ok(),
            "valid RFC3339 bounds must pass the dispatch validation: {ok:?}"
        );

        let _ = std::fs::remove_file(&p);
    }

    /// Feature D: the MCP `get_document` dispatch is visibility-gated — a document in a
    /// sealed-and-not-session-unlocked folder returns the masked "No data" sentinel, and reappears
    /// once the folder is session-unlocked. Mirrors the other MCP gate tests but through the real
    /// `dispatch_tool` (JSON args → `ToolCall::GetDocument` → gated `execute_tool`).
    #[test]
    fn mcp_get_document_is_visibility_gated() {
        use crate::storage::models::Folder;
        let (db, p) = temp_db();
        db.insert_folder(&Folder {
            id: "f-lock".to_string(),
            name: "Secret".to_string(),
            path: "Secret".to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-06-26T00:00:00Z".to_string(),
        })
        .unwrap();
        db.insert_note(
            "note-1",
            "f-lock",
            "note-name",
            "Secret Note",
            "the classified body text",
            1_700_000_000,
        )
        .unwrap();
        db.set_folder_locked("f-lock", true, None).unwrap();

        let args = json!({ "documentId": "note-1" });
        // Locked, not unlocked → masked sentinel, no body/title.
        let out = dispatch_tool(&db, "get_document", &args, &HashSet::new()).unwrap();
        assert_eq!(out, "No data for document note-1.");
        assert!(
            !out.contains("classified"),
            "sealed document body leaked via MCP get_document"
        );

        // Session-unlock → body reappears.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        let out2 = dispatch_tool(&db, "get_document", &args, &unlocked).unwrap();
        assert!(
            out2.contains("the classified body text"),
            "unlocked body must reappear: {out2}"
        );
        assert!(
            out2.contains("TITLE: [[Secret Note]]"),
            "title must render: {out2}"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// Brain v3 audit Fix 2 — the MCP `get_document` DEFAULT (no paging args) no longer floods the
    /// client with the whole body: it returns a BOUNDED window (`MCP_DEFAULT_WINDOW_CHARS`) plus a
    /// `TOTAL_CHARS: …` disclosure so the client can see the true length and page the rest. An
    /// explicit larger `maxChars` is honored verbatim. RED on the pre-fix `(0,0)`-returns-everything.
    #[test]
    fn mcp_get_document_default_window_is_bounded_and_disclosed() {
        use crate::storage::models::Folder;
        let (db, p) = temp_db();
        db.insert_folder(&Folder {
            id: "f-open".to_string(),
            name: "Docs".to_string(),
            path: "Docs".to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-06-26T00:00:00Z".to_string(),
        })
        .unwrap();
        // A body LARGER than the default window so the flood is measurable.
        let big = "A".repeat(MCP_DEFAULT_WINDOW_CHARS + 5000);
        db.insert_document(
            "bigdoc",
            "f-open",
            "big.md",
            &big,
            "document",
            1_700_000_000,
        )
        .unwrap();

        // Default (no paging) → bounded to MCP_DEFAULT_WINDOW_CHARS + a disclosure header showing
        // the TRUE total; NOT the whole 11000-char body.
        let out = dispatch_tool(
            &db,
            "get_document",
            &json!({ "documentId": "bigdoc" }),
            &HashSet::new(),
        )
        .unwrap();
        assert!(
            out.contains(&format!(
                "BODY (TOTAL_CHARS: {} (showing 0..{}))",
                big.chars().count(),
                MCP_DEFAULT_WINDOW_CHARS
            )),
            "the MCP default must disclose the true total + the bounded window: {}",
            &out[..out.len().min(120)]
        );
        // The returned body is bounded (window + headers/title), NOT the full 11000-char body.
        assert!(
            out.len() < big.len(),
            "the default MCP window must NOT return the whole body (flood): got {} vs body {}",
            out.len(),
            big.len()
        );
        assert!(
            !out.contains("[end of content]"),
            "the bounded default window does NOT reach the end of a body larger than the window: {}",
            &out[out.len().saturating_sub(60)..]
        );

        // Explicit larger maxChars is honored (the client CAN ask for more).
        let full = dispatch_tool(
            &db,
            "get_document",
            &json!({ "documentId": "bigdoc", "offset": 0, "maxChars": big.chars().count() + 10 }),
            &HashSet::new(),
        )
        .unwrap();
        assert!(
            full.contains("[end of content]"),
            "an explicit full window reaches the end: last 60 = {}",
            &full[full.len().saturating_sub(60)..]
        );
        assert!(
            full.len() > out.len(),
            "explicit large maxChars returns more than the default window"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// Brain v3 audit Fix 3(b) — the MCP `get_document_outline` dispatch is visibility-gated: a
    /// document in a sealed-and-not-session-unlocked folder returns the "no outline" sentinel (no
    /// heading leak), and the heading map reappears once the folder is session-unlocked. Proves the
    /// NEW gated read routes through `execute_tool` → `get_document_outline_if_visible`.
    #[test]
    fn mcp_get_document_outline_is_visibility_gated() {
        use crate::storage::models::Folder;
        let (db, p) = temp_db();
        db.insert_folder(&Folder {
            id: "f-lock".to_string(),
            name: "Specs".to_string(),
            path: "Specs".to_string(),
            parent_id: None,
            locked: false,
            created_at: "2026-06-26T00:00:00Z".to_string(),
        })
        .unwrap();
        // A two-section doc → two L1 headings in the outline.
        let blocks = vec![
            crate::extract::ExtractedBlock {
                text: "Confidential design of the vault store.".to_string(),
                page: Some(1),
                heading_path: Some("SecretDesign".to_string()),
            },
            crate::extract::ExtractedBlock {
                text: "The keys are wrapped by the master KEK.".to_string(),
                page: Some(2),
                heading_path: Some("SecretDesign › Keys".to_string()),
            },
        ];
        let stored = crate::extract::blocks_to_stored_text(&blocks);
        db.insert_document(
            "od1",
            "f-lock",
            "spec.pdf",
            &stored,
            "document",
            1_700_000_000,
        )
        .unwrap();
        db.index_document_chunks("od1", None).unwrap();
        db.set_folder_locked("f-lock", true, None).unwrap();

        let args = json!({ "documentId": "od1" });
        // Locked, not unlocked → the "no outline" sentinel; the heading trail must NOT leak.
        let out = dispatch_tool(&db, "get_document_outline", &args, &HashSet::new()).unwrap();
        assert!(
            out.contains("No outline for that document"),
            "sealed → sentinel: {out}"
        );
        assert!(
            !out.contains("SecretDesign"),
            "sealed document headings leaked via MCP outline: {out}"
        );

        // Session-unlock → the heading map reappears in document order.
        let mut unlocked = HashSet::new();
        unlocked.insert("f-lock".to_string());
        let out2 = dispatch_tool(&db, "get_document_outline", &args, &unlocked).unwrap();
        assert!(
            out2.contains("SecretDesign (p.1)"),
            "unlocked outline lists section + page: {out2}"
        );
        assert!(
            out2.contains("SecretDesign › Keys (p.2)"),
            "document order preserved: {out2}"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// A visible meeting id redirects to `get_meeting`, but a locked meeting id remains literally
    /// byte-identical to an absent id. The visibility check must run before the raw existence read.
    #[test]
    fn document_outline_redirects_only_for_visible_meeting_ids() {
        use crate::storage::models::Folder;

        let (db, p) = temp_db();
        db.insert_folder(&Folder {
            id: "f-secret".into(),
            name: "Private".into(),
            path: "Private".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-29T00:00:00Z".into(),
        })
        .unwrap();
        seed(&db, "m-visible", "Visible meeting", "note", None);
        seed(
            &db,
            "m-secret",
            "Secret meeting",
            "secret note",
            Some("f-secret"),
        );
        db.set_folder_locked("f-secret", true, None).unwrap();

        let outline = |id: &str| {
            dispatch_tool(
                &db,
                "get_document_outline",
                &json!({ "documentId": id }),
                &HashSet::new(),
            )
            .unwrap()
        };
        let visible = outline("m-visible");
        assert!(
            visible.contains("m-visible is a MEETING") && visible.contains("get_meeting"),
            "visible meeting should redirect: {visible}"
        );
        let locked = outline("m-secret");
        let absent = outline("absent-id");
        assert_eq!(
            locked, absent,
            "locked and absent ids require a byte-identical sentinel"
        );
        assert!(
            !locked.contains("m-secret")
                && !locked.contains("Secret meeting")
                && !locked.contains("MEETING"),
            "locked meeting existence leaked: {locked}"
        );
        let _ = std::fs::remove_file(&p);
    }

    /// Feature D: the MCP `get_meeting` dispatch defaults to the STRUCTURED transcript, and honors an
    /// explicit `transcriptFormat: "plain"` for the legacy flat text.
    #[test]
    fn mcp_get_meeting_transcript_format_switches() {
        use crate::storage::models::Meeting;
        use crate::transcribe::types::Segment;
        let (db, p) = temp_db();
        // Titleless meeting → no TITLE prefix; a note so get_meeting returns content.
        db.insert_meeting(&Meeting {
            id: "mm".to_string(),
            started_at: "2026-06-27T09:00:00Z".to_string(),
            ended_at: None,
            title: None,
            duration_s: 60,
            audio_path: None,
            status: crate::storage::models::MeetingStatus::Summarized,
            folder_id: None,
        })
        .unwrap();
        db.upsert_note(&crate::storage::models::NoteRecord {
            meeting_id: "mm".to_string(),
            provider_id: "claude_code".to_string(),
            markdown: "n".to_string(),
            created_at: "2026-06-27T09:05:00Z".to_string(),
            exported_path: None,
            model_requested: None,
            model_served: None,
            gateway_host: None,
        })
        .unwrap();
        db.insert_segments(
            "mm",
            &[Segment {
                idx: 0,
                start_s: 5.0,
                end_s: 8.0,
                text: "opening remarks".into(),
                speaker: Some("me".into()),
                confidence: None,
            }],
        )
        .unwrap();

        // Default (no transcriptFormat) → STRUCTURED (speaker label + timestamp token). Audit
        // Fix 2: the MCP default now bounds+discloses the transcript window, so the section header
        // carries a `TOTAL_CHARS: …` disclosure (the whole short transcript fits the 6000 window).
        let def = dispatch_tool(
            &db,
            "get_meeting",
            &json!({ "meetingId": "mm" }),
            &HashSet::new(),
        )
        .unwrap();
        assert!(
            def.contains("Me: opening remarks"),
            "default must be structured: {def}"
        );
        assert!(
            def.contains("[5–8]"),
            "default must carry a timestamp token: {def}"
        );
        assert!(
            def.contains("TRANSCRIPT (format=structured, channel=merged, TOTAL_CHARS:"),
            "the MCP default now discloses the transcript window total: {def}"
        );

        // Explicit plain → the legacy flat text (no speaker label, no timestamp). Audit Fix 2: with
        // NO paging args the MCP default now applies the bounded+disclosed window, so the short
        // transcript is fully returned WITH its `TOTAL_CHARS` header + the end-of-content marker.
        let plain = dispatch_tool(
            &db,
            "get_meeting",
            &json!({ "meetingId": "mm", "transcriptFormat": "plain" }),
            &HashSet::new(),
        )
        .unwrap();
        assert_eq!(
            plain,
            "NOTE (TOTAL_CHARS: 1 (showing 0..1)):\nn\n[end of content]\n\nTRANSCRIPT (format=plain, channel=merged, TOTAL_CHARS: 15 (showing 0..15)):\nopening remarks\n[end of content]"
        );
        let invalid = dispatch_tool(
            &db,
            "get_meeting",
            &json!({ "meetingId": "mm", "channel": "invented" }),
            &HashSet::new(),
        )
        .unwrap_err();
        assert_eq!(
            invalid.0, -32602,
            "invalid channel is a transport arg error"
        );
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn mcp_transcript_search_empty_queries_are_successful_zero_hits() {
        let (db, p) = temp_db();
        seed(
            &db,
            "m-empty-query",
            "Must stay hidden",
            "must stay hidden too",
            None,
        );
        for query in ["", " \t\n "] {
            let out = dispatch_tool(
                &db,
                "search_transcript",
                &json!({ "query": query, "meetingId": "m-empty-query" }),
                &HashSet::new(),
            )
            .expect("MCP empty query must not become a JSON-RPC argument error");
            assert_eq!(out, "No transcript passages match \"\".");
            assert!(
                !out.contains("m-empty-query") && !out.contains("Must stay hidden"),
                "scoped empty query disclosed meeting existence: {out}"
            );
        }
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn origin_allow_list_rejects_cross_origin_and_null() {
        // E5: loopback origins allowed; cross-origin and the opaque "null" origin rejected.
        assert!(origin_allowed("http://127.0.0.1:8765"));
        assert!(origin_allowed("http://localhost:8765"));
        assert!(origin_allowed("http://127.0.0.1"));
        assert!(origin_allowed("http://localhost"));
        for bad in [
            "null",
            "https://evil.example.com",
            "http://evil.example.com",
            "https://127.0.0.1:8765", // wrong scheme
            "http://127.0.0.1:9999",  // wrong port
        ] {
            assert!(!origin_allowed(bad), "{bad} must be rejected");
        }
    }
}
