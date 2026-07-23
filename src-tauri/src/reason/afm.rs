//! WS2 — Tier 2: the Apple Foundation Models (AFM) on-device reasoner seam (EXPERIMENTAL).
//!
//! [`AfmReasoner`] implements [`LocalReasoner`] by shelling out to a bundled `meetnotes-afm` Swift
//! sidecar over stdio (mirroring the `meetnotes-calendar` / `meetnotes-sysaudio` pattern): the Rust
//! side sends a `{mode, system, user, schema}` JSON request on the child's STDIN and reads a
//! validated `{status, text?, json?, reason?, error?}` envelope from STDOUT. On macOS 26+ Apple
//! Silicon the sidecar drives `SystemLanguageModel.default` (the ~3B on-device model) for a
//! zero-download, zero-egress, Polish-capable, guaranteed-structured reasoner.
//!
//! ## Status: the native sidecar is DEFERRED to a signed macOS-26 Mac
//! This machine is CLT-only (no macOS 26 SDK), so `afm/afm.swift` is intentionally NOT written and
//! NOT bundled — [`build.rs`] early-returns when the source is absent, and there is no
//! `tauri.conf.json` `bundle.resources` entry (tauri_build validates the binary exists). Everything
//! HERE is headless-green NOW: sidecar resolution, the pure `{build,parse}_afm_request/response`
//! contract, the bounded/hardened spawn, availability probing, and — critically — GRACEFUL
//! DEGRADATION: with the sidecar ABSENT (every current machine) [`afm_reasoner`] returns the
//! deterministic [`StubReasoner`], byte-identical to `BrainBackend::Off`. It NEVER panics/aborts.
//!
//! ## Privacy posture (audited by the lock-security reviewer)
//! AFM is classified ON-DEVICE like the `Local` GGUF reasoner: it is intentionally NOT behind the
//! `cloud_egress_consented` gate and NOT `RedactingProvider`-wrapped — the transcript excerpt stays
//! on the Mac. [`AfmReasoner`] holds no HTTP client and opens no network path; the request rides the
//! child's STDIN PIPE only (never a temp file), and the child is spawned with `env_clear()` + a
//! fixed `PATH` so no DEK/KEK/token/`MURMUR_DEV_*` can leak into it. The zero-egress claim ultimately
//! rests on the native sidecar pinning `SystemLanguageModel.default` (NOT Apple Private Cloud
//! Compute) — that MUST be verified on-Mac with a network capture before the experimental flag comes
//! off (see LOCKSEC in the spec). Logs carry `target: "reason"` + stages/errors only, never content.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::error::{AppError, Result};
use crate::settings::AppConfig;

use super::{parse_first_json, LocalReasoner, StubReasoner};

/// Filename of the sidecar — both inside `Contents/Resources` of a shipped `.app` and at the dev
/// `OUT_DIR`.
const SIDECAR_NAME: &str = "meetnotes-afm";

/// DEV/TEST-ONLY runtime override: an absolute path to a `meetnotes-afm`-compatible executable. NOT a
/// secret (just a path) — it lets the headless spawn round-trip point at a fixture script on any OS,
/// and lets a developer swap the sidecar without a rebuild. Checked FIRST in [`sidecar_path`], but
/// ONLY under `test`/`debug_assertions` — a signed RELEASE never reads it (mirrors the
/// `MURMUR_DEV_DEK`/`MURMUR_DEV_KEK` precedent), so a release build can't be pointed at a
/// bring-your-own sidecar via the process env.
#[cfg(any(test, debug_assertions))]
const ENV_OVERRIDE: &str = "MURMUR_AFM_SIDECAR";

/// Hard wall-clock cap on a sidecar invocation from the Rust side. A forward pass is slower than the
/// calendar lookup (10s), so this is generous; a wedged child is hard-killed at the deadline so it
/// can never block the reasoner call.
const SIDECAR_TIMEOUT: Duration = Duration::from_secs(20);

/// A SIGKILL should reap promptly, but the host must never replace one unbounded `wait()` with
/// another. Teardown gets its own small deadline and every kill/reap error is surfaced.
const SIDECAR_REAP_TIMEOUT: Duration = Duration::from_secs(2);

/// AFM replies are one JSON envelope. Keep enough room for a generously sized generated note while
/// preventing a broken or hostile sidecar from turning stdout into an unbounded host allocation.
/// The reader continues draining after this limit so the child can still exit and be reaped.
const SIDECAR_OUTPUT_LIMIT_BYTES: usize = 1024 * 1024;
const AFM_QUARANTINE_KEY: &str = "afm-unreaped-child";

/// A failed kill/reap must not drop the last Child handle and then release model admission while
/// the AFM process may still own RAM/CPU. Retain every unproven child here; model admission checks
/// this owner, and recording Start explicitly retries bounded death proof after closing new work.
fn unreaped_children() -> &'static Mutex<Vec<std::process::Child>> {
    static CHILDREN: OnceLock<Mutex<Vec<std::process::Child>>> = OnceLock::new();
    CHILDREN.get_or_init(|| Mutex::new(Vec::new()))
}

pub(crate) fn has_unreaped_child() -> bool {
    !unreaped_children()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .is_empty()
}

fn retain_unreaped_child(child: std::process::Child, failure: impl std::fmt::Display) -> AppError {
    let pid = child.id();
    unreaped_children()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(child);
    // Generation calls arrive here while their AppleFoundation lease is still held, so poison the
    // shared residency lane before that lease drops. A direct test/probe without admission may not
    // satisfy the precondition; the retained-child gate above remains fail-closed regardless.
    if let Err(error) = crate::perf::quarantine_resident_model(
        crate::perf::ResidentModelKind::AppleFoundation,
        AFM_QUARANTINE_KEY.to_string(),
    ) {
        tracing::warn!(target: "reason", error = %error, pid, "AFM child retained without coordinator quarantine; global child gate remains active");
    }
    AppError::Unavailable(format!(
        "afm sidecar teardown is unproven ({failure}); local AI and recording stay blocked until pid {pid} is reaped"
    ))
}

