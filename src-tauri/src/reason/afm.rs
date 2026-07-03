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
use std::sync::Arc;
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

/// TEST-ONLY process-wide lock serializing every test that reads/writes the [`ENV_OVERRIDE`] env var
/// (in THIS module AND in `reason` / `commands` tests). `cargo test` runs in parallel, so without it
/// the spawn round-trip (which SETS `MURMUR_AFM_SIDECAR`) could race the stub-fallback tests (which
/// require it UNSET) and flip an "absent sidecar → stub" assertion into an "afm" id. Mirrors
/// `crate::embed::EMBED_SELECTION_TEST_LOCK`. Hold it for the whole set→act→restore span.
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
pub fn sidecar_path(app: Option<&AppHandle>) -> Option<PathBuf> {
    // 1. DEV/TEST runtime override — debug/test builds ONLY (matches the MURMUR_DEV_* precedent), so a
    //    signed release can never be pointed at a bring-your-own sidecar via the process env.
    #[cfg(any(test, debug_assertions))]
    if let Ok(p) = std::env::var(ENV_OVERRIDE) {
        let path = PathBuf::from(p);
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

/// Poll a spawned child to exit under a hard [`SIDECAR_TIMEOUT`] wall-clock cap, hard-killing a
/// wedged child at the deadline so it can never block the caller. `Ok(())` once the child has exited
/// (any status); `Err` on timeout or a wait error. NEVER panics.
fn wait_bounded(child: &mut std::process::Child) -> Result<()> {
    let deadline = std::time::Instant::now() + SIDECAR_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_status)) => return Ok(()),
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    tracing::warn!(target: "reason", "afm sidecar timed out; killing");
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(AppError::Summarize("afm: sidecar timed out".to_string()));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                let _ = child.kill();
                return Err(AppError::Summarize(format!("afm: sidecar wait failed: {e}")));
            }
        }
    }
}