/// Retry kill/reap for every retained AFM child under one shared deadline. Called only after Start
/// installed priority and drained pre-admitted generations, so clearing an exact quarantine cannot
/// race a fresh AFM spawn.
pub(crate) fn reap_unreaped_for_recording(timeout: Duration) -> Result<bool> {
    let deadline = std::time::Instant::now() + timeout;
    let mut children = unreaped_children()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let index = 0usize;
    while index < children.len() {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        match kill_and_reap_bounded(&mut children[index], remaining) {
            Ok(()) => {
                let mut child = children.swap_remove(index);
                if let Err(error) = child.wait() {
                    children.push(child);
                    tracing::warn!(
                        target: "reason",
                        error = %error,
                        "retained AFM child could not confirm cached reap status"
                    );
                    return Ok(false);
                }
            }
            Err(error) => {
                tracing::warn!(target: "reason", error = %error, "retained AFM child still lacks death proof");
                return Ok(false);
            }
        }
    }
    drop(children);
    if crate::perf::resident_model_quarantine_key(crate::perf::ResidentModelKind::AppleFoundation)
        .is_some()
    {
        crate::perf::clear_resident_model_quarantine(
            crate::perf::ResidentModelKind::AppleFoundation,
            AFM_QUARANTINE_KEY,
        )?;
    }
    Ok(true)
}

/// TEST-ONLY process-wide lock serializing tests that require [`ENV_OVERRIDE`] to be UNSET (the
/// `reason.rs` dispatch tests, which `remove_var` it defensively against an externally-exported
/// value). Since the R8 hardening (2026-07-10) NO test SETS the env var anymore — every fixture
/// test injects its sidecar path via [`sidecar_path_with_override`] / [`probe_at`] instead — so
/// the old set→act→restore race is gone by construction; this lock only serializes the residual
/// `remove_var` writers. Mirrors `crate::embed::EMBED_SELECTION_TEST_LOCK`.
#[cfg(test)]
pub(crate) static AFM_ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Availability status of the on-device AFM sidecar, for the FE capability probe (`afm_available`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AfmStatus {
    /// The `meetnotes-afm` binary is bundled/resolvable on this machine.
    pub sidecar_present: bool,
    /// When the sidecar is present, whether its on-device model reports available (`--probe`);
    /// `None` when the sidecar is absent (nothing to ask). `Some(false)` = present but the model is
    /// unavailable (wrong OS / not yet downloaded / probe error).
    pub model_available: Option<bool>,
    /// A short, non-PII human explanation for the status (e.g. "sidecar not bundled (needs a
    /// macOS 26 build)").
    pub reason: String,
}

/// The two success shapes a sidecar reply can carry: free-form `Text` (the `reason` mode) or a
/// native-validated `Json` value (the `structured` mode). Public because it is the return type of
/// the pure [`parse_afm_response`] contract.
#[derive(Debug)]
pub enum AfmReply {
    Text(String),
    Json(Value),
}

/// The generation-reply envelope the sidecar prints for `reason` / `structured`.
#[derive(Deserialize)]
struct AfmEnvelope {
    status: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    json: Option<Value>,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// The `--probe` envelope: `{"status":"available"|"unavailable","reason":..}`.
#[derive(Deserialize)]
struct AfmProbeEnvelope {
    status: String,
    #[serde(default)]
    reason: Option<String>,
}

/// Resolve the AFM sidecar binary, or `None` when it is not present on this machine. Resolution
/// order (each candidate filtered by `.exists()`):
/// 1. `MURMUR_AFM_SIDECAR` runtime override (dev/test — enables the headless spawn round-trip);
/// 2. the bundled resource inside a shipped `.app` (`Contents/Resources/<name>`) when an
///    [`AppHandle`] is available (the command path);
/// 3. `<current_exe dir>/../Resources/<name>` — the same `.app` layout when NO `AppHandle` is
///    available (the reasoner-dispatch path builds no handle);
/// 4. the compile-time `AFM_BIN` (`OUT_DIR`) DEV fallback (unset until `afm/afm.swift` compiles on a
///    macOS-26 SDK machine, so `option_env!` is `None` today).
///
/// NEVER panics; a missing binary is `None`, which the callers treat as "use the stub".
///
/// THIN ENV WRAPPER (R8, 2026-07-10): the override VALUE is read here once and injected into
/// [`sidecar_path_with_override`], so tests exercise the exact same resolution logic by passing
/// the override as an ARGUMENT — no test ever mutates the process-wide env var (the old
/// `set_var`/`remove_var` dance raced parallel tests).
pub fn sidecar_path(app: Option<&AppHandle>) -> Option<PathBuf> {
    // DEV/TEST runtime override — debug/test builds ONLY (matches the MURMUR_DEV_* precedent), so a
    // signed release can never be pointed at a bring-your-own sidecar via the process env.
    #[cfg(any(test, debug_assertions))]
    let override_path = std::env::var(ENV_OVERRIDE).ok().map(PathBuf::from);
    #[cfg(not(any(test, debug_assertions)))]
    let override_path: Option<PathBuf> = None;
    sidecar_path_with_override(override_path, app)
}

/// Core of [`sidecar_path`] with the dev/test override INJECTED as an argument (R8): pure with
/// respect to the `MURMUR_AFM_SIDECAR` env var, so parallel tests can never race on it.
fn sidecar_path_with_override(
    override_path: Option<PathBuf>,
    app: Option<&AppHandle>,
) -> Option<PathBuf> {
    // 1. The injected dev/test override (a fixture script / a developer's own sidecar).
    if let Some(path) = override_path {
        if path.exists() {
            return Some(path);
        }
    }
    // 2. Bundled resource via the AppHandle (prod, command path).
    if let Some(app) = app {
        if let Ok(p) = app
            .path()
            .resolve(SIDECAR_NAME, tauri::path::BaseDirectory::Resource)
        {
            if p.exists() {
                return Some(p);
            }
        }
    }
    // 3. Sibling Resources dir via current_exe (prod, reasoner path with no AppHandle). Tauri .app
    //    layout is Contents/MacOS/<exe> + Contents/Resources/<sidecar>.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("..").join("Resources").join(SIDECAR_NAME);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }
    // 4. Dev OUT_DIR fallback (compile-time; None until the swift sidecar exists).
    option_env!("AFM_BIN")
        .map(PathBuf::from)
        .filter(|p| p.exists())
}

/// Serialize the `{mode, system, user, schema}` stdin request. `schema` is `null` for the free-form
/// `reason` mode and the JSON schema for the `structured` mode. Pure + infallible
/// ([`Value::to_string`] cannot fail for this in-memory value) so it is trivially unit-testable.
pub fn build_afm_request(mode: &str, system: &str, user: &str, schema: Option<&Value>) -> String {
    serde_json::json!({
        "mode": mode,
        "system": system,
        "user": user,
        "schema": schema,
    })
    .to_string()
}

/// Parse the sidecar's generation stdout into an [`AfmReply`]. Contract (NEVER panics):
/// - `status:"ok"` + `json` present ⇒ [`AfmReply::Json`] (prefer the native-validated object);
/// - `status:"ok"` + `text` present ⇒ [`AfmReply::Text`];
/// - `status:"unavailable"` ⇒ `Err(AppError::Unavailable(reason))`;
/// - `status:"error"` ⇒ `Err(AppError::Summarize(error))`;
/// - malformed / empty / unknown status ⇒ `Err(AppError::Summarize("afm: unparseable …"))`.
///
/// Every `Err` floors the reasoner to the deterministic path in `orchestrate.rs`.
pub fn parse_afm_response(stdout: &str) -> Result<AfmReply> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err(AppError::Summarize(
            "afm: unparseable sidecar output".to_string(),
        ));
    }
    let env: AfmEnvelope = serde_json::from_str(trimmed)
        .map_err(|_| AppError::Summarize("afm: unparseable sidecar output".to_string()))?;
    match env.status.as_str() {
        "ok" => {
            if let Some(json) = env.json {
                Ok(AfmReply::Json(json))
            } else if let Some(text) = env.text {
                Ok(AfmReply::Text(text))
            } else {
                Err(AppError::Summarize(
                    "afm: ok envelope carried neither json nor text".to_string(),
                ))
            }
        }
        "unavailable" => Err(AppError::Unavailable(
            env.reason
                .unwrap_or_else(|| "afm model unavailable".to_string()),
        )),
        "error" => Err(AppError::Summarize(
            env.error.unwrap_or_else(|| "afm sidecar error".to_string()),
        )),
        _ => Err(AppError::Summarize(
            "afm: unparseable sidecar output".to_string(),
        )),
    }
}

/// Parse the `--probe` stdout into `(model_available, reason)`. `available` ⇒ `true`; any other
/// status ⇒ `false`; malformed/empty ⇒ `Err`. NEVER panics.
fn parse_probe_output(stdout: &str) -> Result<(bool, String)> {
    let trimmed = stdout.trim();
    let env: AfmProbeEnvelope = serde_json::from_str(trimmed)
        .map_err(|_| AppError::Summarize("afm: unparseable probe output".to_string()))?;
    Ok((env.status == "available", env.reason.unwrap_or_default()))
}

/// Build a hardened [`std::process::Command`] for the sidecar: `env_clear()` + a fixed `PATH`, with
/// only `HOME`/`USER`/`LOGNAME`/`TMPDIR` passed through (the on-device model container is per-user).
/// Mirrors `calendar::run_sidecar` so no DEK/KEK/token/`MURMUR_DEV_*` can be inherited by the child.
fn hardened_command(bin: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(bin);
    cmd.env_clear().env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    for key in ["HOME", "USER", "LOGNAME", "TMPDIR"] {
        if let Ok(val) = std::env::var(key) {
            cmd.env(key, val);
        }
    }
    cmd
}

/// Kill a live child and reap it within `timeout`. A race where the child exits just before `kill`
/// is treated as success only after `try_wait` proves the exit. No error is discarded.
fn kill_and_reap_bounded(child: &mut std::process::Child, timeout: Duration) -> Result<()> {
    let pre_kill_error = match child.try_wait() {
        Ok(Some(_)) => return Ok(()),
        Ok(None) => None,
        // A failed status probe must not prevent the best-effort kill. Preserve it in any teardown
        // error below instead of turning an observation failure into an orphaned live child.
        Err(error) => Some(error),
    };

    if let Err(kill_error) = child.kill() {
        return match child.try_wait() {
            Ok(Some(_)) => Ok(()),
            Ok(None) => Err(AppError::Summarize(match pre_kill_error {
                Some(precheck) => format!(
                    "afm: pre-kill status check failed: {precheck}; sidecar kill failed: {kill_error}"
                ),
                None => format!("afm: sidecar kill failed: {kill_error}"),
            })),
            Err(wait_error) => Err(AppError::Summarize(format!(
                "afm: sidecar kill failed: {kill_error}; status check failed: {wait_error}"
            ))),
        };
    }

    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) if std::time::Instant::now() >= deadline => {
                return Err(AppError::Summarize(
                    "afm: killed sidecar did not reap before deadline".to_string(),
                ))
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => {
                return Err(AppError::Summarize(format!(
                    "afm: sidecar reap failed: {error}"
                )))
            }
        }
    }
}