/// Spawn the sidecar, stream `request` to its STDIN, bound its runtime, and return its STDOUT.
///
/// DEADLOCK AVOIDANCE: the request is written on a SCOPED writer thread that drops stdin (EOF) when
/// done, WHILE the main thread runs the bounded try_wait loop — so neither side blocks on a full OS
/// pipe buffer. Every failure (spawn / timeout / non-zero exit / read) is an `Err` (which floors the
/// reasoner). NO transcript ever touches disk — stdin pipe only. NEVER panics.
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
    let stdin = child.stdin.take();

    std::thread::scope(|scope| -> Result<String> {
        // Writer thread: stream the request, then drop stdin (EOF). A BrokenPipe (child already
        // exited / didn't read stdin) is ignored — the bounded wait below owns the outcome.
        if let Some(mut stdin) = stdin {
            scope.spawn(move || {
                let _ = stdin.write_all(request.as_bytes());
                // `stdin` dropped here → the child's stdin read sees EOF.
            });
        }
        wait_bounded(&mut child)?;
        let out = child
            .wait_with_output()
            .map_err(|e| AppError::Summarize(format!("afm: read output failed: {e}")))?;
        if !out.status.success() {
            return Err(AppError::Summarize(format!(
                "afm: sidecar exited with {}",
                out.status
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    })
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
    wait_bounded(&mut child)?;
    let out = child
        .wait_with_output()
        .map_err(|e| AppError::Summarize(format!("afm: probe read failed: {e}")))?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
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
        return AfmStatus {
            sidecar_present: false,
            model_available: None,
            reason: "sidecar not bundled (needs a macOS 26 build)".to_string(),
        };
    };
    match run_probe(&bin).and_then(|out| parse_probe_output(&out)) {
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
        let schema = serde_json::json!({ "type": "object", "properties": { "n": { "type": "number" } } });
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
        let (avail, _) = parse_probe_output(r#"{"status":"unavailable","reason":"no os26"}"#).unwrap();
        assert!(!avail);
        assert!(parse_probe_output("garbage").is_err());
    }

    // ---- availability probe: the sidecar-ABSENT path (this CLT-only machine) -------------------

    /// On every non-macOS-26 machine the sidecar is absent, so `probe(None)` reports
    /// `sidecar_present:false, model_available:None` and NEVER panics — the graceful capability
    /// floor `afm_available` returns.
    #[test]
    fn probe_reports_absent_when_no_sidecar() {
        let _g = AFM_ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(ENV_OVERRIDE);
        let status = probe(None);
        assert!(!status.sidecar_present);
        assert_eq!(status.model_available, None);
        assert!(!status.reason.is_empty());
    }

    /// With no sidecar resolvable, both constructors degrade to the deterministic stub (id `"stub"`)
    /// — byte-identical to `BrainBackend::Off`, zero egress, no panic.
    #[test]
    fn constructors_fall_back_to_stub_without_sidecar() {
        let _g = AFM_ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(ENV_OVERRIDE);
        let cfg = AppConfig::default();
        assert_eq!(afm_reasoner(&cfg).id(), "stub");
        assert_eq!(afm_reasoner_arc(&cfg).id(), "stub");
    }

    // ---- spawn round-trip via the MURMUR_AFM_SIDECAR fixture (headless, any OS) ----------------

    /// Write an executable fixture script that drains stdin then echoes `envelope` on stdout, so the
    /// full spawn + stdin-write + stdout-parse path can be exercised WITHOUT a real macOS-26 sidecar.
    #[cfg(unix)]
    fn write_fixture(tag: &str, envelope: &str) -> PathBuf {
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
        // Drain stdin (so the parent's write never SIGPIPEs / blocks), then print the canned reply.
        // Single-quote the envelope for the shell; our envelopes contain no single quotes.
        let script = format!("#!/bin/sh\ncat >/dev/null\nprintf '%s' '{envelope}'\n");
        std::fs::write(&p, script).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[cfg(unix)]
    #[test]
    fn reason_round_trips_through_a_fixture_sidecar() {
        let _g = AFM_ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let fixture = write_fixture("reason", r#"{"status":"ok","text":"canned answer"}"#);
        std::env::set_var(ENV_OVERRIDE, &fixture);

        // Resolve via sidecar_path(None) → the env-override branch → the fixture.
        let bin = sidecar_path(None).expect("fixture must resolve via MURMUR_AFM_SIDECAR");
        let r = AfmReasoner::new(bin);
        assert_eq!(r.id(), "afm");
        let got = r.reason("system prompt", "user prompt").unwrap();
        assert_eq!(got, "canned answer");

        std::env::remove_var(ENV_OVERRIDE);
        let _ = std::fs::remove_file(&fixture);
    }

    #[cfg(unix)]
    #[test]
    fn structured_round_trips_native_json_through_a_fixture_sidecar() {
        let _g = AFM_ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let fixture = write_fixture("structured", r#"{"status":"ok","json":{"n":2,"ok":true}}"#);
        std::env::set_var(ENV_OVERRIDE, &fixture);

        let bin = sidecar_path(None).expect("fixture must resolve via MURMUR_AFM_SIDECAR");
        let r = AfmReasoner::new(bin);
        let schema = serde_json::json!({ "type": "object" });
        let v = r.structured("sys", "user", &schema).unwrap();
        assert_eq!(v["n"], serde_json::json!(2));
        assert_eq!(v["ok"], serde_json::json!(true));

        std::env::remove_var(ENV_OVERRIDE);
        let _ = std::fs::remove_file(&fixture);
    }

    /// An `unavailable` envelope from the sidecar surfaces as `Err(Unavailable)` end-to-end (the
    /// reasoner then floors) — proving the spawn path propagates the sidecar's own status.
    #[cfg(unix)]
    #[test]
    fn unavailable_envelope_propagates_as_err_through_the_spawn_path() {
        let _g = AFM_ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let fixture = write_fixture("unavail", r#"{"status":"unavailable","reason":"no model"}"#);
        std::env::set_var(ENV_OVERRIDE, &fixture);

        let bin = sidecar_path(None).unwrap();
        let r = AfmReasoner::new(bin);
        match r.reason("s", "u") {
            Err(AppError::Unavailable(reason)) => assert_eq!(reason, "no model"),
            other => panic!("expected Unavailable from the sidecar, got {other:?}"),
        }

        std::env::remove_var(ENV_OVERRIDE);
        let _ = std::fs::remove_file(&fixture);
    }

    /// The probe path over a fixture: a `--probe` reply of `available` is reported as
    /// `sidecar_present:true, model_available:Some(true)`.
    #[cfg(unix)]
    #[test]
    fn probe_reports_available_through_a_fixture_sidecar() {
        let _g = AFM_ENV_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let fixture = write_fixture("probe", r#"{"status":"available","reason":"ready"}"#);
        std::env::set_var(ENV_OVERRIDE, &fixture);

        let status = probe(None);
        assert!(status.sidecar_present);
        assert_eq!(status.model_available, Some(true));

        std::env::remove_var(ENV_OVERRIDE);
        let _ = std::fs::remove_file(&fixture);
    }
}