/// Drain a child's stdout concurrently with its runtime. The byte buffer is capped, but excess
/// output is discarded until EOF so a full pipe can never block the child before [`wait_bounded`]
/// observes its exit. Completion is reported over a capacity-one channel; callers collect it only
/// after exit/kill has been proven and always under [`SIDECAR_REAP_TIMEOUT`].
fn spawn_bounded_stdout_reader(
    mut stdout: std::process::ChildStdout,
    thread_name: &str,
) -> std::result::Result<std::sync::mpsc::Receiver<std::io::Result<Vec<u8>>>, std::io::Error> {
    use std::io::Read;

    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name(thread_name.to_string())
        .spawn(move || {
            let mut output = Vec::new();
            let mut chunk = [0_u8; 8192];
            let mut exceeded_limit = false;
            let result = loop {
                match stdout.read(&mut chunk) {
                    Ok(0) => {
                        if exceeded_limit {
                            break Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "afm sidecar output exceeded bounded limit",
                            ));
                        }
                        break Ok(output);
                    }
                    Ok(read) => {
                        let remaining = SIDECAR_OUTPUT_LIMIT_BYTES.saturating_sub(output.len());
                        let retained = read.min(remaining);
                        output.extend_from_slice(&chunk[..retained]);
                        if retained < read {
                            exceeded_limit = true;
                        }
                    }
                    Err(error) => break Err(error),
                }
            };
            let _ = tx.send(result);
        })?;
    Ok(rx)
}

/// Receive a worker result before one shared teardown deadline. Using a shared deadline prevents
/// two sequential channel collections from each consuming the full reap timeout.
fn recv_before_deadline<T>(
    rx: &std::sync::mpsc::Receiver<T>,
    deadline: std::time::Instant,
) -> std::result::Result<T, std::sync::mpsc::RecvTimeoutError> {
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    rx.recv_timeout(remaining)
}

fn worker_completion_summary(
    result: &std::result::Result<std::io::Result<()>, std::sync::mpsc::RecvTimeoutError>,
) -> String {
    match result {
        Ok(Ok(())) => "completed".to_string(),
        Ok(Err(error)) => format!("failed: {error}"),
        Err(error) => format!("not finished: {error}"),
    }
}

fn output_completion_summary(
    result: &std::result::Result<std::io::Result<Vec<u8>>, std::sync::mpsc::RecvTimeoutError>,
) -> String {
    match result {
        Ok(Ok(_)) => "completed".to_string(),
        Ok(Err(error)) => format!("failed: {error}"),
        Err(error) => format!("not finished: {error}"),
    }
}

/// Poll a spawned child to exit under a hard [`SIDECAR_TIMEOUT`] wall-clock cap, hard-killing and
/// bounded-reaping a wedged child at the deadline. Returns the proven exit status; `Err` on timeout
/// or any wait/kill/reap failure. NEVER panics.
fn wait_bounded(child: &mut std::process::Child) -> Result<std::process::ExitStatus> {
    let deadline = std::time::Instant::now() + SIDECAR_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    tracing::warn!(target: "reason", "afm sidecar timed out; killing");
                    if let Err(teardown) = kill_and_reap_bounded(child, SIDECAR_REAP_TIMEOUT) {
                        return Err(AppError::Summarize(format!(
                            "afm: sidecar timed out; teardown failed: {teardown}"
                        )));
                    }
                    return Err(AppError::Summarize("afm: sidecar timed out".to_string()));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                return match kill_and_reap_bounded(child, SIDECAR_REAP_TIMEOUT) {
                    Ok(()) => Err(AppError::Summarize(format!(
                        "afm: sidecar wait failed: {e}"
                    ))),
                    Err(teardown) => Err(AppError::Summarize(format!(
                        "afm: sidecar wait failed: {e}; teardown failed: {teardown}"
                    ))),
                };
            }
        }
    }
}

/// Spawn the sidecar, stream `request` to its STDIN, bound its runtime, and return its STDOUT.
///
/// DEADLOCK AVOIDANCE: owned reader/writer threads concurrently drain stdout and write/drop stdin
/// while the main thread runs the bounded wait. Both report over capacity-one channels and are
/// collected only after proven exit/kill, under one small deadline. Every failure (spawn / write /
/// timeout / kill / reap / exit / read) is an `Err` (which floors the reasoner). NO transcript ever
/// touches disk. NEVER panics.
fn run_afm(bin: &Path, request: &str) -> Result<String> {
    use std::io::Write;
    use std::process::Stdio;

    let mut cmd = hardened_command(bin);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = cmd
        .spawn()
        .map_err(|e| AppError::Summarize(format!("afm: spawn failed: {e}")))?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let teardown = kill_and_reap_bounded(&mut child, SIDECAR_REAP_TIMEOUT);
            return match teardown {
                Ok(()) => Err(AppError::Summarize(
                    "afm: stdout pipe unavailable".to_string(),
                )),
                Err(error) => Err(retain_unreaped_child(
                    child,
                    format!("stdout pipe unavailable; teardown failed: {error}"),
                )),
            };
        }
    };
    let output_rx = match spawn_bounded_stdout_reader(stdout, "murmur-afm-stdout") {
        Ok(rx) => rx,
        Err(spawn_error) => {
            let teardown = kill_and_reap_bounded(&mut child, SIDECAR_REAP_TIMEOUT);
            return match teardown {
                Ok(()) => Err(AppError::Summarize(format!(
                    "afm: stdout reader spawn failed: {spawn_error}"
                ))),
                Err(error) => Err(retain_unreaped_child(
                    child,
                    format!("stdout reader spawn failed: {spawn_error}; teardown failed: {error}"),
                )),
            };
        }
    };
    let mut stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => {
            let teardown = kill_and_reap_bounded(&mut child, SIDECAR_REAP_TIMEOUT);
            let collection_deadline = std::time::Instant::now() + SIDECAR_REAP_TIMEOUT;
            let output_result = recv_before_deadline(&output_rx, collection_deadline);
            let output_summary = output_completion_summary(&output_result);
            return match teardown {
                Ok(()) => Err(AppError::Summarize(format!(
                    "afm: stdin pipe unavailable; stdout completion: {output_summary}"
                ))),
                Err(error) => Err(retain_unreaped_child(
                    child,
                    format!(
                        "stdin pipe unavailable; teardown failed: {error}; stdout completion: {output_summary}"
                    ),
                )),
            };
        }
    };
    let request = request.as_bytes().to_vec();
    let (write_tx, write_rx) = std::sync::mpsc::sync_channel(1);
    if let Err(spawn_error) = std::thread::Builder::new()
        .name("murmur-afm-stdin".into())
        .spawn(move || {
            let result = stdin.write_all(&request);
            // `stdin` drops here, signalling EOF even when the write failed.
            let _ = write_tx.send(result);
        })
    {
        let teardown = kill_and_reap_bounded(&mut child, SIDECAR_REAP_TIMEOUT);
        let collection_deadline = std::time::Instant::now() + SIDECAR_REAP_TIMEOUT;
        let output_result = recv_before_deadline(&output_rx, collection_deadline);
        let output_summary = output_completion_summary(&output_result);
        return match teardown {
            Ok(()) => Err(AppError::Summarize(format!(
                "afm: stdin writer spawn failed: {spawn_error}; stdout completion: {output_summary}"
            ))),
            Err(error) => Err(retain_unreaped_child(
                child,
                format!(
                    "stdin writer spawn failed: {spawn_error}; teardown failed: {error}; stdout completion: {output_summary}"
                ),
            )),
        };
    }

    let wait_result = wait_bounded(&mut child);
    let collection_deadline = std::time::Instant::now() + SIDECAR_REAP_TIMEOUT;
    let write_result = recv_before_deadline(&write_rx, collection_deadline);
    let output_result = recv_before_deadline(&output_rx, collection_deadline);
    let status = match wait_result {
        Ok(status) => status,
        Err(wait_error) => {
            let failure = format!(
                "{wait_error}; stdin completion: {}; stdout completion: {}",
                worker_completion_summary(&write_result),
                output_completion_summary(&output_result)
            );
            return match child.try_wait() {
                Ok(Some(_)) => Err(AppError::Summarize(failure)),
                Ok(None) | Err(_) => Err(retain_unreaped_child(child, failure)),
            };
        }
    };
    match write_result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            return Err(AppError::Summarize(format!(
                "afm: stdin write failed: {error}"
            )))
        }
        Err(error) => {
            return Err(AppError::Summarize(format!(
                "afm: stdin writer did not finish after child exit: {error}"
            )))
        }
    }
    let output = match output_result {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            return Err(AppError::Summarize(format!(
                "afm: stdout read failed: {error}"
            )))
        }
        Err(error) => {
            return Err(AppError::Summarize(format!(
                "afm: stdout reader did not finish after child exit: {error}"
            )))
        }
    };
    if !status.success() {
        return Err(AppError::Summarize(format!(
            "afm: sidecar exited with {}",
            status
        )));
    }
    Ok(String::from_utf8_lossy(&output).into_owned())
}

/// Spawn the sidecar with a single `--probe` arg (a fast availability check, NOT a generation) and
/// return its STDOUT. No stdin. Same hardening + bounded wait as [`run_afm`]. NEVER panics.
fn run_probe(bin: &Path) -> Result<String> {
    use std::process::Stdio;

    let mut cmd = hardened_command(bin);
    cmd.arg("--probe")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    let mut child = cmd
        .spawn()
        .map_err(|e| AppError::Summarize(format!("afm: probe spawn failed: {e}")))?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let teardown = kill_and_reap_bounded(&mut child, SIDECAR_REAP_TIMEOUT);
            return match teardown {
                Ok(()) => Err(AppError::Summarize(
                    "afm: probe stdout pipe unavailable".to_string(),
                )),
                Err(error) => Err(retain_unreaped_child(
                    child,
                    format!("probe stdout pipe unavailable; teardown failed: {error}"),
                )),
            };
        }
    };
    let output_rx = match spawn_bounded_stdout_reader(stdout, "murmur-afm-probe-stdout") {
        Ok(rx) => rx,
        Err(spawn_error) => {
            let teardown = kill_and_reap_bounded(&mut child, SIDECAR_REAP_TIMEOUT);
            return match teardown {
                Ok(()) => Err(AppError::Summarize(format!(
                    "afm: probe stdout reader spawn failed: {spawn_error}"
                ))),
                Err(error) => Err(retain_unreaped_child(
                    child,
                    format!(
                        "probe stdout reader spawn failed: {spawn_error}; teardown failed: {error}"
                    ),
                )),
            };
        }
    };
    let wait_result = wait_bounded(&mut child);
    let collection_deadline = std::time::Instant::now() + SIDECAR_REAP_TIMEOUT;
    let output_result = recv_before_deadline(&output_rx, collection_deadline);
    let status = match wait_result {
        Ok(status) => status,
        Err(wait_error) => {
            let failure = format!(
                "{wait_error}; probe stdout completion: {}",
                output_completion_summary(&output_result)
            );
            return match child.try_wait() {
                Ok(Some(_)) => Err(AppError::Summarize(failure)),
                Ok(None) | Err(_) => Err(retain_unreaped_child(child, failure)),
            };
        }
    };
    let output = match output_result {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => {
            return Err(AppError::Summarize(format!(
                "afm: probe stdout read failed: {error}"
            )))
        }
        Err(error) => {
            return Err(AppError::Summarize(format!(
                "afm: probe stdout reader did not finish after child exit: {error}"
            )))
        }
    };
    if !status.success() {
        return Err(AppError::Summarize(format!(
            "afm: probe sidecar exited with {status}"
        )));
    }
    Ok(String::from_utf8_lossy(&output).into_owned())
}

/// Availability probe for the FE (`afm_available`). GRACEFUL on every path:
/// - sidecar absent ⇒ `{sidecar_present:false, model_available:None}` (the current state on every
///   non-macOS-26 machine);
/// - sidecar present + probe ok ⇒ `{sidecar_present:true, model_available:Some(available)}`;
/// - sidecar present + probe error ⇒ `{sidecar_present:true, model_available:Some(false)}` + reason.
///
/// NEVER panics, NEVER egresses (a local availability check only).
pub fn probe(app: Option<&AppHandle>) -> AfmStatus {
    let Some(bin) = sidecar_path(app) else {
        return absent_probe_status();
    };
    // Treat the capability probe as a real AFM process lifetime. Even if today's Swift `--probe`
    // is cheap, it is not allowed to spawn beside capture or another resident model on assumption.
    let result = crate::perf::with_model_generation(
        None,
        crate::perf::ResidentModelKind::AppleFoundation,
        || run_probe(&bin).and_then(|out| parse_probe_output(&out)),
    );
    probe_status_from_result(result)
}

/// Core of [`probe`] over an already-resolved sidecar path (R8): env-free, so tests drive the
/// probe against a fixture (or `None`) without touching `MURMUR_AFM_SIDECAR`.
#[cfg(test)]
fn probe_at(bin: Option<PathBuf>) -> AfmStatus {
    let Some(bin) = bin else {
        return absent_probe_status();
    };
    probe_status_from_result(run_probe(&bin).and_then(|out| parse_probe_output(&out)))
}

fn absent_probe_status() -> AfmStatus {
    AfmStatus {
        sidecar_present: false,
        model_available: None,
        reason: "sidecar not bundled (needs a macOS 26 build)".to_string(),
    }
}

fn probe_status_from_result(result: Result<(bool, String)>) -> AfmStatus {
    match result {
        Ok((available, reason)) => AfmStatus {
            sidecar_present: true,
            model_available: Some(available),
            reason: if reason.is_empty() {
                if available {
                    "on-device model available".to_string()
                } else {
                    "on-device model unavailable".to_string()
                }
            } else {
                reason
            },
        },
        Err(e) => AfmStatus {
            sidecar_present: true,
            model_available: Some(false),
            reason: format!("afm probe failed: {e}"),
        },
    }
}

/// The EXPERIMENTAL on-device Apple Foundation Models reasoner. Holds only the resolved sidecar
/// path (the ~3B model lives OS-resident in the sidecar process, not loaded by us), so it is cheap
/// to build per call — no cache slot, like [`super::CloudReasoner`].
pub struct AfmReasoner {
    /// Stable id (`"afm"`) so [`LocalReasoner::id`] can return a borrow.
    id: String,
    /// The resolved `meetnotes-afm` binary.
    bin: PathBuf,
}

impl AfmReasoner {
    /// Build over a resolved sidecar path. `id()` is `"afm"`.
    pub fn new(bin: PathBuf) -> Self {
        Self {
            id: "afm".to_string(),
            bin,
        }
    }
}

impl LocalReasoner for AfmReasoner {
    fn id(&self) -> &str {
        &self.id
    }

    fn reason(&self, system: &str, user: &str) -> Result<String> {
        let req = build_afm_request("reason", system, user, None);
        let out = run_afm(&self.bin, &req)?;
        match parse_afm_response(&out)? {
            AfmReply::Text(t) => Ok(t),
            // A JSON reply to a free-form ask is unusual but valid — surface it as text.
            AfmReply::Json(v) => Ok(v.to_string()),
        }
    }

    fn structured(&self, system: &str, user: &str, json_schema: &Value) -> Result<Value> {
        let req = build_afm_request("structured", system, user, Some(json_schema));
        let out = run_afm(&self.bin, &req)?;
        match parse_afm_response(&out)? {
            // Prefer the native-validated JSON.
            AfmReply::Json(v) => Ok(v),
            // Fall back to the robust extractor (like MistralReasoner / CloudReasoner) when the
            // sidecar returned prose/fenced text instead of a validated object.
            AfmReply::Text(t) => parse_first_json(&t),
        }
    }
}

/// Resolve the AFM reasoner for a one-shot [`super::active_reasoner`] dispatch: the real
/// [`AfmReasoner`] when the sidecar resolves, else the dependency-free [`StubReasoner`]. Keeps the
/// fallback IDENTICAL to `Local`-without-a-GGUF (id `"stub"`, zero egress). NEVER panics.
pub fn afm_reasoner(_config: &AppConfig) -> Box<dyn LocalReasoner> {
    match sidecar_path(None) {
        Some(bin) => {
            tracing::info!(target: "reason", "afm sidecar resolved; using on-device apple foundation reasoner");
            Box::new(AfmReasoner::new(bin))
        }
        None => {
            tracing::info!(target: "reason", "no afm sidecar; using stub reasoner");
            Box::new(StubReasoner)
        }
    }
}

/// `Arc` variant for the live [`super::ReasonerCell::current_for`] dispatch — same fallback as
/// [`afm_reasoner`]. Built per call (the reasoner holds only a `PathBuf`, so there is nothing
/// expensive to cache). NEVER panics.
pub fn afm_reasoner_arc(_cfg: &AppConfig) -> Arc<dyn LocalReasoner> {
    match sidecar_path(None) {
        Some(bin) => Arc::new(AfmReasoner::new(bin)),
        None => Arc::new(StubReasoner),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- pure protocol: build_afm_request / parse_afm_response ---------------------------------

    #[test]
    fn build_request_round_trips_mode_system_user_schema() {
        // reason mode: schema is null.
        let req = build_afm_request("reason", "sys", "usr", None);
        let v: Value = serde_json::from_str(&req).unwrap();
        assert_eq!(v["mode"], serde_json::json!("reason"));
        assert_eq!(v["system"], serde_json::json!("sys"));
        assert_eq!(v["user"], serde_json::json!("usr"));
        assert_eq!(v["schema"], serde_json::json!(null));

        // structured mode: the schema is carried verbatim.
        let schema =
            serde_json::json!({ "type": "object", "properties": { "n": { "type": "number" } } });
        let req = build_afm_request("structured", "s", "u", Some(&schema));
        let v: Value = serde_json::from_str(&req).unwrap();
        assert_eq!(v["mode"], serde_json::json!("structured"));
        assert_eq!(v["schema"], schema);
    }

    #[test]
    fn parse_ok_text_is_text() {
        match parse_afm_response(r#"{"status":"ok","text":"hello world"}"#).unwrap() {
            AfmReply::Text(t) => assert_eq!(t, "hello world"),
            AfmReply::Json(_) => panic!("expected Text"),
        }
    }

    #[test]
    fn parse_ok_json_is_json_and_wins_over_text() {
        // When both json and text are present, the native-validated json is preferred.
        let s = r#"{"status":"ok","json":{"entities":["Atlas"]},"text":"ignored"}"#;
        match parse_afm_response(s).unwrap() {
            AfmReply::Json(v) => assert_eq!(v["entities"], serde_json::json!(["Atlas"])),
            AfmReply::Text(_) => panic!("expected Json to win"),
        }
    }

    #[test]
    fn parse_unavailable_is_unavailable_err() {
        match parse_afm_response(r#"{"status":"unavailable","reason":"needs macOS 26"}"#) {
            Err(AppError::Unavailable(r)) => assert_eq!(r, "needs macOS 26"),
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[test]
    fn parse_error_is_summarize_err() {
        match parse_afm_response(r#"{"status":"error","error":"model load failed"}"#) {
            Err(AppError::Summarize(e)) => assert_eq!(e, "model load failed"),
            other => panic!("expected Summarize, got {other:?}"),
        }
    }

    #[test]
    fn parse_malformed_and_empty_are_err_never_panic() {
        assert!(parse_afm_response("not json at all").is_err());
        assert!(parse_afm_response(r#"{"status":"#).is_err());
        assert!(parse_afm_response("").is_err());
        assert!(parse_afm_response("   \n  ").is_err());
        // ok with neither text nor json is an error, not a panic.
        assert!(parse_afm_response(r#"{"status":"ok"}"#).is_err());
        // an unknown status is treated as unparseable.
        assert!(parse_afm_response(r#"{"status":"weird"}"#).is_err());
    }

    #[test]
    fn parse_probe_output_maps_status_to_bool() {
        assert_eq!(
            parse_probe_output(r#"{"status":"available","reason":"ready"}"#).unwrap(),
            (true, "ready".to_string())
        );
        let (avail, _) =
            parse_probe_output(r#"{"status":"unavailable","reason":"no os26"}"#).unwrap();
        assert!(!avail);
        assert!(parse_probe_output("garbage").is_err());
    }

    // ---- availability probe: the sidecar-ABSENT path (this CLT-only machine) -------------------

    /// On every non-macOS-26 machine the sidecar is absent, so the probe reports
    /// `sidecar_present:false, model_available:None` and NEVER panics — the graceful capability
    /// floor `afm_available` returns. Driven through the env-free [`probe_at`] core (R8), so a
    /// parallel test / an exported shell var can never flip this into a present sidecar.
    #[test]
    fn probe_reports_absent_when_no_sidecar() {
        let status = probe_at(None);
        assert!(!status.sidecar_present);
        assert_eq!(status.model_available, None);
        assert!(!status.reason.is_empty());
    }

    /// With no sidecar resolvable, the resolution core returns `None` for every candidate on this
    /// machine (no override injected, no bundle, no OUT_DIR sidecar), which the constructors map to
    /// the deterministic stub (id `"stub"`) — byte-identical to `BrainBackend::Off`, zero egress,
    /// no panic. Env-free via [`sidecar_path_with_override`] (R8).
    #[test]
    fn constructors_fall_back_to_stub_without_sidecar() {
        assert!(
            sidecar_path_with_override(None, None).is_none(),
            "no sidecar candidate resolves on a CLT-only machine"
        );
        // The constructors' mapping over an unresolvable path is the stub. (They read the env
        // wrapper, which no test sets anymore — deterministic in-process.)
        let cfg = AppConfig::default();
        assert_eq!(afm_reasoner(&cfg).id(), "stub");
        assert_eq!(afm_reasoner_arc(&cfg).id(), "stub");
    }

    // ---- spawn round-trip via the MURMUR_AFM_SIDECAR fixture (headless, any OS) ----------------

    /// Write an executable fixture script that drains stdin then echoes `envelope` on stdout, so the
    /// full spawn + stdin-write + stdout-parse path can be exercised WITHOUT a real macOS-26 sidecar.
    #[cfg(unix)]
    fn write_fixture(tag: &str, envelope: &str) -> PathBuf {
        write_script_fixture(tag, &format!("cat >/dev/null\nprintf '%s' '{envelope}'\n"))
    }

    #[cfg(unix)]
    fn write_script_fixture(tag: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let mut p = std::env::temp_dir();
        p.push(format!(
            "murmur-afm-fixture-{tag}-{}-{}.sh",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let script = format!("#!/bin/sh\n{body}");
        std::fs::write(&p, script).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[cfg(unix)]
    #[test]
    fn reason_round_trips_through_a_fixture_sidecar() {
        let fixture = write_fixture("reason", r#"{"status":"ok","text":"canned answer"}"#);

        // Resolve through the SAME override branch production takes — the value injected as an
        // argument (R8), never via the shared process env.
        let bin = sidecar_path_with_override(Some(fixture.clone()), None)
            .expect("fixture must resolve via the injected override");
        let r = AfmReasoner::new(bin);
        assert_eq!(r.id(), "afm");
        let got = r.reason("system prompt", "user prompt").unwrap();
        assert_eq!(got, "canned answer");

        let _ = std::fs::remove_file(&fixture);
    }

    #[cfg(unix)]
    #[test]
    fn structured_round_trips_native_json_through_a_fixture_sidecar() {
        let fixture = write_fixture("structured", r#"{"status":"ok","json":{"n":2,"ok":true}}"#);

        let bin = sidecar_path_with_override(Some(fixture.clone()), None)
            .expect("fixture must resolve via the injected override");
        let r = AfmReasoner::new(bin);
        let schema = serde_json::json!({ "type": "object" });
        let v = r.structured("sys", "user", &schema).unwrap();
        assert_eq!(v["n"], serde_json::json!(2));
        assert_eq!(v["ok"], serde_json::json!(true));

        let _ = std::fs::remove_file(&fixture);
    }

    /// Regression: output larger than a typical OS pipe must be drained while the process is still
    /// alive. This fixture writes first and reads stdin second, the exact ordering that deadlocked
    /// the old wait-before-read implementation.
    #[cfg(unix)]
    #[test]
    fn stdout_larger_than_pipe_is_drained_concurrently() {
        let fixture =
            write_script_fixture("large-stdout", "head -c 131072 /dev/zero\ncat >/dev/null\n");

        let output = run_afm(&fixture, r#"{"mode":"reason"}"#).unwrap();
        assert_eq!(output.len(), 131072);

        let _ = std::fs::remove_file(&fixture);
    }

    /// A broken sidecar may produce arbitrary output forever. We still drain through EOF so it can
    /// exit, but never retain more than the one-envelope cap in host RAM.
    #[cfg(unix)]
    #[test]
    fn stdout_over_limit_is_rejected_after_bounded_drain() {
        let fixture = write_script_fixture(
            "oversized-stdout",
            &format!(
                "head -c {} /dev/zero\ncat >/dev/null\n",
                SIDECAR_OUTPUT_LIMIT_BYTES + 1
            ),
        );

        let error = run_afm(&fixture, r#"{"mode":"reason"}"#).unwrap_err();
        assert!(format!("{error}").contains("output exceeded bounded limit"));

        let _ = std::fs::remove_file(&fixture);
    }

    /// An `unavailable` envelope from the sidecar surfaces as `Err(Unavailable)` end-to-end (the
    /// reasoner then floors) — proving the spawn path propagates the sidecar's own status.
    #[cfg(unix)]
    #[test]
    fn unavailable_envelope_propagates_as_err_through_the_spawn_path() {
        let fixture = write_fixture("unavail", r#"{"status":"unavailable","reason":"no model"}"#);

        let bin = sidecar_path_with_override(Some(fixture.clone()), None).unwrap();
        let r = AfmReasoner::new(bin);
        match r.reason("s", "u") {
            Err(AppError::Unavailable(reason)) => assert_eq!(reason, "no model"),
            other => panic!("expected Unavailable from the sidecar, got {other:?}"),
        }

        let _ = std::fs::remove_file(&fixture);
    }

    /// The probe path over a fixture: a `--probe` reply of `available` is reported as
    /// `sidecar_present:true, model_available:Some(true)`. Driven through the env-free
    /// [`probe_at`] core (R8) — no `set_var`, no race with the stub-fallback tests.
    #[cfg(unix)]
    #[test]
    fn probe_reports_available_through_a_fixture_sidecar() {
        let fixture = write_fixture("probe", r#"{"status":"available","reason":"ready"}"#);

        let status = probe_at(Some(fixture.clone()));
        assert!(status.sidecar_present);
        assert_eq!(status.model_available, Some(true));

        let _ = std::fs::remove_file(&fixture);
    }

    /// R8 — a non-existent injected override falls through the override branch (never resolves a
    /// missing file), exactly like the env wrapper did.
    #[test]
    fn missing_override_path_falls_through() {
        let bogus = PathBuf::from("/definitely/not/a/real/sidecar-fixture");
        assert!(sidecar_path_with_override(Some(bogus), None).is_none());
    }
}
