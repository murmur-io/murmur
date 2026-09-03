//! Reminder commands. The original Apple Reminders integration below remains a content-free
//! `osascript` adapter. The first-class Murmur reminder store is independent user data, while its
//! disposable Smart-audit suggestions ARE source-derived plaintext: every list/accept/dismiss path
//! must therefore gate and re-hash the canonical meeting or authored note under the lock lifecycle.
//! All symbols are re-exported at `crate::commands` via `pub use reminders::*;`.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::{ipc::Response, AppHandle, Manager, State};

use crate::error::AppError;
use crate::reason::{GenOptions, LocalReasoner};
use crate::reminder_audit::ReminderAuditCandidate;
use crate::state::AppState;
use crate::storage::models::{
    ReminderDraft, ReminderInboxItem, ReminderOrigin, ReminderSourceAnchor, ReminderSourceView,
    ReminderState, ReminderSuggestionView, ReminderSummary, ReminderView, RemindersSnapshot,
    StoredReminder, StoredReminderSuggestion,
};
use crate::storage::reminder_store::ReminderSuggestionPromotion;
use crate::transcribe::types::Segment;

/// Escape a string for embedding inside an AppleScript `"…"` literal: backslash + double-quote are
/// escaped, and raw CR/LF are flattened to spaces (an AppleScript string literal cannot span lines).
/// This is what stops the item text from breaking out of the quoted literal or injecting extra
/// statements (`"`, `end tell`, …) into the osascript program.
fn escape_applescript(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace(['\n', '\r'], " ")
}

/// Parse a strict ISO `YYYY-MM-DD` into `(year, month, day)`; `None` for anything else.
fn parse_iso_ymd(s: &str) -> Option<(i32, u32, u32)> {
    let s = s.trim();
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let y: i32 = s.get(0..4)?.parse().ok()?;
    let m: u32 = s.get(5..7)?.parse().ok()?;
    let d: u32 = s.get(8..10)?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some((y, m, d))
}

/// Build the osascript program that creates a Reminder named `name`. When `due_date` is a valid
/// ISO `YYYY-MM-DD`, attach `remind me date`/`due date` (defaulted to 9am local) so the date
/// actually lands in Reminders — previously the date was dropped. The name is
/// `escape_applescript`-escaped so its text can never break out of the string literal. The date is
/// built by setting `day` to 1 FIRST (so a year/month change can't overflow the current day-of-month),
/// then year, then month, then the real day.
pub(crate) fn build_reminder_script(name: &str, due_date: Option<&str>) -> String {
    let esc = escape_applescript(name);
    match due_date.and_then(parse_iso_ymd) {
        Some((y, m, d)) => format!(
            "set theDate to current date\n\
             set day of theDate to 1\n\
             set year of theDate to {y}\n\
             set month of theDate to {m}\n\
             set day of theDate to {d}\n\
             set hours of theDate to 9\n\
             set minutes of theDate to 0\n\
             set seconds of theDate to 0\n\
             tell application \"Reminders\" to make new reminder with properties {{name:\"{esc}\", remind me date:theDate, due date:theDate}}"
        ),
        None => format!(
            "tell application \"Reminders\" to make new reminder with properties {{name:\"{esc}\"}}"
        ),
    }
}

/// Add a macOS Reminder (via osascript) for an action item. A denied Reminders permission
/// surfaces a clear, actionable error rather than crashing the UI. When the item carries an ISO
/// due date, it is set as the reminder's due/remind date (best-effort; verify on a real Mac).
#[tauri::command]
pub async fn add_reminder(text: String, due_date: Option<String>) -> Result<(), AppError> {
    let name = text.trim().to_string();
    if name.is_empty() {
        return Err(AppError::InvalidArg("empty reminder".into()));
    }
    let due = due_date.as_deref().filter(|d| !d.is_empty());
    let script = build_reminder_script(&name, due);
    let out = tokio::task::spawn_blocking(move || {
        std::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output()
    })
    .await
    .map_err(|e| AppError::Unavailable(format!("reminder task failed: {e}")))?
    .map_err(|e| AppError::Unavailable(format!("osascript failed: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(AppError::Unavailable(crate::errcode::tag(
            crate::errcode::REMINDERS_DENIED,
            format!(
                "osascript refused: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        )))
    }
}

/// SYNCHRONOUS reminder creation for the off-thread voice-action dispatch (Flow B). Mirrors the
/// `add_reminder` command's osascript path, but blocking (it already runs on a detached task, so it
/// must not require an async runtime). Returns `Ok(())` on success, a typed `AppError` otherwise —
/// NEVER panics. NO PII logged by the caller; the reminder text is the user's own dictated note.
pub(crate) fn add_reminder_blocking(text: &str, due_date: Option<&str>) -> Result<(), AppError> {
    let name = text.trim();
    if name.is_empty() {
        return Err(AppError::InvalidArg("empty reminder".into()));
    }
    let due = due_date.filter(|d| !d.is_empty());
    let script = build_reminder_script(name, due);
    let out = std::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| AppError::Unavailable(format!("osascript failed: {e}")))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(AppError::Unavailable(crate::errcode::tag(
            crate::errcode::REMINDERS_DENIED,
            format!(
                "osascript refused: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        )))
    }
}

// ── First-class Murmur reminders (independent from the Apple integration above) ─────────────────

const MAX_REMINDER_SOURCES: usize = 20;
const MAX_REMINDER_AUDIT_ROWS: usize = 32;
const MAX_AUDIT_MARKDOWN_BYTES: usize = 256 * 1024;
const MAX_AUDIT_SEGMENT_BYTES: usize = 64 * 1024;
const MAX_AUDIT_SEGMENTS: usize = 128;
pub(crate) const REMINDER_SCHEDULER_TICK_SECS: u64 = 15;
const REMINDER_SOURCE_INVALIDATION_TICK_MS: u64 = 250;
const REMINDER_SOURCE_INVALIDATION_BATCH: usize = 128;
const REMINDER_RUNTIME_PROBE_TASK_ID: &str = "reminders-smart-inbox-v2-merge-20260731";
const REMINDER_RUNTIME_GATEWAY_URL: &str = "https://runtime-gateway.invalid";

/// The native Harness runtime smoke uses an isolated temporary HOME and exposes its task-private
/// runtime directory. The exact task id is part of the authorization so this feature-specific
/// regression never changes another Harness task, an ordinary `tauri dev`, or a release build.
pub(crate) fn reminder_runtime_probe_requested() -> bool {
    cfg!(debug_assertions)
        && std::env::var("MURMUR_HARNESS").as_deref() == Ok("1")
        && std::env::var("MURMUR_HARNESS_TASK").as_deref() == Ok(REMINDER_RUNTIME_PROBE_TASK_ID)
        && std::env::var_os("MURMUR_HARNESS_RUNTIME_DIR").is_some()
        && std::env::var_os("MURMUR_DEV_DEK").is_some()
}

/// Install the debug KEK and fake local Brain sidecar before Tauri starts any worker. The smoke
/// supplies an isolated HOME + SQLCipher DEK and an externally-owned loopback egress observer, so
/// none of this can read the operator's Keychain, models, database, or configuration.
pub(crate) fn prepare_reminder_runtime_probe_environment() {
    #[cfg(debug_assertions)]
    if reminder_runtime_probe_requested() {
        if std::env::var_os("MURMUR_DEV_KEK").is_none() {
            // SAFETY: `lib::run` calls this at process entry, before Tauri or any app worker starts.
            std::env::set_var(
                "MURMUR_DEV_KEK",
                "1111111111111111111111111111111111111111111111111111111111111111",
            );
        }
        runtime_probe::prepare_process_environment();
    }
}

#[cfg(debug_assertions)]
pub(crate) fn record_reminder_runtime_startup_reconcile(
    derived_before: usize,
    derived_after: usize,
) -> Result<(), AppError> {
    runtime_probe::record_startup_reconcile(derived_before, derived_after)
}

/// Fixed renderer orchestration for the exact Harness task. Product commands perform every
/// lifecycle transition; this script only sequences real `invoke` calls and reduces their
/// content-bearing responses to booleans before handing them to the native control probe.
pub(crate) fn reminder_runtime_probe_initialization_script() -> &'static str {
    r#"
void (async () => {
  const SOURCE_ID = "runtime-probe-meeting-5-20260729";
  const DUE_AT = 4102444800000;
  const TITLE = "Runtime reminder probe";
  const errorIsLocked = (error) => String(error ?? "").startsWith("locked: ");
  const detailIsMasked = (detail) =>
    detail?.locked === true &&
    detail?.note === null &&
    Array.isArray(detail?.segments) && detail.segments.length === 0 &&
    Array.isArray(detail?.assistantInteractions) && detail.assistantInteractions.length === 0 &&
    detail?.meeting?.title === "🔒 Locked" &&
    detail?.meeting?.audioPath == null &&
    detail?.aiProvider == null &&
    detail?.aiModel == null &&
    detail?.modelServed == null;
  const reminderSourcesAreMasked = (snapshot) => {
    const all = [
      ...(snapshot?.upcoming ?? []),
      ...(snapshot?.completed ?? []),
      ...(snapshot?.inbox ?? []).map((row) => row.reminder),
    ];
    const reminder = all.find((row) => row?.title === TITLE && row?.dueAt === DUE_AT);
    return !!reminder && Array.isArray(reminder.sources) && reminder.sources.length === 0;
  };
  let invoke = null;
  let control = null;
  let stage = "ipc-bridge";
  try {
    // Tauri injects the invoke protocol before this appended script, but installs the public
    // `invoke` helper in the following core script. Yield until that fixed bootstrap finishes.
    for (let attempt = 0; attempt < 1000; attempt += 1) {
      const candidate = window.__TAURI_INTERNALS__?.invoke;
      if (typeof candidate === "function") {
        const nativeInvoke = candidate;
        invoke = (command, args, options) => {
          if (command === "check_for_update") {
            return Promise.reject(new Error("runtime update check disabled"));
          }
          return nativeInvoke(command, args, options);
        };
        window.__TAURI_INTERNALS__.invoke = invoke;
        break;
      }
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
    if (!invoke) throw new Error("Tauri IPC bridge did not initialize");
    control = (action, extra = {}) =>
      invoke("reminder_runtime_probe_control", { input: { action, ...extra } });
    stage = "claim";
    const claim = await control("claim");
    if (!claim.owner) return;
    let phase = claim.phase;
    for (let attempt = 0; phase === "waiting" && attempt < 1000; attempt += 1) {
      await new Promise((resolve) => setTimeout(resolve, 10));
      phase = (await control("phase")).phase;
    }
    if (phase === "initial") {
      stage = "prepare";
      const fixture = await control("prepare");
      stage = "lock-unlock";
      await invoke("lock_folder", { folderId: fixture.folderId });
      await invoke("unlock_folder", { folderId: fixture.folderId });
      stage = "create-reminder";
      await invoke("create_reminder", {
        draft: {
          title: TITLE,
          details: "Synthetic runtime proof",
          dueAt: DUE_AT,
          repeatEvery: null,
          repeatUnit: null,
          sources: [{ kind: "meeting", id: SOURCE_ID }],
        },
      });

      stage = "provider-preflight";
      await invoke("set_anthropic_key", { key: "runtime-probe-anthropic-key" });
      await invoke("set_gateway_key", { key: "runtime-probe-gateway-key" });
      await invoke("consent_to_cloud_egress");
      await invoke("select_brain_model", { modelId: "qwen3-1.7b" });
      const providers = ["claude_code", "anthropic", "gateway", "ollama"];
      for (let index = 0; index < providers.length; index += 1) {
        const provider = providers[index];
        stage = `provider-${index}-config`;
        const config = await invoke("get_config");
        config.providerId = provider;
        config.providerModel = "runtime-probe-model";
        config.anthropicModel = "runtime-probe-model";
        config.gatewayModel = "runtime-probe-model";
        config.ollamaModel = "runtime-probe-model";
        config.gatewayBaseUrl = fixture.gatewayEndpoint;
        config.ollamaBaseUrl = "https://remote-ollama.invalid";
        config.claudeBinary = fixture.claudeBinary;
        config.roleNotesConnection = provider;
        config.roleAskConnection = provider;
        config.roleLiveConnection = provider;
        await invoke("save_config", { config });
        const readback = await invoke("get_config");
        if (
          readback.providerId !== provider ||
          readback.roleNotesConnection !== provider ||
          readback.roleAskConnection !== provider ||
          readback.roleLiveConnection !== provider
        ) throw new Error("provider config readback mismatch");
        stage = `provider-${index}-audit`;
        await control("begin-provider-case", { providerIndex: index });
        const rows = await invoke("audit_reminder_suggestions", {
          sourceKind: "meeting",
          sourceId: fixture.sourceIds[index],
        });
        if (!Array.isArray(rows) || rows.length === 0) {
          throw new Error("provider audit produced no deterministic candidate");
        }
        await control("finish-provider-case", { providerIndex: index });
      }

      stage = "stub-audit";
      await control("begin-stub-case");
      const stubRows = await invoke("audit_reminder_suggestions", {
        sourceKind: "meeting",
        sourceId: fixture.sourceIds[4],
      });
      if (!Array.isArray(stubRows) || stubRows.length === 0) {
        throw new Error("stub audit produced no deterministic candidate");
      }
      await control("finish-stub-case");

      stage = "race-arm";
      await control("arm-race");
      const inflight = invoke("audit_reminder_suggestions", {
        sourceKind: "meeting",
        sourceId: SOURCE_ID,
      }).then(
        () => ({ locked: false }),
        (error) => ({ locked: errorIsLocked(error) }),
      );
      let entered = false;
      for (let attempt = 0; attempt < 1000; attempt += 1) {
        if ((await control("race-entered")).entered) {
          entered = true;
          break;
        }
        await new Promise((resolve) => setTimeout(resolve, 10));
      }
      if (!entered) throw new Error("local inference did not enter");
      stage = "race-relock";
      await invoke("relock_all");
      await control("release-race");
      const raceResult = await inflight;

      stage = "phase-one-reads";
      const snapshot = await invoke("list_reminders");
      const detail = await invoke("get_meeting_detail", { meetingId: SOURCE_ID });
      let postLockAuditLocked = false;
      try {
        await invoke("audit_reminder_suggestions", {
          sourceKind: "meeting",
          sourceId: SOURCE_ID,
        });
      } catch (error) {
        postLockAuditLocked = errorIsLocked(error);
      }
      stage = "phase-one-finish";
      await control("finish-phase-one", {
        auditLocked: raceResult.locked,
        sourcesMasked: reminderSourcesAreMasked(snapshot),
        detailMasked: detailIsMasked(detail),
        postLockAuditLocked,
      });
      return;
    }

    if (phase !== "restart") throw new Error("restart phase was not established");
    stage = "restart-reads";
    const snapshot = await invoke("list_reminders");
    const detail = await invoke("get_meeting_detail", { meetingId: SOURCE_ID });
    let postLockAuditLocked = false;
    try {
      await invoke("audit_reminder_suggestions", {
        sourceKind: "meeting",
        sourceId: SOURCE_ID,
      });
    } catch (error) {
      postLockAuditLocked = errorIsLocked(error);
    }
    stage = "restart-finish";
    await control("finish-restart", {
      auditLocked: postLockAuditLocked,
      sourcesMasked: reminderSourcesAreMasked(snapshot),
      detailMasked: detailIsMasked(detail),
      postLockAuditLocked,
    });
  } catch (_) {
    if (control) await control("fail", { stage }).catch(() => {});
  }
})();"#
}

/// Product update command with one debug-only guard for the exact Harness runtime probe.
///
/// The renderer also suppresses its best-effort startup check, but Angular and the appended
/// initialization script can race during a real WebView boot. Blocking at the IPC boundary keeps
/// the no-egress proof deterministic without changing release behavior.
#[tauri::command(rename = "check_for_update")]
pub async fn check_for_update_guarded() -> Result<crate::update::UpdateInfo, AppError> {
    #[cfg(debug_assertions)]
    if reminder_runtime_probe_requested() {
        return Err(AppError::Unavailable(
            "Update checks are disabled during the isolated runtime probe.".into(),
        ));
    }

    crate::update::check_for_update().await
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReminderRuntimeProbeInput {
    action: String,
    #[serde(default)]
    provider_index: Option<usize>,
    #[serde(default)]
    audit_locked: bool,
    #[serde(default)]
    sources_masked: bool,
    #[serde(default)]
    detail_masked: bool,
    #[serde(default)]
    post_lock_audit_locked: bool,
    #[serde(default)]
    stage: Option<String>,
}

/// Test-control command for the exact debug Harness task. All content-bearing operations remain
/// ordinary product IPC commands; this endpoint exposes only fixture ids, counters, booleans, and
/// the final task-private witness.
#[tauri::command]
pub fn reminder_runtime_probe_control(
    app: AppHandle,
    state: State<'_, AppState>,
    input: ReminderRuntimeProbeInput,
) -> Result<serde_json::Value, AppError> {
    #[cfg(debug_assertions)]
    {
        runtime_probe::control(&app, state.inner(), input)
    }
    #[cfg(not(debug_assertions))]
    {
        let _ = (app, state, input);
        Err(AppError::InvalidArg(
            "runtime reminder probe is unavailable".into(),
        ))
    }
}

#[cfg(debug_assertions)]
mod runtime_probe {
    use std::collections::BTreeSet;
    use std::fs::{File, OpenOptions};
    use std::io::{ErrorKind, Read, Write};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Mutex, OnceLock};
    use std::time::Duration;

    use serde::{Deserialize, Serialize};
    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};
    use tauri::Manager;
    use zeroize::Zeroizing;

    use super::*;
    use crate::settings::AppConfig;
    use crate::storage::models::{Folder, Meeting, MeetingStatus, NoteRecord};

    const FOLDER_ID: &str = "runtime-probe-folder-20260729";
    const SOURCE_PREFIX: &str = "runtime-probe-meeting";
    const SOURCE_COUNT: usize = 6;
    const PROVIDER_COUNT: usize = 4;
    const LOCAL_MODEL_ID: &str = "qwen3-1.7b";
    const REMINDER_DUE_AT: i64 = 4_102_444_800_000;
    const REMINDER_TITLE: &str = "Runtime reminder probe";
    const CHALLENGE_FILE: &str = "reminder-runtime-challenge.json";
    const INTENT_FILE: &str = "reminder-runtime-restart-intent.json";
    const GENERATION_FILE: &str = "reminder-runtime-restart-generation.json";
    const WITNESS_FILE: &str = "reminder-runtime-witness.json";
    const MAX_RECEIPT_BYTES: u64 = 64 * 1024;

    static PROCESS_CLAIMED: AtomicBool = AtomicBool::new(false);
    static SETUP_FAILED: AtomicBool = AtomicBool::new(false);
    static SELECTOR_SUCCESSES: AtomicUsize = AtomicUsize::new(0);
    static LOCKED_AUDIT_OUTCOMES: AtomicUsize = AtomicUsize::new(0);
    static STARTUP_DERIVED_BEFORE: AtomicUsize = AtomicUsize::new(usize::MAX);
    static STARTUP_DERIVED_AFTER: AtomicUsize = AtomicUsize::new(usize::MAX);
    static RUNTIME: OnceLock<Mutex<RuntimeState>> = OnceLock::new();

    struct RuntimeState {
        proxy_endpoint: String,
        original_config: Option<AppConfig>,
        provider_cases: BTreeSet<usize>,
        stub_passed: bool,
        active_case: Option<(usize, ObservationWindow)>,
        race_window: Option<ObservationWindow>,
    }

    #[derive(Clone)]
    struct ObservationWindow {
        egress_before: i64,
        brain_before: usize,
        selector_before: usize,
        seal_epoch_before: u64,
        race_nonce: Option<String>,
    }

    #[derive(Debug, Clone)]
    struct ProbePaths {
        runtime_dir: PathBuf,
        challenge: PathBuf,
        intent: PathBuf,
        generation: PathBuf,
        witness: PathBuf,
        sidecar_script: PathBuf,
        claude_script: PathBuf,
        dummy_model: PathBuf,
        brain_count: PathBuf,
        race_armed: PathBuf,
        race_started: PathBuf,
        race_release: PathBuf,
        race_done: PathBuf,
        claude_called: PathBuf,
    }

    #[derive(Debug, Clone, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct RuntimeChallenge {
        schema_version: u8,
        task_id: String,
        run_id: String,
        runner_pid: u32,
        runner_nonce: String,
        binary_sha256: String,
        network_profile_sha256: String,
        runner_source_sha256: String,
        task_contract_sha256: String,
        git_head: String,
        started_at: String,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct RestartIntent {
        schema_version: u8,
        run_id: String,
        first_pid: u32,
        phase_one_passed: bool,
        runner_nonce: String,
        binary_sha256: String,
        selector_successes: usize,
        locked_audit_outcomes: usize,
        restart_canary_rows: usize,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct RestartGeneration {
        schema_version: u8,
        run_id: String,
        runner_nonce: String,
        binary_sha256: String,
        first_pid: u32,
        second_pid: u32,
        runner_owned: bool,
    }

    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct RuntimeWitness {
        schema_version: u8,
        verdict: &'static str,
        run_id: String,
        runner_nonce: String,
        phase_two_nonce: String,
        binary_sha256: String,
        network_profile_sha256: String,
        runner_source_sha256: String,
        task_contract_sha256: String,
        git_head: String,
        first_pid: u32,
        second_pid: u32,
        checks: RuntimeWitnessChecks,
    }

    #[derive(Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    struct RuntimeWitnessChecks {
        actual_tauri_ipc: bool,
        sqlcipher_integrity: bool,
        plain_sqlite_rejected: bool,
        lock_unlock_relock: bool,
        in_flight_local_inference: bool,
        masked_reads: bool,
        restart_purge: bool,
        no_egress: bool,
        provider_cases: usize,
        local_generate_count: usize,
        stub_case: bool,
    }

    #[derive(Debug, Serialize, Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct SidecarRaceEvent {
        runner_nonce: String,
        race_nonce: String,
        request_id: u64,
    }

    enum ProbePhase {
        Initial,
        Waiting,
        Restart(RestartGeneration),
    }

    pub(super) fn prepare_process_environment() {
        if let Err(_error) = prepare_process_environment_inner() {
            SETUP_FAILED.store(true, Ordering::Release);
        }
    }

    fn prepare_process_environment_inner() -> Result<(), AppError> {
        verify_isolated_runtime()?;
        let paths = ProbePaths::new()?;
        let challenge = probe_challenge(&paths)?;
        if current_executable_sha256()? != challenge.binary_sha256 {
            return Err(AppError::Unavailable(
                "running executable does not match the runner challenge".into(),
            ));
        }
        configure_witness_fd()?;
        write_probe_executables(&paths)?;
        ensure_dummy_model(&paths)?;
        let endpoint = std::env::var("MURMUR_HARNESS_EGRESS_PROXY")
            .map_err(|_| AppError::Unavailable("runner egress observer is absent".into()))?;
        validate_proxy_endpoint(&endpoint)?;
        if std::env::var("MURMUR_HARNESS_RUN_ID").as_deref() != Ok(challenge.run_id.as_str())
            || std::env::var("MURMUR_HARNESS_RUNNER_NONCE").as_deref()
                != Ok(challenge.runner_nonce.as_str())
            || std::env::var("MURMUR_HARNESS_BINARY_SHA256").as_deref()
                != Ok(challenge.binary_sha256.as_str())
        {
            return Err(AppError::Unavailable(
                "runner challenge environment is not bound".into(),
            ));
        }
        // SAFETY: called at process entry before Tauri starts any worker.
        for key in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            std::env::set_var(key, &endpoint);
        }
        std::env::set_var("NO_PROXY", "");
        std::env::set_var("no_proxy", "");
        std::env::set_var("MURMUR_BRAIN_SIDECAR", &paths.sidecar_script);
        RUNTIME
            .set(Mutex::new(RuntimeState {
                proxy_endpoint: endpoint,
                original_config: None,
                provider_cases: BTreeSet::new(),
                stub_passed: false,
                active_case: None,
                race_window: None,
            }))
            .map_err(|_| AppError::Unavailable("runtime probe initialized twice".into()))?;
        Ok(())
    }

    fn validate_proxy_endpoint(endpoint: &str) -> Result<(), AppError> {
        let parsed = reqwest::Url::parse(endpoint).map_err(|_| {
            AppError::Unavailable("runner egress observer endpoint is invalid".into())
        })?;
        let valid = endpoint.len() <= 160
            && parsed.scheme() == "http"
            && parsed.host_str() == Some("127.0.0.1")
            && parsed.port().is_some()
            && parsed.username() == "murmur-harness"
            && parsed
                .password()
                .is_some_and(|password| valid_lowercase_hex(password, 64))
            && parsed.path() == "/"
            && parsed.query().is_none()
            && parsed.fragment().is_none();
        if valid {
            Ok(())
        } else {
            Err(AppError::Unavailable(
                "runner egress observer endpoint is invalid".into(),
            ))
        }
    }

    pub(super) fn control(
        app: &AppHandle,
        state: &AppState,
        input: ReminderRuntimeProbeInput,
    ) -> Result<Value, AppError> {
        if !super::reminder_runtime_probe_requested() {
            return Err(AppError::InvalidArg(
                "runtime reminder probe is not authorized".into(),
            ));
        }
        if SETUP_FAILED.load(Ordering::Acquire) {
            return Err(AppError::Unavailable(
                "runtime reminder probe setup failed".into(),
            ));
        }
        let paths = ProbePaths::new()?;
        match input.action.as_str() {
            "claim" => claim(&paths),
            "phase" => Ok(json!({"phase": phase_name(&probe_phase(&paths)?)})),
            "prepare" => prepare_fixture(state, &paths),
            "begin-provider-case" => begin_provider_case(
                state,
                &paths,
                required_provider_index(input.provider_index)?,
            ),
            "finish-provider-case" => finish_provider_case(
                state,
                &paths,
                required_provider_index(input.provider_index)?,
            ),
            "begin-stub-case" => begin_stub_case(state, &paths),
            "finish-stub-case" => finish_stub_case(state, &paths),
            "arm-race" => arm_race(state, &paths),
            "race-entered" => race_entered(&paths),
            "release-race" => release_race(state, &paths),
            "finish-phase-one" => finish_phase_one(app, state, &paths, &input),
            "finish-restart" => finish_restart(app, state, &paths, &input),
            "fail" => fail_probe(app, input.stage.as_deref()),
            _ => Err(AppError::InvalidArg(
                "unknown runtime reminder probe action".into(),
            )),
        }
    }

    fn claim(paths: &ProbePaths) -> Result<Value, AppError> {
        if PROCESS_CLAIMED.swap(true, Ordering::AcqRel) {
            return Ok(json!({"owner": false, "phase": "duplicate"}));
        }
        let phase = probe_phase(paths)?;
        Ok(json!({"owner": true, "phase": phase_name(&phase)}))
    }

    fn phase_name(phase: &ProbePhase) -> &'static str {
        match phase {
            ProbePhase::Initial => "initial",
            ProbePhase::Waiting => "waiting",
            ProbePhase::Restart(_) => "restart",
        }
    }

    fn probe_phase(paths: &ProbePaths) -> Result<ProbePhase, AppError> {
        let challenge = probe_challenge(paths)?;
        if !paths.intent.is_file() {
            return Ok(ProbePhase::Initial);
        }
        let intent: RestartIntent = read_json(&paths.intent)?;
        if intent.schema_version != 1
            || intent.run_id != challenge.run_id
            || intent.runner_nonce != challenge.runner_nonce
            || intent.binary_sha256 != challenge.binary_sha256
            || !intent.phase_one_passed
            || intent.first_pid == 0
            || intent.selector_successes != PROVIDER_COUNT + 1
            || intent.locked_audit_outcomes != 2
            || intent.restart_canary_rows != 2
        {
            return Err(AppError::Unavailable(
                "runtime restart intent is stale or invalid".into(),
            ));
        }
        if !paths.generation.is_file() {
            return Ok(ProbePhase::Waiting);
        }
        let generation: RestartGeneration = read_json(&paths.generation)?;
        if generation.schema_version != 1
            || generation.run_id != intent.run_id
            || generation.runner_nonce != challenge.runner_nonce
            || generation.binary_sha256 != challenge.binary_sha256
            || !std::env::var("MURMUR_HARNESS_PHASE_TWO_NONCE")
                .is_ok_and(|value| valid_lowercase_hex(&value, 64))
            || generation.first_pid != intent.first_pid
            || generation.second_pid == generation.first_pid
            || generation.second_pid != std::process::id()
            || !generation.runner_owned
        {
            return Err(AppError::Unavailable(
                "runtime restart generation is invalid".into(),
            ));
        }
        Ok(ProbePhase::Restart(generation))
    }

    fn prepare_fixture(state: &AppState, paths: &ProbePaths) -> Result<Value, AppError> {
        if !matches!(probe_phase(paths)?, ProbePhase::Initial) {
            return Err(AppError::Unavailable(
                "runtime fixture can only be prepared before restart".into(),
            ));
        }
        for path in [
            &paths.witness,
            &paths.brain_count,
            &paths.race_armed,
            &paths.race_started,
            &paths.race_release,
            &paths.race_done,
            &paths.claude_called,
        ] {
            if path.exists() {
                return Err(AppError::Unavailable(
                    "runtime fixture contains stale proof artifacts".into(),
                ));
            }
        }
        ensure_dummy_model(paths)?;
        seed_sources(state)?;

        let mut runtime = runtime()?;
        if runtime.original_config.is_some() {
            return Err(AppError::Unavailable(
                "runtime fixture was prepared twice".into(),
            ));
        }
        runtime.original_config = Some(
            state
                .config
                .lock()
                .map_err(|_| AppError::Unavailable("runtime config is unavailable".into()))?
                .clone(),
        );
        if derived_count(state)? != 0 || egress_count(state)? != 0 {
            return Err(AppError::Unavailable(
                "runtime fixture did not start from an empty proof domain".into(),
            ));
        }
        let source_ids = (0..SOURCE_COUNT).map(source_id).collect::<Vec<_>>();
        Ok(json!({
            "folderId": FOLDER_ID,
            "sourceIds": source_ids,
            "dueAt": REMINDER_DUE_AT,
            "providerCount": PROVIDER_COUNT,
            "proxyEndpoint": runtime.proxy_endpoint.clone(),
            "gatewayEndpoint": REMINDER_RUNTIME_GATEWAY_URL,
            "claudeBinary": paths.claude_script,
        }))
    }

    fn begin_provider_case(
        state: &AppState,
        paths: &ProbePaths,
        index: usize,
    ) -> Result<Value, AppError> {
        let expected = provider_name(index)?;
        let mut runtime = runtime()?;
        if runtime.provider_cases.len() != index
            || runtime.provider_cases.contains(&index)
            || runtime.active_case.is_some()
        {
            return Err(AppError::Unavailable(
                "runtime provider case order is invalid".into(),
            ));
        }
        let config = state
            .config
            .lock()
            .map_err(|_| AppError::Unavailable("runtime config is unavailable".into()))?
            .clone();
        if config.provider_id != expected
            || config.role_notes_connection != expected
            || config.role_ask_connection != expected
            || config.role_live_connection != expected
            || !config.cloud_egress_consented
            || config.claude_binary != paths.claude_script.to_string_lossy()
            || config.gateway_base_url.as_str() != REMINDER_RUNTIME_GATEWAY_URL
            || config.ollama_base_url != "https://remote-ollama.invalid"
            || config.brain_light_model_id.as_deref() != Some(LOCAL_MODEL_ID)
        {
            return Err(AppError::Unavailable(
                "runtime provider configuration is not dispatch-capable".into(),
            ));
        }
        if crate::secrets::get_secret(crate::summarize::ANTHROPIC_KEY_ACCOUNT)?.is_none()
            || crate::secrets::get_secret(crate::summarize::GATEWAY_KEY_ACCOUNT)?.is_none()
        {
            return Err(AppError::Unavailable(
                "runtime provider credentials are absent".into(),
            ));
        }
        let engine = state.reasoner.light();
        if !engine.id().starts_with("sidecar:") || source_derived_count(state, index)? != 0 {
            return Err(AppError::Unavailable(
                "runtime local provider audit is not an uncached sidecar call".into(),
            ));
        }
        runtime.active_case = Some((index, begin_window(state, paths)?));
        Ok(json!({"ready": true}))
    }

    fn finish_provider_case(
        state: &AppState,
        paths: &ProbePaths,
        index: usize,
    ) -> Result<Value, AppError> {
        let mut runtime = runtime()?;
        let Some((active_index, window)) = runtime.active_case.take() else {
            return Err(AppError::Unavailable(
                "runtime provider case was not started".into(),
            ));
        };
        if active_index != index {
            return Err(AppError::Unavailable(
                "runtime provider case changed identity".into(),
            ));
        }
        finish_window(state, paths, &window, 1)?;
        assert_source_engine(state, index, |engine| engine.starts_with("sidecar:"))?;
        runtime.provider_cases.insert(index);
        Ok(json!({"passed": true}))
    }

    fn begin_stub_case(state: &AppState, paths: &ProbePaths) -> Result<Value, AppError> {
        let mut runtime = runtime()?;
        if runtime.provider_cases.len() != PROVIDER_COUNT
            || runtime.stub_passed
            || runtime.active_case.is_some()
        {
            return Err(AppError::Unavailable(
                "runtime stub case precondition failed".into(),
            ));
        }
        remove_file_if_present(&paths.dummy_model)?;
        if state.reasoner.light().id() != "stub"
            || source_derived_count(state, PROVIDER_COUNT)? != 0
        {
            return Err(AppError::Unavailable(
                "runtime missing local model did not select the stub".into(),
            ));
        }
        runtime.active_case = Some((PROVIDER_COUNT, begin_window(state, paths)?));
        Ok(json!({"ready": true}))
    }

    fn finish_stub_case(state: &AppState, paths: &ProbePaths) -> Result<Value, AppError> {
        let mut runtime = runtime()?;
        let Some((index, window)) = runtime.active_case.take() else {
            return Err(AppError::Unavailable(
                "runtime stub case was not started".into(),
            ));
        };
        if index != PROVIDER_COUNT {
            return Err(AppError::Unavailable(
                "runtime stub case changed identity".into(),
            ));
        }
        finish_window(state, paths, &window, 0)?;
        assert_source_engine(state, PROVIDER_COUNT, |engine| engine == "stub")?;
        ensure_dummy_model(paths)?;
        runtime.stub_passed = true;
        Ok(json!({"passed": true}))
    }

    fn arm_race(state: &AppState, paths: &ProbePaths) -> Result<Value, AppError> {
        let mut runtime = runtime()?;
        if runtime.provider_cases.len() != PROVIDER_COUNT
            || !runtime.stub_passed
            || runtime.race_window.is_some()
            || source_derived_count(state, SOURCE_COUNT - 1)? != 0
            || derived_count(state)? == 0
        {
            return Err(AppError::Unavailable(
                "runtime inference race precondition failed".into(),
            ));
        }
        ensure_dummy_model(paths)?;
        if !state.reasoner.light().id().starts_with("sidecar:") {
            return Err(AppError::Unavailable(
                "runtime inference race is not using the local sidecar".into(),
            ));
        }
        for path in [
            &paths.race_armed,
            &paths.race_started,
            &paths.race_release,
            &paths.race_done,
        ] {
            if path.exists() {
                return Err(AppError::Unavailable(
                    "runtime inference race contains stale markers".into(),
                ));
            }
        }
        let challenge = probe_challenge(paths)?;
        let race_nonce = uuid::Uuid::new_v4().simple().to_string();
        let mut window = begin_window(state, paths)?;
        window.race_nonce = Some(race_nonce.clone());
        write_json_atomic(
            &paths.race_armed,
            &SidecarRaceEvent {
                runner_nonce: challenge.runner_nonce,
                race_nonce,
                request_id: 0,
            },
        )?;
        runtime.race_window = Some(window);
        Ok(json!({"armed": true}))
    }

    fn race_entered(paths: &ProbePaths) -> Result<Value, AppError> {
        let runtime = runtime()?;
        let Some(window) = runtime.race_window.as_ref() else {
            return Err(AppError::Unavailable(
                "runtime inference race was not armed".into(),
            ));
        };
        let expected_nonce = window
            .race_nonce
            .as_deref()
            .ok_or_else(|| AppError::Unavailable("runtime race nonce is absent".into()))?;
        let entered = if paths.race_started.is_file() {
            let event: SidecarRaceEvent = read_json(&paths.race_started)?;
            let challenge = probe_challenge(paths)?;
            event.runner_nonce == challenge.runner_nonce
                && event.race_nonce == expected_nonce
                && event.request_id > 0
                && brain_generate_count(paths)? == window.brain_before.saturating_add(1)
        } else {
            false
        };
        Ok(json!({"entered": entered}))
    }

    fn release_race(state: &AppState, paths: &ProbePaths) -> Result<Value, AppError> {
        if !paths.race_started.is_file() {
            return Err(AppError::Unavailable(
                "runtime local inference has not started".into(),
            ));
        }
        let runtime = runtime()?;
        let window = runtime
            .race_window
            .as_ref()
            .ok_or_else(|| AppError::Unavailable("runtime inference race was not armed".into()))?;
        let expected_nonce = window
            .race_nonce
            .as_deref()
            .ok_or_else(|| AppError::Unavailable("runtime race nonce is absent".into()))?;
        let event: SidecarRaceEvent = read_json(&paths.race_started)?;
        let challenge = probe_challenge(paths)?;
        if event.runner_nonce != challenge.runner_nonce
            || event.race_nonce != expected_nonce
            || event.request_id == 0
            || state.seal_epoch.load(Ordering::SeqCst) <= window.seal_epoch_before
            || !folder_is_relocked(state)?
            || crate::commands::locked_folder_requires_authenticated_repair(&state.db, FOLDER_ID)?
            || !relocked_content_is_recoverable(state)?
        {
            return Err(AppError::Unavailable(
                "runtime relock did not land between local Generate start and release".into(),
            ));
        }
        write_json_atomic(&paths.race_release, &event)?;
        Ok(json!({"released": true}))
    }

    fn finish_phase_one(
        _app: &AppHandle,
        state: &AppState,
        paths: &ProbePaths,
        input: &ReminderRuntimeProbeInput,
    ) -> Result<Value, AppError> {
        require_ipc_assertions(input)?;
        let mut runtime = runtime()?;
        let window = runtime.race_window.take().ok_or_else(|| {
            AppError::Unavailable(
                "runtime inference race did not have an observation window".into(),
            )
        })?;
        finish_window(state, paths, &window, 1)?;
        let race_started: SidecarRaceEvent = read_json(&paths.race_started)?;
        let race_done: SidecarRaceEvent = read_json(&paths.race_done)?;
        let challenge = probe_challenge(paths)?;
        if derived_count(state)? != 0
            || source_derived_count(state, SOURCE_COUNT - 1)? != 0
            || brain_generate_count(paths)? != PROVIDER_COUNT + 1
            || SELECTOR_SUCCESSES.load(Ordering::Acquire) != PROVIDER_COUNT + 1
            || LOCKED_AUDIT_OUTCOMES.load(Ordering::Acquire) != 2
            || race_done.runner_nonce != challenge.runner_nonce
            || Some(race_done.race_nonce.as_str()) != window.race_nonce.as_deref()
            || race_done.request_id == 0
            || race_done.runner_nonce != race_started.runner_nonce
            || race_done.race_nonce != race_started.race_nonce
            || race_done.request_id != race_started.request_id
            || !folder_is_relocked(state)?
            || crate::commands::locked_folder_requires_authenticated_repair(&state.db, FOLDER_ID)?
            || !relocked_content_is_recoverable(state)?
            || !reminder_is_persisted_and_masked(state)?
        {
            return Err(AppError::Unavailable(
                "runtime phase-one privacy invariant failed".into(),
            ));
        }
        let (cipher_integrity, plain_rejected) = prove_sqlcipher(state)?;
        if !cipher_integrity || !plain_rejected {
            return Err(AppError::Unavailable(
                "runtime SQLCipher proof failed".into(),
            ));
        }

        let original = runtime.original_config.take().ok_or_else(|| {
            AppError::Unavailable("runtime original configuration is unavailable".into())
        })?;
        let _lifecycle = crate::commands::lifecycle_guard(state);
        let current_projection = state
            .config
            .lock()
            .map_err(|_| AppError::Unavailable("runtime config is unavailable".into()))
            .map(|config| crate::commands::ask_dispatch_projection(&config))?;
        if current_projection != crate::commands::ask_dispatch_projection(&original) {
            state.db.advance_ask_dispatch_generation()?;
        }
        original.save(&state.db)?;
        *state
            .config
            .lock()
            .map_err(|_| AppError::Unavailable("runtime config is unavailable".into()))? = original;
        // The debug runtime fixture also changes provider credentials after restoring config.
        // Rotate before either fallible deletion; partial cleanup may over-invalidate but cannot
        // leave an old authorization live against a changed Keychain.
        state.db.advance_ask_dispatch_generation()?;
        crate::secrets::delete_secret(crate::summarize::ANTHROPIC_KEY_ACCOUNT)?;
        crate::secrets::delete_secret(crate::summarize::GATEWAY_KEY_ACCOUNT)?;
        drop(runtime);

        // Simulate the exact crash window startup reconciliation protects: the folder remains
        // sealed and session-hidden, but two synthetic source-derived rows reach disk after the
        // clean relock transaction. Process two must observe both rows before startup reblank and
        // observe zero afterwards. No user content is copied into this isolated debug-only canary.
        seed_restart_purge_canary(state)?;
        if derived_count(state)? != 2 {
            return Err(AppError::Unavailable(
                "runtime restart purge canary was not persisted".into(),
            ));
        }
        let intent = RestartIntent {
            schema_version: 1,
            run_id: challenge.run_id,
            first_pid: std::process::id(),
            phase_one_passed: true,
            runner_nonce: challenge.runner_nonce,
            binary_sha256: challenge.binary_sha256,
            selector_successes: SELECTOR_SUCCESSES.load(Ordering::Acquire),
            locked_audit_outcomes: LOCKED_AUDIT_OUTCOMES.load(Ordering::Acquire),
            restart_canary_rows: 2,
        };
        write_json_atomic(&paths.intent, &intent)?;
        tracing::info!(
            target: "reminders",
            provider_cases = PROVIDER_COUNT,
            local_generates = PROVIDER_COUNT + 1,
            "native reminder runtime probe phase one passed; requesting runner restart"
        );
        // Deliberately bypass Tauri's normal ExitRequested lifecycle hook: that hook calls
        // `relock_all` and would purge this canary in process one, turning the claimed startup
        // crash-recovery proof into a false positive. This debug-only, exact-task probe owns a
        // synthetic isolated HOME; the runner observes this exact PID exit, drains its process
        // group, then starts the same binary so process two must perform the 2 -> 0 startup purge.
        std::process::exit(0)
    }

    fn finish_restart(
        app: &AppHandle,
        state: &AppState,
        paths: &ProbePaths,
        input: &ReminderRuntimeProbeInput,
    ) -> Result<Value, AppError> {
        require_ipc_assertions(input)?;
        let challenge = probe_challenge(paths)?;
        let generation = match probe_phase(paths)? {
            ProbePhase::Restart(generation) => generation,
            _ => {
                return Err(AppError::Unavailable(
                    "runtime second process lacks a runner generation".into(),
                ));
            }
        };
        let intent: RestartIntent = read_json(&paths.intent)?;
        if derived_count(state)? != 0
            || source_derived_count(state, SOURCE_COUNT - 1)? != 0
            || !folder_is_relocked(state)?
            || crate::commands::locked_folder_requires_authenticated_repair(&state.db, FOLDER_ID)?
            || !relocked_content_is_recoverable(state)?
            || !reminder_is_persisted_and_masked(state)?
            || brain_generate_count(paths)? != PROVIDER_COUNT + 1
            || intent.selector_successes != PROVIDER_COUNT + 1
            || intent.locked_audit_outcomes != 2
            || intent.restart_canary_rows != 2
            || SELECTOR_SUCCESSES.load(Ordering::Acquire) != 0
            || LOCKED_AUDIT_OUTCOMES.load(Ordering::Acquire) != 1
            || STARTUP_DERIVED_BEFORE.load(Ordering::Acquire) != 2
            || STARTUP_DERIVED_AFTER.load(Ordering::Acquire) != 0
            || egress_count(state)? != 0
            || paths.claude_called.exists()
        {
            return Err(AppError::Unavailable(
                "runtime post-restart privacy invariant failed".into(),
            ));
        }
        let (cipher_integrity, plain_rejected) = prove_sqlcipher(state)?;
        if !cipher_integrity || !plain_rejected {
            return Err(AppError::Unavailable(
                "runtime post-restart SQLCipher proof failed".into(),
            ));
        }
        let phase_two_nonce = std::env::var("MURMUR_HARNESS_PHASE_TWO_NONCE")
            .map_err(|_| AppError::Unavailable("runtime phase-two nonce is absent".into()))?;
        if !valid_lowercase_hex(&phase_two_nonce, 64) {
            return Err(AppError::Unavailable(
                "runtime phase-two nonce is invalid".into(),
            ));
        }
        let witness = RuntimeWitness {
            schema_version: 1,
            verdict: "PASS",
            run_id: generation.run_id,
            runner_nonce: generation.runner_nonce,
            phase_two_nonce,
            binary_sha256: generation.binary_sha256,
            network_profile_sha256: challenge.network_profile_sha256,
            runner_source_sha256: challenge.runner_source_sha256,
            task_contract_sha256: challenge.task_contract_sha256,
            git_head: challenge.git_head,
            first_pid: generation.first_pid,
            second_pid: generation.second_pid,
            checks: RuntimeWitnessChecks {
                actual_tauri_ipc: true,
                sqlcipher_integrity: true,
                plain_sqlite_rejected: true,
                lock_unlock_relock: true,
                in_flight_local_inference: true,
                masked_reads: true,
                restart_purge: true,
                no_egress: true,
                provider_cases: PROVIDER_COUNT,
                local_generate_count: PROVIDER_COUNT + 1,
                stub_case: true,
            },
        };
        write_witness_pipe(&witness)?;
        start_mcp_after_probe(app);
        tracing::info!(
            target: "reminders",
            provider_cases = PROVIDER_COUNT,
            local_generates = PROVIDER_COUNT + 1,
            "native reminder runtime privacy probe passed after runner-owned restart"
        );
        Ok(json!({"passed": true}))
    }

    fn fail_probe(app: &AppHandle, stage: Option<&str>) -> Result<Value, AppError> {
        let stage = stage.filter(|candidate| allowed_failure_stage(candidate));
        tracing::error!(
            target: "reminders",
            stage = stage.unwrap_or("unknown"),
            "native reminder runtime privacy probe failed"
        );
        app.exit(70);
        Err(AppError::Unavailable(
            "runtime reminder probe failed".into(),
        ))
    }

    fn allowed_failure_stage(stage: &str) -> bool {
        matches!(
            stage,
            "claim"
                | "prepare"
                | "lock-unlock"
                | "create-reminder"
                | "provider-preflight"
                | "stub-audit"
                | "race-arm"
                | "race-relock"
                | "phase-one-reads"
                | "phase-one-finish"
                | "restart-reads"
                | "restart-finish"
        ) || (stage.starts_with("provider-")
            && (stage.ends_with("-config") || stage.ends_with("-audit"))
            && stage.len() <= 32)
    }

    fn require_ipc_assertions(input: &ReminderRuntimeProbeInput) -> Result<(), AppError> {
        if input.audit_locked
            && input.sources_masked
            && input.detail_masked
            && input.post_lock_audit_locked
        {
            Ok(())
        } else {
            Err(AppError::Unavailable(
                "runtime renderer IPC assertion failed".into(),
            ))
        }
    }

    fn required_provider_index(index: Option<usize>) -> Result<usize, AppError> {
        index.ok_or_else(|| AppError::InvalidArg("runtime provider index is required".into()))
    }

    fn provider_name(index: usize) -> Result<&'static str, AppError> {
        ["claude_code", "anthropic", "gateway", "ollama"]
            .get(index)
            .copied()
            .ok_or_else(|| AppError::InvalidArg("runtime provider index is invalid".into()))
    }

    fn begin_window(state: &AppState, paths: &ProbePaths) -> Result<ObservationWindow, AppError> {
        remove_file_if_present(&paths.claude_called)?;
        Ok(ObservationWindow {
            egress_before: egress_count(state)?,
            brain_before: brain_generate_count(paths)?,
            selector_before: SELECTOR_SUCCESSES.load(Ordering::Acquire),
            seal_epoch_before: state.seal_epoch.load(Ordering::SeqCst),
            race_nonce: None,
        })
    }

    fn finish_window(
        state: &AppState,
        paths: &ProbePaths,
        window: &ObservationWindow,
        expected_brain_increment: usize,
    ) -> Result<(), AppError> {
        std::thread::sleep(Duration::from_millis(50));
        if paths.claude_called.exists()
            || egress_count(state)? != window.egress_before
            || brain_generate_count(paths)?
                != window.brain_before.saturating_add(expected_brain_increment)
            || SELECTOR_SUCCESSES.load(Ordering::Acquire)
                != window
                    .selector_before
                    .saturating_add(expected_brain_increment)
        {
            return Err(AppError::Unavailable(
                "runtime audit attempted a non-local transport".into(),
            ));
        }
        Ok(())
    }

    fn assert_source_engine(
        state: &AppState,
        source_index: usize,
        expected: impl FnOnce(&str) -> bool,
    ) -> Result<(), AppError> {
        let id = source_id(source_index);
        let conn = state.db.lock();
        let engine: String = conn
            .query_row(
                "SELECT engine_id FROM reminder_audit_cache
                  WHERE source_kind='meeting' AND source_id=?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .map_err(crate::storage::db::map_err)?;
        let pending: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM reminder_pending_suggestions
                  WHERE source_kind='meeting' AND source_id=?1",
                rusqlite::params![source_id(source_index)],
                |row| row.get(0),
            )
            .map_err(crate::storage::db::map_err)?;
        if expected(&engine) && pending > 0 {
            Ok(())
        } else {
            Err(AppError::Unavailable(
                "runtime audit engine evidence is invalid".into(),
            ))
        }
    }

    fn source_derived_count(state: &AppState, source_index: usize) -> Result<i64, AppError> {
        let id = source_id(source_index);
        state
            .db
            .lock()
            .query_row(
                "SELECT
                   (SELECT COUNT(*) FROM reminder_audit_cache
                     WHERE source_kind='meeting' AND source_id=?1) +
                   (SELECT COUNT(*) FROM reminder_pending_suggestions
                     WHERE source_kind='meeting' AND source_id=?1)",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .map_err(crate::storage::db::map_err)
    }

    fn derived_count(state: &AppState) -> Result<i64, AppError> {
        state
            .db
            .lock()
            .query_row(
                "SELECT
                   (SELECT COUNT(*) FROM reminder_audit_cache) +
                   (SELECT COUNT(*) FROM reminder_pending_suggestions)",
                [],
                |row| row.get(0),
            )
            .map_err(crate::storage::db::map_err)
    }

    fn seed_restart_purge_canary(state: &AppState) -> Result<(), AppError> {
        const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const KEY: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let mut connection = state.db.lock();
        let transaction = connection
            .transaction()
            .map_err(crate::storage::db::map_err)?;
        transaction
            .execute(
                "INSERT INTO reminder_audit_cache
                   (source_kind,source_id,content_hash,engine_id,audited_at)
                 VALUES ('meeting',?1,?2,'runtime-restart-canary',1)",
                rusqlite::params![source_id(SOURCE_COUNT - 1), HASH],
            )
            .map_err(crate::storage::db::map_err)?;
        transaction
            .execute(
                "INSERT INTO reminder_pending_suggestions
                   (id,source_kind,source_id,content_hash,engine_id,candidate_key,title,
                    suggested_due_at,created_at)
                 VALUES ('runtime-restart-canary','meeting',?1,?2,
                         'runtime-restart-canary',?3,'Synthetic restart canary',NULL,1)",
                rusqlite::params![source_id(SOURCE_COUNT - 1), HASH, KEY],
            )
            .map_err(crate::storage::db::map_err)?;
        transaction.commit().map_err(crate::storage::db::map_err)
    }

    fn egress_count(state: &AppState) -> Result<i64, AppError> {
        state
            .db
            .lock()
            .query_row("SELECT COUNT(*) FROM egress_log", [], |row| row.get(0))
            .map_err(crate::storage::db::map_err)
    }

    fn folder_is_relocked(state: &AppState) -> Result<bool, AppError> {
        let locked = state
            .db
            .folder_by_id(FOLDER_ID)?
            .is_some_and(|folder| folder.locked);
        let session_visible = state
            .unlocked_folders
            .lock()
            .map_err(|_| AppError::Unavailable("runtime unlock set is unavailable".into()))?
            .contains(FOLDER_ID);
        Ok(locked && !session_visible)
    }

    /// Read-only native recovery proof for the exact synthetic rows. The folder remains locked and
    /// absent from the session unlock set: we unwrap its CK through the debug KEK hatch, decrypt
    /// every at-rest note/segment blob with its production AAD, and compare byte-for-byte with the
    /// seeded fixture without restoring plaintext into SQLite.
    fn relocked_content_is_recoverable(state: &AppState) -> Result<bool, AppError> {
        let kek = Zeroizing::new(crate::secrets::master_kek_with_policy(
            "Verify Harness relock",
            false,
        )?);
        let wrapped = state.db.folder_wrapped_key(FOLDER_ID)?.ok_or_else(|| {
            AppError::Unavailable("runtime folder has no wrapped content key".into())
        })?;
        let ck_bytes =
            crate::crypto::decrypt(&kek, &wrapped, &crate::commands::aad_wrapped_ck(FOLDER_ID))?;
        let ck_array: [u8; 32] = ck_bytes.try_into().map_err(|_| {
            AppError::Unavailable("runtime folder content key has an invalid length".into())
        })?;
        let ck = Zeroizing::new(ck_array);

        let notes = state.db.notes_in_folder(FOLDER_ID)?;
        if notes.len() != SOURCE_COUNT {
            return Ok(false);
        }
        let mut observed = BTreeSet::new();
        for note in notes {
            let Some(blob) = note.content_blob.as_deref() else {
                return Ok(false);
            };
            if !note.markdown.is_empty() || note.exported_path.is_some() {
                return Ok(false);
            }
            let index = (0..SOURCE_COUNT)
                .find(|index| note.meeting_id == source_id(*index))
                .ok_or_else(|| {
                    AppError::Unavailable("runtime relock contains an unknown note".into())
                })?;
            if !observed.insert(index) {
                return Ok(false);
            }
            let plaintext = crate::crypto::decrypt(
                &ck,
                blob,
                &crate::commands::aad_content(
                    FOLDER_ID,
                    &note.meeting_id,
                    &note.provider_id,
                    "note",
                ),
            )?;
            if plaintext
                != format!("## Action items\n- [ ] Runtime provider probe {index}").as_bytes()
            {
                return Ok(false);
            }
            let segments = state.db.raw_segments(&note.meeting_id)?;
            if segments.len() != 1
                || segments[0].idx != 0
                || !segments[0].text.is_empty()
                || segments[0].text_blob.is_none()
            {
                return Ok(false);
            }
            let segment_plaintext = crate::crypto::decrypt(
                &ck,
                segments[0].text_blob.as_deref().unwrap_or_default(),
                &crate::commands::aad_content(FOLDER_ID, &note.meeting_id, "-", "segment"),
            )?;
            if segment_plaintext != format!("Runtime provider probe {index}").as_bytes() {
                return Ok(false);
            }
        }
        Ok(observed.len() == SOURCE_COUNT)
    }

    fn reminder_is_persisted_and_masked(state: &AppState) -> Result<bool, AppError> {
        let snapshot = reminders_snapshot_at(state, chrono::Utc::now().timestamp_millis())?;
        Ok(snapshot
            .upcoming
            .iter()
            .chain(snapshot.completed.iter())
            .find(|reminder| reminder.title == REMINDER_TITLE && reminder.due_at == REMINDER_DUE_AT)
            .is_some_and(|reminder| reminder.sources.is_empty()))
    }

    fn prove_sqlcipher(state: &AppState) -> Result<(bool, bool), AppError> {
        let integrity = {
            let conn = state.db.lock();
            let mut statement = conn
                .prepare("PRAGMA cipher_integrity_check")
                .map_err(crate::storage::db::map_err)?;
            let mut rows = statement.query([]).map_err(crate::storage::db::map_err)?;
            rows.next().map_err(crate::storage::db::map_err)?.is_none()
        };
        let path = database_path()?;
        let mut header = [0u8; 16];
        let encrypted_header = File::open(&path)
            .and_then(|mut file| std::io::Read::read_exact(&mut file, &mut header))
            .map(|_| header != *b"SQLite format 3\0")
            .unwrap_or(false);
        let plain_query_rejected = rusqlite::Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .and_then(|connection| {
            connection.query_row("SELECT COUNT(*) FROM sqlite_master", [], |row| {
                row.get::<_, i64>(0)
            })
        })
        .is_err();
        Ok((integrity, encrypted_header && plain_query_rejected))
    }

    fn database_path() -> Result<PathBuf, AppError> {
        dirs::data_dir()
            .map(|base| {
                base.join(crate::state::app_dir_name())
                    .join("meetnotes.sqlite")
            })
            .ok_or_else(|| AppError::Unavailable("runtime database path is unavailable".into()))
    }

    fn seed_sources(state: &AppState) -> Result<(), AppError> {
        if state.db.folder_by_id(FOLDER_ID)?.is_some() {
            return Err(AppError::Unavailable(
                "runtime fixture folder already exists".into(),
            ));
        }
        state.db.insert_folder(&Folder {
            id: FOLDER_ID.into(),
            name: "Runtime probe".into(),
            path: "Runtime probe".into(),
            parent_id: None,
            locked: false,
            created_at: "2026-07-29T09:00:00Z".into(),
        })?;
        for index in 0..SOURCE_COUNT {
            let id = source_id(index);
            state.db.insert_meeting(&Meeting {
                id: id.clone(),
                started_at: format!("2026-07-29T10:{index:02}:00Z"),
                ended_at: None,
                title: Some(format!("Runtime probe {index}")),
                duration_s: 0,
                audio_path: None,
                status: MeetingStatus::Summarized,
                folder_id: None,
            })?;
            state.db.upsert_note(&NoteRecord {
                meeting_id: id.clone(),
                provider_id: "runtime-probe-local".into(),
                markdown: format!("## Action items\n- [ ] Runtime provider probe {index}"),
                created_at: format!("2026-07-29T11:{index:02}:00Z"),
                exported_path: None,
                model_requested: None,
                model_served: None,
                gateway_host: None,
            })?;
            state.db.set_note_folder(&id, Some(FOLDER_ID))?;
            state.db.replace_segments(
                &id,
                &[Segment {
                    idx: 0,
                    start_s: 0.0,
                    end_s: 1.0,
                    text: format!("Runtime provider probe {index}"),
                    speaker: Some("me".into()),
                    confidence: Some(1.0),
                }],
            )?;
        }
        Ok(())
    }

    fn source_id(index: usize) -> String {
        format!("{SOURCE_PREFIX}-{index}-20260729")
    }

    fn runtime() -> Result<std::sync::MutexGuard<'static, RuntimeState>, AppError> {
        RUNTIME
            .get()
            .ok_or_else(|| AppError::Unavailable("runtime probe was not initialized".into()))?
            .lock()
            .map_err(|_| AppError::Unavailable("runtime probe state is unavailable".into()))
    }

    pub(super) fn record_selector_success() -> Result<(), AppError> {
        if !super::reminder_runtime_probe_requested() || RUNTIME.get().is_none() {
            return Ok(());
        }
        SELECTOR_SUCCESSES
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map(|_| ())
            .map_err(|_| AppError::Unavailable("runtime selector counter overflowed".into()))
    }

    pub(super) fn record_locked_audit_outcome() -> Result<(), AppError> {
        if !super::reminder_runtime_probe_requested() || RUNTIME.get().is_none() {
            return Ok(());
        }
        LOCKED_AUDIT_OUTCOMES
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| {
                value.checked_add(1)
            })
            .map(|_| ())
            .map_err(|_| AppError::Unavailable("runtime lock counter overflowed".into()))
    }

    pub(super) fn record_startup_reconcile(
        derived_before: usize,
        derived_after: usize,
    ) -> Result<(), AppError> {
        if !super::reminder_runtime_probe_requested()
            || std::env::var_os("MURMUR_HARNESS_PHASE_TWO_NONCE").is_none()
        {
            return Ok(());
        }
        let paths = ProbePaths::new()?;
        let intent: RestartIntent = read_json(&paths.intent)?;
        if intent.restart_canary_rows != 2 || derived_before != 2 || derived_after != 0 {
            return Err(AppError::Unavailable(
                "startup reminder purge did not consume the persisted canary".into(),
            ));
        }
        STARTUP_DERIVED_BEFORE
            .compare_exchange(
                usize::MAX,
                derived_before,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| {
                AppError::Unavailable("startup reminder purge was observed twice".into())
            })?;
        STARTUP_DERIVED_AFTER
            .compare_exchange(
                usize::MAX,
                derived_after,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| {
                AppError::Unavailable("startup reminder purge was observed twice".into())
            })?;
        Ok(())
    }

    impl ProbePaths {
        fn new() -> Result<Self, AppError> {
            let home = verified_home()?;
            let runtime_dir = std::env::var_os("MURMUR_HARNESS_RUNTIME_DIR")
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .ok_or_else(|| {
                    AppError::Unavailable("runtime probe directory is unavailable".into())
                })?;
            if !runtime_dir.is_dir() {
                return Err(AppError::Unavailable(
                    "runner-owned runtime probe directory is absent".into(),
                ));
            }
            let model = crate::reason::brain_model_by_id(LOCAL_MODEL_ID).ok_or_else(|| {
                AppError::Unavailable("runtime local model fixture is unavailable".into())
            })?;
            let dummy_model = crate::transcribe::models_dir()?.join(model.filename);
            Ok(Self {
                challenge: runtime_dir.join(CHALLENGE_FILE),
                intent: runtime_dir.join(INTENT_FILE),
                generation: runtime_dir.join(GENERATION_FILE),
                witness: runtime_dir.join(WITNESS_FILE),
                sidecar_script: runtime_dir.join("reminder-runtime-brain-sidecar.py"),
                claude_script: runtime_dir.join("reminder-runtime-claude-marker.sh"),
                dummy_model,
                brain_count: home.join(".murmur-reminder-brain-count"),
                race_armed: home.join(".murmur-reminder-race-armed"),
                race_started: home.join(".murmur-reminder-race-started"),
                race_release: home.join(".murmur-reminder-race-release"),
                race_done: home.join(".murmur-reminder-race-done"),
                claude_called: home.join(".murmur-reminder-claude-called"),
                runtime_dir,
            })
        }
    }

    fn verify_isolated_runtime() -> Result<(), AppError> {
        let _ = verified_home()?;
        let runtime = std::env::var_os("MURMUR_HARNESS_RUNTIME_DIR")
            .map(PathBuf::from)
            .ok_or_else(|| AppError::Unavailable("runtime probe directory is absent".into()))?;
        if !runtime.is_absolute() || !runtime.is_dir() {
            return Err(AppError::Unavailable(
                "runtime probe directory is not an absolute runner directory".into(),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = std::fs::symlink_metadata(&runtime).map_err(|_| {
                AppError::Unavailable("runtime probe directory metadata is unavailable".into())
            })?;
            if metadata.file_type().is_symlink() || metadata.permissions().mode() & 0o077 != 0 {
                return Err(AppError::Unavailable(
                    "runtime probe directory is not private".into(),
                ));
            }
        }
        Ok(())
    }

    fn verified_home() -> Result<PathBuf, AppError> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| AppError::Unavailable("runtime probe HOME is unavailable".into()))?;
        let parent_name = home
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if home.file_name().and_then(|name| name.to_str()) != Some("home")
            || !parent_name.starts_with("murmur-boot-")
        {
            return Err(AppError::Unavailable(
                "runtime probe is outside its isolated HOME".into(),
            ));
        }
        Ok(home)
    }

    fn probe_challenge(paths: &ProbePaths) -> Result<RuntimeChallenge, AppError> {
        let challenge: RuntimeChallenge = read_json(&paths.challenge)?;
        let valid_run_id = challenge.run_id.starts_with("boot-")
            && challenge.run_id.len() <= 128
            && challenge
                .run_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-');
        if challenge.schema_version != 1
            || challenge.task_id != REMINDER_RUNTIME_PROBE_TASK_ID
            || !valid_run_id
            || paths.runtime_dir.file_name().and_then(|name| name.to_str())
                != Some(challenge.run_id.as_str())
            || challenge.runner_pid == 0
            || !valid_lowercase_hex(&challenge.runner_nonce, 64)
            || !valid_lowercase_hex(&challenge.binary_sha256, 64)
            || !valid_lowercase_hex(&challenge.network_profile_sha256, 64)
            || !valid_lowercase_hex(&challenge.runner_source_sha256, 64)
            || !valid_lowercase_hex(&challenge.task_contract_sha256, 64)
            || !valid_lowercase_hex(&challenge.git_head, 40)
            || challenge.started_at.len() < 20
            || challenge.started_at.len() > 40
            || !challenge.started_at.ends_with('Z')
            || !challenge.started_at.is_ascii()
        {
            return Err(AppError::Unavailable(
                "runner challenge is stale or invalid".into(),
            ));
        }
        Ok(challenge)
    }

    fn valid_lowercase_hex(value: &str, len: usize) -> bool {
        value.len() == len
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    }

    fn current_executable_sha256() -> Result<String, AppError> {
        let path = std::env::current_exe()
            .map_err(|_| AppError::Unavailable("running executable path is unavailable".into()))?;
        let before = std::fs::symlink_metadata(&path).map_err(|_| {
            AppError::Unavailable("running executable metadata is unavailable".into())
        })?;
        if before.file_type().is_symlink() || !before.file_type().is_file() || before.len() == 0 {
            return Err(AppError::Unavailable(
                "running executable is not a regular file".into(),
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if before.nlink() != 1 {
                return Err(AppError::Unavailable(
                    "running executable has an unexpected link count".into(),
                ));
            }
        }
        let mut file = File::open(&path)
            .map_err(|_| AppError::Unavailable("running executable open failed".into()))?;
        let opened = file
            .metadata()
            .map_err(|_| AppError::Unavailable("running executable metadata changed".into()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if opened.dev() != before.dev()
                || opened.ino() != before.ino()
                || opened.len() != before.len()
            {
                return Err(AppError::Unavailable(
                    "running executable changed while opening".into(),
                ));
            }
        }
        #[cfg(not(unix))]
        if opened.len() != before.len() {
            return Err(AppError::Unavailable(
                "running executable changed while opening".into(),
            ));
        }
        let mut hasher = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|_| AppError::Unavailable("running executable read failed".into()))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let after = std::fs::symlink_metadata(&path)
            .map_err(|_| AppError::Unavailable("running executable metadata disappeared".into()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if after.file_type().is_symlink()
                || !after.file_type().is_file()
                || after.dev() != before.dev()
                || after.ino() != before.ino()
                || after.len() != before.len()
            {
                return Err(AppError::Unavailable(
                    "running executable changed while hashing".into(),
                ));
            }
        }
        #[cfg(not(unix))]
        if !after.file_type().is_file() || after.len() != before.len() {
            return Err(AppError::Unavailable(
                "running executable changed while hashing".into(),
            ));
        }
        Ok(format!("{:x}", hasher.finalize()))
    }

    #[cfg(unix)]
    fn configure_witness_fd() -> Result<(), AppError> {
        use std::os::fd::RawFd;

        unsafe extern "C" {
            fn fcntl(fd: RawFd, command: i32, ...) -> i32;
        }
        const F_GETFD: i32 = 1;
        const F_SETFD: i32 = 2;
        const FD_CLOEXEC: i32 = 1;

        let phase_nonce = std::env::var("MURMUR_HARNESS_PHASE_TWO_NONCE").ok();
        let descriptor = std::env::var("MURMUR_HARNESS_WITNESS_FD").ok();
        match (phase_nonce, descriptor) {
            (None, None) => Ok(()),
            (Some(nonce), Some(raw)) if valid_lowercase_hex(&nonce, 64) => {
                let fd = raw.parse::<RawFd>().map_err(|_| {
                    AppError::Unavailable("runtime witness descriptor is invalid".into())
                })?;
                if fd <= 2 {
                    return Err(AppError::Unavailable(
                        "runtime witness descriptor is unsafe".into(),
                    ));
                }
                // SAFETY: the runner deliberately passes this open descriptor to process two.
                let flags = unsafe { fcntl(fd, F_GETFD) };
                if flags < 0 || unsafe { fcntl(fd, F_SETFD, flags | FD_CLOEXEC) } < 0 {
                    return Err(AppError::Unavailable(
                        "runtime witness descriptor cannot be protected".into(),
                    ));
                }
                Ok(())
            }
            _ => Err(AppError::Unavailable(
                "runtime witness descriptor is not phase-bound".into(),
            )),
        }
    }

    #[cfg(not(unix))]
    fn configure_witness_fd() -> Result<(), AppError> {
        if std::env::var_os("MURMUR_HARNESS_PHASE_TWO_NONCE").is_some()
            || std::env::var_os("MURMUR_HARNESS_WITNESS_FD").is_some()
        {
            return Err(AppError::Unavailable(
                "runtime witness descriptor requires Unix".into(),
            ));
        }
        Ok(())
    }

    #[cfg(unix)]
    fn write_witness_pipe(witness: &RuntimeWitness) -> Result<(), AppError> {
        use std::os::fd::{FromRawFd, RawFd};

        let fd = std::env::var("MURMUR_HARNESS_WITNESS_FD")
            .map_err(|_| AppError::Unavailable("runtime witness descriptor is absent".into()))?
            .parse::<RawFd>()
            .map_err(|_| AppError::Unavailable("runtime witness descriptor is invalid".into()))?;
        if fd <= 2 {
            return Err(AppError::Unavailable(
                "runtime witness descriptor is unsafe".into(),
            ));
        }
        let mut bytes = serde_json::to_vec(witness)
            .map_err(|_| AppError::Unavailable("runtime witness encoding failed".into()))?;
        bytes.push(b'\n');
        if bytes.len() as u64 > MAX_RECEIPT_BYTES {
            return Err(AppError::Unavailable(
                "runtime witness exceeds its size bound".into(),
            ));
        }
        // Consume the descriptor exactly once. Removing the environment binding first makes a
        // duplicate IPC call fail closed even if the OS later reuses the descriptor number.
        std::env::remove_var("MURMUR_HARNESS_WITNESS_FD");
        // SAFETY: process two exclusively owns the inherited write end supplied by the runner.
        let mut pipe = unsafe { File::from_raw_fd(fd) };
        pipe.write_all(&bytes)
            .and_then(|_| pipe.flush())
            .map_err(|_| AppError::Unavailable("runtime witness pipe write failed".into()))
    }

    #[cfg(not(unix))]
    fn write_witness_pipe(_witness: &RuntimeWitness) -> Result<(), AppError> {
        Err(AppError::Unavailable(
            "runtime witness pipe requires Unix".into(),
        ))
    }

    fn write_probe_executables(paths: &ProbePaths) -> Result<(), AppError> {
        let sidecar = r#"#!/usr/bin/env python3
import json
import os
import pathlib
import sys
import time

home = pathlib.Path(os.environ["HOME"])
count = home / ".murmur-reminder-brain-count"
armed = home / ".murmur-reminder-race-armed"
started = home / ".murmur-reminder-race-started"
release = home / ".murmur-reminder-race-release"
done = home / ".murmur-reminder-race-done"

def read_object(path):
    with path.open("r", encoding="utf-8") as handle:
        value = json.load(handle)
    if not isinstance(value, dict):
        raise ValueError("race receipt is not an object")
    return value

def create_object(path, value):
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    descriptor = os.open(path, flags, 0o600)
    try:
        if os.write(descriptor, encoded) != len(encoded):
            raise OSError("short race receipt write")
        os.fsync(descriptor)
    finally:
        os.close(descriptor)

print(json.dumps({"type": "ready", "model_id": "runtime-probe-local"}), flush=True)
for line in sys.stdin:
    try:
        request = json.loads(line)
    except json.JSONDecodeError:
        sys.exit(72)
    request_id = request.get("id") if isinstance(request, dict) else None
    if (
        not isinstance(request, dict)
        or request.get("type") != "generate"
        or not isinstance(request_id, int)
        or isinstance(request_id, bool)
        or request_id <= 0
    ):
        sys.exit(73)
    with count.open("a", encoding="ascii") as handle:
        handle.write("x\n")
        handle.flush()
        os.fsync(handle.fileno())
    race_event = None
    if armed.exists():
        arm = read_object(armed)
        if (
            set(arm) != {"runnerNonce", "raceNonce", "requestId"}
            or not isinstance(arm.get("runnerNonce"), str)
            or not isinstance(arm.get("raceNonce"), str)
            or arm.get("requestId") != 0
        ):
            sys.exit(74)
        race_event = {
            "runnerNonce": arm["runnerNonce"],
            "raceNonce": arm["raceNonce"],
            "requestId": request_id,
        }
        create_object(started, race_event)
        while True:
            if release.exists():
                if read_object(release) != race_event:
                    sys.exit(75)
                break
            time.sleep(0.01)
    response = json.dumps(
        {
            "type": "done",
            "id": request_id,
            "text": json.dumps({"keepIds": ["c1"]}, separators=(",", ":")),
        },
        separators=(",", ":"),
    )
    if race_event is not None:
        create_object(done, race_event)
    print(
        response,
        flush=True,
    )
"#;
        let claude = r#"#!/bin/sh
: > "$HOME/.murmur-reminder-claude-called"
exit 71
"#;
        write_executable(&paths.sidecar_script, sidecar.as_bytes())?;
        write_executable(&paths.claude_script, claude.as_bytes())
    }

    #[cfg(unix)]
    fn write_executable(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
        use std::os::unix::fs::PermissionsExt;
        if path.exists() {
            let existing = read_bounded_regular(path)?;
            let metadata = std::fs::symlink_metadata(path)
                .map_err(|_| AppError::Unavailable("runtime executable metadata failed".into()))?;
            if existing != bytes || metadata.permissions().mode() & 0o777 != 0o700 {
                return Err(AppError::Unavailable(
                    "runtime executable changed between generations".into(),
                ));
            }
            return Ok(());
        }
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .map_err(|_| AppError::Unavailable("runtime executable create failed".into()))?;
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| AppError::Unavailable("runtime executable write failed".into()))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| AppError::Unavailable("runtime executable chmod failed".into()))
    }

    #[cfg(not(unix))]
    fn write_executable(_path: &Path, _bytes: &[u8]) -> Result<(), AppError> {
        Err(AppError::Unavailable(
            "runtime executable probe requires Unix".into(),
        ))
    }

    fn ensure_dummy_model(paths: &ProbePaths) -> Result<(), AppError> {
        if paths.dummy_model.is_file() {
            return Ok(());
        }
        if let Some(parent) = paths.dummy_model.parent() {
            std::fs::create_dir_all(parent).map_err(|_| {
                AppError::Unavailable("runtime model directory could not be created".into())
            })?;
        }
        std::fs::write(&paths.dummy_model, b"GGUF").map_err(|_| {
            AppError::Unavailable("runtime local model fixture could not be written".into())
        })
    }

    fn brain_generate_count(paths: &ProbePaths) -> Result<usize, AppError> {
        match std::fs::read_to_string(&paths.brain_count) {
            Ok(value) => Ok(value.lines().filter(|line| *line == "x").count()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(0),
            Err(_) => Err(AppError::Unavailable(
                "runtime local generation counter is unreadable".into(),
            )),
        }
    }

    fn remove_file_if_present(path: &Path) -> Result<(), AppError> {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(_) => Err(AppError::Unavailable(
                "runtime marker cleanup failed".into(),
            )),
        }
    }

    fn read_bounded_regular(path: &Path) -> Result<Vec<u8>, AppError> {
        let before = std::fs::symlink_metadata(path)
            .map_err(|_| AppError::Unavailable("runtime receipt metadata failed".into()))?;
        if !before.file_type().is_file() || before.len() > MAX_RECEIPT_BYTES {
            return Err(AppError::Unavailable(
                "runtime receipt is not a bounded regular file".into(),
            ));
        }
        #[cfg(unix)]
        let file = {
            use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
            #[cfg(target_os = "macos")]
            const O_NOFOLLOW: i32 = 0x0000_0100;
            #[cfg(target_os = "linux")]
            const O_NOFOLLOW: i32 = 0x0002_0000;
            #[cfg(not(any(target_os = "macos", target_os = "linux")))]
            const O_NOFOLLOW: i32 = 0;
            if before.nlink() != 1 {
                return Err(AppError::Unavailable(
                    "runtime receipt has an unsafe link count".into(),
                ));
            }
            OpenOptions::new()
                .read(true)
                .custom_flags(O_NOFOLLOW)
                .open(path)
                .map_err(|_| AppError::Unavailable("runtime receipt open failed".into()))?
        };
        #[cfg(not(unix))]
        let file = File::open(path)
            .map_err(|_| AppError::Unavailable("runtime receipt open failed".into()))?;
        let after = file
            .metadata()
            .map_err(|_| AppError::Unavailable("runtime receipt metadata changed".into()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if !after.file_type().is_file()
                || after.nlink() != 1
                || after.dev() != before.dev()
                || after.ino() != before.ino()
                || after.len() > MAX_RECEIPT_BYTES
            {
                return Err(AppError::Unavailable(
                    "runtime receipt changed while opening".into(),
                ));
            }
        }
        #[cfg(not(unix))]
        if !after.file_type().is_file() || after.len() > MAX_RECEIPT_BYTES {
            return Err(AppError::Unavailable(
                "runtime receipt changed while opening".into(),
            ));
        }
        let mut bytes = Vec::with_capacity(after.len() as usize);
        file.take(MAX_RECEIPT_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| AppError::Unavailable("runtime receipt read failed".into()))?;
        if bytes.len() as u64 > MAX_RECEIPT_BYTES {
            return Err(AppError::Unavailable(
                "runtime receipt exceeds its size bound".into(),
            ));
        }
        Ok(bytes)
    }

    fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, AppError> {
        let bytes = read_bounded_regular(path)?;
        serde_json::from_slice(&bytes)
            .map_err(|_| AppError::Unavailable("runtime receipt is invalid".into()))
    }

    fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<(), AppError> {
        let parent = path
            .parent()
            .ok_or_else(|| AppError::Unavailable("runtime receipt has no parent".into()))?;
        if !parent.is_dir() || path.exists() {
            return Err(AppError::Unavailable(
                "runtime receipt target is absent or already claimed".into(),
            ));
        }
        let bytes = serde_json::to_vec(value)
            .map_err(|_| AppError::Unavailable("runtime receipt encoding failed".into()))?;
        if bytes.len() as u64 > MAX_RECEIPT_BYTES {
            return Err(AppError::Unavailable(
                "runtime receipt exceeds its size bound".into(),
            ));
        }
        let temp = parent.join(format!(
            ".{}.{}.{}.tmp",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("receipt"),
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temp)
            .map_err(|_| AppError::Unavailable("runtime receipt create failed".into()))?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| AppError::Unavailable("runtime receipt sync failed".into()))?;
        drop(file);
        let promoted = std::fs::hard_link(&temp, path);
        let cleanup = std::fs::remove_file(&temp);
        promoted.map_err(|_| AppError::Unavailable("runtime receipt promote failed".into()))?;
        cleanup.map_err(|_| AppError::Unavailable("runtime receipt cleanup failed".into()))?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| AppError::Unavailable("runtime receipt directory sync failed".into()))
    }

    fn start_mcp_after_probe(app: &AppHandle) {
        let state = app.state::<AppState>();
        let require_token = state
            .config
            .lock()
            .map(|config| config.mcp_require_token)
            .unwrap_or(true);
        crate::mcp::spawn(app.clone(), require_token);
    }

    #[cfg(test)]
    mod tests {
        use super::validate_proxy_endpoint;

        #[test]
        fn runtime_proxy_requires_loopback_and_per_run_basic_auth() {
            let token = "a".repeat(64);
            assert!(validate_proxy_endpoint(&format!(
                "http://murmur-harness:{token}@127.0.0.1:49152"
            ))
            .is_ok());

            for invalid in [
                "http://127.0.0.1:49152",
                "http://murmur-harness:aaaaaaaa@127.0.0.1:49152",
                "http://murmur-harness:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa@localhost:49152",
                "https://murmur-harness:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa@127.0.0.1:49152",
                "http://murmur-harness:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa@127.0.0.1:49152/path",
            ] {
                assert!(validate_proxy_endpoint(invalid).is_err(), "{invalid}");
            }
        }
    }
}

/// Serialize a source-content-bearing command result while the same coarse lock lifecycle guard
/// still protects the reads that built it. Tauri's blanket `IpcResponse` implementation normally
/// calls `serde_json::to_string` only *after* the command returns; returning an already encoded
/// `Response::Json` closes that post-return relock window without changing the JS payload shape.
fn lifecycle_reminder_response<T: Serialize>(
    state: &AppState,
    build: impl FnOnce() -> Result<T, AppError>,
) -> Result<Response, AppError> {
    let lifecycle = super::lifecycle_guard(state);
    let payload = build()?;
    serialize_reminder_response_under_lifecycle(&lifecycle, &payload)
}

fn serialize_reminder_response_under_lifecycle<T: Serialize>(
    _lifecycle: &std::sync::MutexGuard<'_, ()>,
    payload: &T,
) -> Result<Response, AppError> {
    serde_json::to_string(payload)
        .map(Response::new)
        .map_err(|_| AppError::Unavailable("reminder response encoding failed".into()))
}

#[tauri::command]
pub fn list_reminders(app: AppHandle, state: State<'_, AppState>) -> Result<Response, AppError> {
    lifecycle_reminder_response(state.inner(), || {
        let now = chrono::Utc::now().timestamp_millis();
        let inserted = state.db.materialize_due_reminders(now)?;
        let snapshot = reminders_snapshot_at(state.inner(), now)?;
        if inserted > 0 {
            crate::events::emit_reminders_updated(&app, snapshot.due_inbox_count);
        }
        Ok(snapshot)
    })
}

#[tauri::command]
pub fn get_reminder_summary(
    app: AppHandle,
    state: State<'_, AppState>,
) -> Result<ReminderSummary, AppError> {
    let _lifecycle = super::lifecycle_guard(state.inner());
    let inserted = state
        .db
        .materialize_due_reminders(chrono::Utc::now().timestamp_millis())?;
    let summary = ReminderSummary {
        due_inbox_count: state.db.due_reminder_count()?,
    };
    if inserted > 0 {
        crate::events::emit_reminders_updated(&app, summary.due_inbox_count);
    }
    Ok(summary)
}

#[tauri::command]
pub fn create_reminder(
    app: AppHandle,
    state: State<'_, AppState>,
    draft: ReminderDraft,
) -> Result<Response, AppError> {
    lifecycle_reminder_response(state.inner(), || {
        let draft = validate_draft(draft)?;
        require_sources_visible(state.inner(), &draft.sources)?;
        let id = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().timestamp_millis();
        state
            .db
            .create_reminder(&id, &draft, ReminderOrigin::Manual, now)?;
        let stored = state
            .db
            .get_stored_reminder(&id)?
            .ok_or_else(|| AppError::Storage("new reminder row disappeared".into()))?;
        let view = reminder_view(state.inner(), stored)?;
        state.db.materialize_due_reminders(now)?;
        emit_reminder_count(&app, state.inner());
        Ok(view)
    })
}

#[tauri::command]
pub fn update_reminder(
    app: AppHandle,
    state: State<'_, AppState>,
    reminder_id: String,
    draft: ReminderDraft,
) -> Result<Response, AppError> {
    lifecycle_reminder_response(state.inner(), || {
        validate_opaque_id(&reminder_id, "reminder")?;
        let mut draft = validate_draft(draft)?;
        require_sources_visible(state.inner(), &draft.sources)?;
        let existing = state
            .db
            .get_stored_reminder(&reminder_id)?
            .ok_or_else(|| AppError::InvalidArg("unknown reminder id".into()))?;
        preserve_hidden_sources_on_update(state.inner(), &existing.sources, &mut draft.sources)?;
        let now = chrono::Utc::now().timestamp_millis();
        if !state.db.update_reminder(&reminder_id, &draft, now)? {
            return Err(AppError::InvalidArg("unknown reminder id".into()));
        }
        let stored = state
            .db
            .get_stored_reminder(&reminder_id)?
            .ok_or_else(|| AppError::Storage("updated reminder row disappeared".into()))?;
        let view = reminder_view(state.inner(), stored)?;
        state.db.materialize_due_reminders(now)?;
        emit_reminder_count(&app, state.inner());
        Ok(view)
    })
}

#[tauri::command]
pub fn delete_reminder(
    app: AppHandle,
    state: State<'_, AppState>,
    reminder_id: String,
) -> Result<(), AppError> {
    let _lifecycle = super::lifecycle_guard(state.inner());
    validate_opaque_id(&reminder_id, "reminder")?;
    if !state.db.delete_reminder(&reminder_id)? {
        return Err(AppError::InvalidArg("unknown reminder id".into()));
    }
    emit_reminder_count(&app, state.inner());
    Ok(())
}

#[tauri::command]
pub fn complete_reminder(
    app: AppHandle,
    state: State<'_, AppState>,
    reminder_id: String,
    expected_due_at: i64,
) -> Result<(), AppError> {
    let _lifecycle = super::lifecycle_guard(state.inner());
    validate_opaque_id(&reminder_id, "reminder")?;
    validate_due_at(expected_due_at)?;
    let now = chrono::Utc::now().timestamp_millis();
    // False is an idempotent replay (or a stale UI schedule generation), never a second advance.
    let _ = state
        .db
        .complete_reminder(&reminder_id, expected_due_at, now)?;
    // A very short recurrence can already be due after advancement; materialize it in the same
    // user action rather than waiting for the next scheduler tick.
    state.db.materialize_due_reminders(now)?;
    emit_reminder_count(&app, state.inner());
    Ok(())
}

#[tauri::command]
pub fn dismiss_reminder_occurrence(
    app: AppHandle,
    state: State<'_, AppState>,
    occurrence_id: String,
) -> Result<(), AppError> {
    let _lifecycle = super::lifecycle_guard(state.inner());
    validate_opaque_id(&occurrence_id, "occurrence")?;
    let _ = state
        .db
        .dismiss_reminder_occurrence(&occurrence_id, chrono::Utc::now().timestamp_millis())?;
    emit_reminder_count(&app, state.inner());
    Ok(())
}

/// Audit one currently visible canonical source for possible reminders. Deterministic candidates
/// are prepared locally; the optional light-model call may only select their ids and never creates
/// a reminder. The source is re-gated and re-hashed under lifecycle immediately before any cache
/// rows cross IPC or any derived rows are replaced.
#[tauri::command]
pub async fn audit_reminder_suggestions(
    state: State<'_, AppState>,
    source_kind: String,
    source_id: String,
) -> Result<Response, AppError> {
    #[cfg(debug_assertions)]
    let record_locked_outcome = reminder_runtime_probe_requested()
        && source_kind == "meeting"
        && source_id == "runtime-probe-meeting-5-20260729";
    let reasoner = state.reasoner.light();
    let result = audit_reminder_suggestions_with_response(
        state.inner(),
        source_kind,
        source_id,
        reasoner,
        |lifecycle, views| serialize_reminder_response_under_lifecycle(lifecycle, &views),
    )
    .await;
    #[cfg(debug_assertions)]
    if record_locked_outcome && matches!(&result, Err(AppError::Locked(_))) {
        runtime_probe::record_locked_audit_outcome()?;
    }
    result
}

#[cfg(test)]
async fn audit_reminder_suggestions_inner(
    state: &AppState,
    source_kind: String,
    source_id: String,
    reasoner: Arc<dyn LocalReasoner>,
) -> Result<Vec<ReminderSuggestionView>, AppError> {
    audit_reminder_suggestions_with_response(state, source_kind, source_id, reasoner, |_, views| {
        Ok(views)
    })
    .await
}

async fn audit_reminder_suggestions_with_response<R>(
    state: &AppState,
    source_kind: String,
    source_id: String,
    reasoner: Arc<dyn LocalReasoner>,
    finish: impl FnOnce(
        &std::sync::MutexGuard<'_, ()>,
        Vec<ReminderSuggestionView>,
    ) -> Result<R, AppError>,
) -> Result<R, AppError> {
    validate_reminder_audit_source(&source_kind, &source_id)?;
    let (snapshot, source_content) =
        capture_reminder_audit_source(state, &source_kind, &source_id)?;
    let expected_hash = reminder_audit_source_hash(&source_content);
    let engine_id = reasoner.id().to_string();

    // Cache reads return source-derived rows, so they stay inside the same lifecycle interval as
    // snapshot revalidation, exact canonical re-read/hash, and live source-metadata resolution.
    {
        let lifecycle = super::lifecycle_guard(state);
        let current = current_reminder_audit_source_under_lifecycle(
            state,
            &source_kind,
            &source_id,
            &snapshot,
        )?;
        require_reminder_audit_hash(&current, &expected_hash)?;
        if state.db.reminder_audit_cache_matches(
            &source_kind,
            &source_id,
            &expected_hash,
            &engine_id,
        )? {
            let rows = state.db.list_pending_reminder_suggestions(
                &source_kind,
                &source_id,
                &expected_hash,
                &engine_id,
                MAX_REMINDER_AUDIT_ROWS,
            )?;
            return finish(&lifecycle, reminder_suggestion_views(rows, &current.source));
        }
    }

    // The hash above covers ALL exact canonical parts. Candidate extraction gets a separately
    // bounded, complete-line note prefix and ordered segment prefix so neither local tokenization
    // nor the tiny selector prompt scales with an arbitrarily long meeting.
    let candidate_markdown = reminder_candidate_markdown(&source_content);
    let candidate_segments = bounded_audit_segments(&source_content.segments);
    drop(source_content);
    let transcript_candidates = crate::summarize::recall_net::possible_missed_item_candidates(
        &candidate_markdown,
        &candidate_segments,
    );
    let candidates =
        crate::reminder_audit::build_candidates(&candidate_markdown, &transcript_candidates);
    let candidates = select_reminder_audit_candidates(reasoner, candidates).await?;

    let lifecycle = super::lifecycle_guard(state);
    let current =
        current_reminder_audit_source_under_lifecycle(state, &source_kind, &source_id, &snapshot)?;
    require_reminder_audit_hash(&current, &expected_hash)?;
    if !state.db.replace_reminder_audit_results(
        &source_kind,
        &source_id,
        &expected_hash,
        &engine_id,
        &candidates,
        chrono::Utc::now().timestamp_millis(),
    )? {
        return Err(AppError::InvalidArg(
            "reminder audit source changed — retry the audit".into(),
        ));
    }
    let rows = state.db.list_pending_reminder_suggestions(
        &source_kind,
        &source_id,
        &expected_hash,
        &engine_id,
        MAX_REMINDER_AUDIT_ROWS,
    )?;
    finish(&lifecycle, reminder_suggestion_views(rows, &current.source))
}

/// Promote one still-current suggestion into a canonical Smart reminder. The derived row is the
/// one-shot capability: replay cannot insert a second reminder, while sibling suggestions from the
/// same audit remain independently promotable.
#[tauri::command]
pub fn accept_reminder_suggestion(
    app: AppHandle,
    state: State<'_, AppState>,
    suggestion_id: String,
    draft: ReminderDraft,
) -> Result<Response, AppError> {
    accept_reminder_suggestion_with_postwork(
        state.inner(),
        suggestion_id,
        draft,
        |state| {
            state
                .db
                .materialize_due_reminders(chrono::Utc::now().timestamp_millis())?;
            emit_reminder_count(&app, state);
            Ok(())
        },
        |lifecycle, view| serialize_reminder_response_under_lifecycle(lifecycle, &view),
    )
}

#[cfg(test)]
fn accept_reminder_suggestion_inner(
    state: &AppState,
    suggestion_id: String,
    draft: ReminderDraft,
) -> Result<ReminderView, AppError> {
    accept_reminder_suggestion_with_postwork(
        state,
        suggestion_id,
        draft,
        |_| Ok(()),
        |_, view| Ok(view),
    )
}

/// Keep the source-title-bearing response and every fallible command post-step inside one
/// visibility lifecycle interval. A concurrent relock must not blank/revoke the source after the
/// view is built but before the command has finished materializing, publishing its new count, and
/// serializing the final IPC body.
fn accept_reminder_suggestion_with_postwork<R>(
    state: &AppState,
    suggestion_id: String,
    draft: ReminderDraft,
    postwork: impl FnOnce(&AppState) -> Result<(), AppError>,
    finish: impl FnOnce(&std::sync::MutexGuard<'_, ()>, ReminderView) -> Result<R, AppError>,
) -> Result<R, AppError> {
    let lifecycle = super::lifecycle_guard(state);
    let view = accept_reminder_suggestion_under_lifecycle(state, suggestion_id, draft)?;
    postwork(state)?;
    finish(&lifecycle, view)
}

fn accept_reminder_suggestion_under_lifecycle(
    state: &AppState,
    suggestion_id: String,
    draft: ReminderDraft,
) -> Result<ReminderView, AppError> {
    validate_opaque_id(&suggestion_id, "reminder suggestion")?;
    let suggestion = state
        .db
        .get_pending_reminder_suggestion_gate_anchor(&suggestion_id)?
        .ok_or_else(|| AppError::InvalidArg("unknown reminder suggestion".into()))?;

    let current = read_reminder_audit_source_under_lifecycle(
        state,
        &suggestion.source_kind,
        &suggestion.source_id,
    )?;
    let current_hash = reminder_audit_source_hash(&current);
    let draft = validate_draft(draft)?;
    require_smart_source_capacity(
        &draft.sources,
        &suggestion.source_kind,
        &suggestion.source_id,
    )?;
    require_sources_visible(state, &draft.sources)?;

    let reminder_id = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp_millis();
    if !state
        .db
        .promote_pending_reminder_suggestion(ReminderSuggestionPromotion {
            suggestion_id: &suggestion.id,
            expected_source_kind: &suggestion.source_kind,
            expected_source_id: &suggestion.source_id,
            expected_content_hash: &current_hash,
            reminder_id: &reminder_id,
            draft: &draft,
            now,
        })?
    {
        return Err(AppError::InvalidArg(
            "reminder suggestion is stale or was already accepted".into(),
        ));
    }
    let stored = state
        .db
        .get_stored_reminder(&reminder_id)?
        .ok_or_else(|| AppError::Storage("promoted reminder row disappeared".into()))?;
    reminder_view(state, stored)
}

/// Dismiss one still-current derived row. Editing, moving, sealing, or relocking the source before
/// this point rejects the request; the command never acts on a stale suggestion capability.
#[tauri::command]
pub fn dismiss_reminder_suggestion(
    state: State<'_, AppState>,
    suggestion_id: String,
) -> Result<(), AppError> {
    validate_opaque_id(&suggestion_id, "reminder suggestion")?;
    let suggestion = state
        .db
        .get_pending_reminder_suggestion_gate_anchor(&suggestion_id)?
        .ok_or_else(|| AppError::InvalidArg("unknown reminder suggestion".into()))?;

    let _lifecycle = super::lifecycle_guard(state.inner());
    let current = read_reminder_audit_source_under_lifecycle(
        state.inner(),
        &suggestion.source_kind,
        &suggestion.source_id,
    )?;
    let current_hash = reminder_audit_source_hash(&current);
    if !state.db.dismiss_pending_reminder_suggestion(
        &suggestion.id,
        &suggestion.source_kind,
        &suggestion.source_id,
        &current_hash,
    )? {
        return Err(AppError::InvalidArg(
            "reminder suggestion is stale or already dismissed".into(),
        ));
    }
    Ok(())
}

/// Start the app-process reminder workers. The durable due scheduler retains its 15-second cadence;
/// a separate short worker drains content-free canonical-source invalidations for Smart cards.
/// Neither worker makes a delivery claim while the process is fully quit.
pub(crate) fn spawn_reminder_scheduler(app: AppHandle) {
    let invalidation_app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            if let Some(state) = invalidation_app.try_state::<AppState>() {
                match state
                    .db
                    .peek_reminder_source_invalidations(REMINDER_SOURCE_INVALIDATION_BATCH)
                {
                    Ok(invalidations) => {
                        for invalidation in invalidations {
                            if crate::events::emit_reminder_source_updated(
                                &invalidation_app,
                                &invalidation.kind,
                                &invalidation.id,
                            ) {
                                if let Err(error) =
                                    state.db.ack_reminder_source_invalidation(&invalidation)
                                {
                                    tracing::warn!(
                                        target: "reminders",
                                        error = %error,
                                        "reminder source invalidation acknowledgement failed"
                                    );
                                }
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(
                            target: "reminders",
                            error = %error,
                            "reminder source invalidation drain failed"
                        );
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(
                REMINDER_SOURCE_INVALIDATION_TICK_MS,
            ))
            .await;
        }
    });

    tauri::async_runtime::spawn(async move {
        loop {
            if let Some(state) = app.try_state::<AppState>() {
                let now = chrono::Utc::now().timestamp_millis();
                match state.db.materialize_due_reminders(now) {
                    Ok(inserted) if inserted > 0 => emit_reminder_count(&app, state.inner()),
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(
                            target: "reminders",
                            error = %error,
                            "due reminder materialization failed"
                        );
                    }
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(REMINDER_SCHEDULER_TICK_SECS)).await;
        }
    });
}

fn reminders_snapshot_at(state: &AppState, _now: i64) -> Result<RemindersSnapshot, AppError> {
    let stored = state.db.list_stored_reminders()?;
    let occurrences = state.db.unread_reminder_occurrences()?;
    let inbox_reminder_ids = occurrences
        .iter()
        .map(|occurrence| occurrence.reminder_id.as_str())
        .collect::<BTreeSet<_>>();
    let mut views = HashMap::<String, ReminderView>::new();
    let mut upcoming = Vec::new();
    let mut completed = Vec::new();
    for row in stored {
        let view = reminder_view(state, row)?;
        match view.state {
            ReminderState::Active if !inbox_reminder_ids.contains(view.id.as_str()) => {
                upcoming.push(view.clone());
            }
            ReminderState::Active => {}
            ReminderState::Completed => completed.push(view.clone()),
        }
        views.insert(view.id.clone(), view);
    }
    completed.sort_by(|a, b| {
        b.completed_at
            .cmp(&a.completed_at)
            .then_with(|| b.updated_at.cmp(&a.updated_at))
            .then_with(|| a.id.cmp(&b.id))
    });
    let inbox = occurrences
        .into_iter()
        .filter_map(|occurrence| {
            views
                .get(&occurrence.reminder_id)
                .cloned()
                .map(|reminder| ReminderInboxItem {
                    occurrence_id: occurrence.id,
                    due_at: occurrence.due_at,
                    reminder,
                })
        })
        .collect::<Vec<_>>();
    Ok(RemindersSnapshot {
        due_inbox_count: inbox.len() as u64,
        inbox,
        upcoming,
        completed,
    })
}

fn reminder_view(state: &AppState, stored: StoredReminder) -> Result<ReminderView, AppError> {
    let sources = visible_source_views(state, &stored.sources)?;
    Ok(ReminderView {
        id: stored.id,
        title: stored.title,
        details: stored.details,
        due_at: stored.due_at,
        repeat_every: stored.repeat_every,
        repeat_unit: stored.repeat_unit,
        state: stored.state,
        origin: stored.origin,
        created_at: stored.created_at,
        updated_at: stored.updated_at,
        completed_at: stored.completed_at,
        sources,
    })
}

enum ReminderAuditSourceSnapshot {
    Meeting(super::MeetingContentSnapshot),
    Note(super::DocumentContentSnapshot),
}

struct ReminderAuditSourceContent {
    markdown: String,
    manual_notes: String,
    segments: Vec<Segment>,
    source: ReminderSourceView,
}

/// Capture the source's visibility generation, then re-enter the same non-reentrant lifecycle
/// interval for the exact canonical plaintext read. The second check closes the small gap between
/// the existing snapshot helper releasing the mutex and this domain selecting content.
fn capture_reminder_audit_source(
    state: &AppState,
    source_kind: &str,
    source_id: &str,
) -> Result<(ReminderAuditSourceSnapshot, ReminderAuditSourceContent), AppError> {
    let snapshot = match source_kind {
        "meeting" => ReminderAuditSourceSnapshot::Meeting(super::capture_meeting_content_snapshot(
            state, source_id,
        )?),
        "note" => ReminderAuditSourceSnapshot::Note(super::capture_document_content_snapshot(
            state, source_id,
        )?),
        _ => {
            return Err(AppError::InvalidArg(
                "reminder audit source must be a meeting or note".into(),
            ));
        }
    };
    let _lifecycle = super::lifecycle_guard(state);
    let content =
        current_reminder_audit_source_under_lifecycle(state, source_kind, source_id, &snapshot)?;
    Ok((snapshot, content))
}

/// Revalidate a previously captured source authorization and re-read its exact canonical content.
/// Callers hold the lifecycle mutex through the subsequent cache read or derived write.
fn current_reminder_audit_source_under_lifecycle(
    state: &AppState,
    source_kind: &str,
    source_id: &str,
    snapshot: &ReminderAuditSourceSnapshot,
) -> Result<ReminderAuditSourceContent, AppError> {
    match (source_kind, snapshot) {
        ("meeting", ReminderAuditSourceSnapshot::Meeting(snapshot)) => {
            super::require_current_meeting_content_snapshot_under_lifecycle(
                state, source_id, snapshot,
            )?;
        }
        ("note", ReminderAuditSourceSnapshot::Note(snapshot)) => {
            super::require_current_document_content_snapshot_under_lifecycle(
                state, source_id, snapshot,
            )?;
        }
        _ => {
            return Err(AppError::InvalidArg(
                "reminder audit source changed kind".into(),
            ));
        }
    }
    read_reminder_audit_source_under_lifecycle(state, source_kind, source_id)
}

/// Gate before every content/title read. Meeting hashes include the resolved source title, latest
/// note (or an explicit empty part), typed manual notes, and every segment text in canonical `idx`
/// order. Authored-note hashes include the resolved title plus exact `documents.text` body.
fn read_reminder_audit_source_under_lifecycle(
    state: &AppState,
    source_kind: &str,
    source_id: &str,
) -> Result<ReminderAuditSourceContent, AppError> {
    match source_kind {
        "meeting" => {
            if !super::meeting_is_unlocked(state, source_id)? {
                return Err(AppError::Locked(crate::errcode::tag(
                        crate::errcode::MEETING_LOCKED,
                        "this meeting's folder is locked — unlock it and retry",
                    )));
            }
            let meeting = state
                .db
                .get_meeting(source_id)?
                .ok_or_else(|| AppError::InvalidArg("unknown reminder audit source".into()))?;
            let markdown = state
                .db
                .latest_reminder_audit_markdown(source_id)?
                .unwrap_or_default();
            let manual_notes = state.db.get_manual_notes(source_id)?;
            // Smart reminders follow the same canonical merged-transcript presentation as the
            // meeting UI/export: measured capture echo is provenance, not user-visible speech.
            let segments = state
                .db
                .get_segments_with_echo_provenance(source_id)?
                .into_iter()
                .filter(|stored| !stored.echo_suppressed)
                .map(|stored| stored.segment)
                .collect();
            Ok(ReminderAuditSourceContent {
                markdown,
                manual_notes,
                segments,
                source: ReminderSourceView {
                    kind: source_kind.to_string(),
                    id: source_id.to_string(),
                    title: meeting
                        .title
                        .filter(|title| !title.trim().is_empty())
                        .unwrap_or_else(|| "Untitled meeting".into()),
                },
            })
        }
        "note" => {
            let Some((folder_id, _created_at, _updated_at)) =
                state.db.note_gate_anchor(source_id)?
            else {
                return Err(AppError::InvalidArg("unknown reminder audit source".into()));
            };
            if !super::folder_is_unlocked(state, &folder_id)? {
                return Err(AppError::Locked(
                    "this note's folder is locked — unlock it and retry".into(),
                ));
            }
            let row = state
                .db
                .get_note_row(source_id)?
                .ok_or_else(|| AppError::InvalidArg("unknown reminder audit source".into()))?;
            Ok(ReminderAuditSourceContent {
                markdown: row.text,
                manual_notes: String::new(),
                segments: Vec::new(),
                source: ReminderSourceView {
                    kind: source_kind.to_string(),
                    id: source_id.to_string(),
                    title: row
                        .title
                        .filter(|title| !title.trim().is_empty())
                        .unwrap_or(row.name),
                },
            })
        }
        _ => Err(AppError::InvalidArg(
            "reminder audit source must be a meeting or note".into(),
        )),
    }
}

fn reminder_audit_source_hash(content: &ReminderAuditSourceContent) -> String {
    crate::storage::reminder_store::canonical_reminder_source_hash(
        &content.source.title,
        &content.markdown,
        (content.source.kind == "meeting").then_some(content.manual_notes.as_str()),
        &content.segments,
    )
}

fn reminder_candidate_markdown(content: &ReminderAuditSourceContent) -> String {
    // Candidate preparation is independently bounded from the full streaming hash. Reserve a
    // quarter of the fixed budget for typed notes when present, then let a short generated note
    // donate its unused bytes. Never concatenate the full canonical strings before truncation.
    let markdown_budget = if content.manual_notes.is_empty() {
        MAX_AUDIT_MARKDOWN_BYTES
    } else {
        MAX_AUDIT_MARKDOWN_BYTES * 3 / 4
    };
    let markdown = bounded_complete_lines(&content.markdown, markdown_budget);
    let mut output = String::with_capacity(MAX_AUDIT_MARKDOWN_BYTES);
    output.push_str(markdown);
    if !content.manual_notes.is_empty() {
        let separator_bytes = usize::from(!output.is_empty());
        let remaining = MAX_AUDIT_MARKDOWN_BYTES
            .saturating_sub(output.len())
            .saturating_sub(separator_bytes);
        let manual_notes = bounded_complete_lines(&content.manual_notes, remaining);
        if !manual_notes.is_empty() {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(manual_notes);
        }
    }
    output
}

fn reminder_suggestion_views(
    rows: Vec<StoredReminderSuggestion>,
    source: &ReminderSourceView,
) -> Vec<ReminderSuggestionView> {
    rows.into_iter()
        .map(|row| ReminderSuggestionView {
            id: row.id,
            title: row.title,
            suggested_due_at: row.suggested_due_at,
            source: source.clone(),
        })
        .collect()
}

fn validate_reminder_audit_source(source_kind: &str, source_id: &str) -> Result<(), AppError> {
    if !matches!(source_kind, "meeting" | "note") {
        return Err(AppError::InvalidArg(
            "reminder audit source must be a meeting or note".into(),
        ));
    }
    validate_opaque_id(source_id, "reminder audit source")
}

fn require_reminder_audit_hash(
    current: &ReminderAuditSourceContent,
    expected_hash: &str,
) -> Result<(), AppError> {
    if reminder_audit_source_hash(current) != expected_hash {
        return Err(AppError::InvalidArg(
            "reminder audit source changed — retry the audit".into(),
        ));
    }
    Ok(())
}

fn bounded_complete_lines(value: &str, max_bytes: usize) -> &str {
    let prefix = bounded_prefix(value, max_bytes);
    if prefix.len() == value.len() {
        return prefix;
    }
    prefix
        .rfind('\n')
        .map_or("", |last_newline| &prefix[..=last_newline])
}

fn bounded_prefix(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    &value[..end]
}

fn bounded_audit_segments(segments: &[Segment]) -> Vec<Segment> {
    let mut remaining_bytes = MAX_AUDIT_SEGMENT_BYTES;
    let mut bounded = Vec::new();
    for segment in segments.iter().take(MAX_AUDIT_SEGMENTS) {
        if segment.text.len() > remaining_bytes {
            break;
        }
        remaining_bytes = remaining_bytes.saturating_sub(segment.text.len());
        bounded.push(segment.clone());
    }
    bounded
}

async fn select_reminder_audit_candidates(
    reasoner: Arc<dyn LocalReasoner>,
    candidates: Vec<ReminderAuditCandidate>,
) -> Result<Vec<ReminderAuditCandidate>, AppError> {
    if candidates.is_empty() || reasoner.id() == "stub" {
        return Ok(candidates);
    }

    let prompt_candidates = candidates
        .iter()
        .map(|candidate| {
            serde_json::json!({
                "id": candidate.id,
                "title": candidate.title,
            })
        })
        .collect::<Vec<_>>();
    let user = format!(
        "Select only clear, useful reminder candidates from this local list. \
         Return candidate ids only.\n\nCandidates:\n{}",
        serde_json::Value::Array(prompt_candidates)
    );
    let schema = serde_json::json!({
        "type": "object",
        "properties": {
            "keepIds": {
                "type": "array",
                "items": { "type": "string" },
                "maxItems": MAX_REMINDER_AUDIT_ROWS
            }
        },
        "required": ["keepIds"],
        "additionalProperties": false
    });
    let opts = GenOptions {
        max_tokens: Some(128),
        temperature: Some(0.0),
        enable_thinking: false,
        timeout: Some(Duration::from_secs(30)),
        ..GenOptions::default()
    };
    let verdict = tokio::task::spawn_blocking(move || {
        reasoner.structured_with(
            "You are a conservative local reminder selector. Never invent, rewrite, or add \
             candidates. Reply only with the requested JSON object.",
            &user,
            &schema,
            opts,
        )
    })
    .await;

    let value = match verdict {
        Ok(Ok(value)) => value,
        Ok(Err(_)) | Err(_) => return Ok(candidates),
    };
    match crate::reminder_audit::validate_keep_ids(value, &candidates) {
        Ok(selected) => {
            #[cfg(debug_assertions)]
            runtime_probe::record_selector_success()?;
            Ok(selected)
        }
        Err(_) => Ok(candidates),
    }
}

fn visible_source_views(
    state: &AppState,
    sources: &[ReminderSourceAnchor],
) -> Result<Vec<ReminderSourceView>, AppError> {
    let unlocked = super::unlocked_snapshot(state)?;
    let mut visible = Vec::new();
    for source in sources {
        match source.kind.as_str() {
            "meeting" if state.db.meeting_is_visible(&source.id, &unlocked)? => {
                if let Some(meeting) = state.db.get_meeting(&source.id)? {
                    visible.push(ReminderSourceView {
                        kind: source.kind.clone(),
                        id: source.id.clone(),
                        title: meeting
                            .title
                            .filter(|title| !title.trim().is_empty())
                            .unwrap_or_else(|| "Untitled meeting".into()),
                    });
                }
            }
            "note" if state.db.note_is_visible(&source.id, &unlocked)? => {
                if let Some(note) = state.db.get_note_row(&source.id)? {
                    visible.push(ReminderSourceView {
                        kind: source.kind.clone(),
                        id: source.id.clone(),
                        title: note
                            .title
                            .filter(|title| !title.trim().is_empty())
                            .unwrap_or(note.name),
                    });
                }
            }
            "meeting" | "note" => {} // currently sealed/deleted: omit metadata, keep reminder.
            _ => {
                return Err(AppError::Storage(
                    "reminder source kind is invalid on disk".into(),
                ));
            }
        }
    }
    Ok(visible)
}

fn validate_draft(mut draft: ReminderDraft) -> Result<ReminderDraft, AppError> {
    draft.title = draft.title.trim().to_string();
    if draft.title.is_empty() || draft.title.chars().count() > 240 {
        return Err(AppError::InvalidArg(
            "reminder title must be between 1 and 240 characters".into(),
        ));
    }
    draft.details = draft.details.and_then(|details| {
        let trimmed = details.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    });
    if draft
        .details
        .as_ref()
        .is_some_and(|details| details.chars().count() > 4000)
    {
        return Err(AppError::InvalidArg("reminder details are too long".into()));
    }
    validate_due_at(draft.due_at)?;
    match (draft.repeat_every, draft.repeat_unit) {
        (None, None) => {}
        (Some(1..=365), Some(_)) => {}
        _ => {
            return Err(AppError::InvalidArg(
                "reminder recurrence requires an interval from 1 to 365 and a unit".into(),
            ));
        }
    }
    if draft.sources.len() > MAX_REMINDER_SOURCES {
        return Err(AppError::InvalidArg("too many reminder sources".into()));
    }
    let mut dedup = BTreeSet::new();
    for source in &draft.sources {
        if !matches!(source.kind.as_str(), "meeting" | "note") {
            return Err(AppError::InvalidArg(
                "reminder source must be a meeting or note".into(),
            ));
        }
        validate_opaque_id(&source.id, "source")?;
        dedup.insert((source.kind.clone(), source.id.clone()));
    }
    draft.sources = dedup
        .into_iter()
        .map(|(kind, id)| ReminderSourceAnchor { kind, id })
        .collect();
    Ok(draft)
}

fn validate_due_at(due_at: i64) -> Result<(), AppError> {
    if !(crate::storage::reminder_store::MIN_REMINDER_DUE_AT
        ..crate::storage::reminder_store::MAX_REMINDER_DUE_AT)
        .contains(&due_at)
    {
        return Err(AppError::InvalidArg(
            "reminder due time is out of range".into(),
        ));
    }
    Ok(())
}

fn require_smart_source_capacity(
    sources: &[ReminderSourceAnchor],
    suggestion_source_kind: &str,
    suggestion_source_id: &str,
) -> Result<(), AppError> {
    let already_attached = sources
        .iter()
        .any(|source| source.kind == suggestion_source_kind && source.id == suggestion_source_id);
    if !already_attached && sources.len() >= MAX_REMINDER_SOURCES {
        return Err(AppError::InvalidArg(
            "too many reminder sources after attaching the Smart suggestion source".into(),
        ));
    }
    Ok(())
}

fn preserve_hidden_sources_on_update(
    state: &AppState,
    existing: &[ReminderSourceAnchor],
    submitted: &mut Vec<ReminderSourceAnchor>,
) -> Result<(), AppError> {
    let unlocked = super::unlocked_snapshot(state)?;
    let mut hidden = BTreeSet::new();
    for source in existing {
        let (exists, visible) = source_existence_and_visibility(state, source, &unlocked)?;
        if exists && !visible {
            hidden.insert((source.kind.clone(), source.id.clone()));
        }
    }
    merge_hidden_source_anchors(submitted, hidden);
    if submitted.len() > MAX_REMINDER_SOURCES {
        return Err(AppError::InvalidArg("too many reminder sources".into()));
    }
    Ok(())
}

fn merge_hidden_source_anchors(
    submitted: &mut Vec<ReminderSourceAnchor>,
    hidden: BTreeSet<(String, String)>,
) {
    submitted.extend(
        hidden
            .into_iter()
            .map(|(kind, id)| ReminderSourceAnchor { kind, id }),
    );
    submitted.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.id.cmp(&right.id))
    });
    submitted.dedup();
}

fn validate_opaque_id(id: &str, label: &str) -> Result<(), AppError> {
    if id.is_empty() || id.len() > 160 || id.chars().any(char::is_whitespace) {
        return Err(AppError::InvalidArg(format!("{label} id is invalid")));
    }
    Ok(())
}

fn require_sources_visible(
    state: &AppState,
    sources: &[ReminderSourceAnchor],
) -> Result<(), AppError> {
    let unlocked = super::unlocked_snapshot(state)?;
    for source in sources {
        let (_exists, visible) = source_existence_and_visibility(state, source, &unlocked)?;
        if !visible {
            return Err(AppError::Locked(
                "unlock the source before attaching it to a reminder".into(),
            ));
        }
    }
    Ok(())
}

fn source_existence_and_visibility(
    state: &AppState,
    source: &ReminderSourceAnchor,
    unlocked: &std::collections::HashSet<String>,
) -> Result<(bool, bool), AppError> {
    match source.kind.as_str() {
        "meeting" => {
            let exists = state.db.get_meeting_gate_anchor(&source.id)?.is_some();
            let visible = exists && state.db.meeting_is_visible(&source.id, unlocked)?;
            Ok((exists, visible))
        }
        "note" => {
            let exists = state.db.note_gate_anchor(&source.id)?.is_some();
            let visible = exists && state.db.note_is_visible(&source.id, unlocked)?;
            Ok((exists, visible))
        }
        _ => Err(AppError::Storage(
            "reminder source kind is invalid on disk".into(),
        )),
    }
}

fn emit_reminder_count(app: &AppHandle, state: &AppState) {
    match state.db.due_reminder_count() {
        Ok(count) => crate::events::emit_reminders_updated(app, count),
        Err(error) => {
            tracing::warn!(
                target: "reminders",
                error = %error,
                "could not count due reminders for update event"
            );
        }
    }
}

#[cfg(test)]
mod smart_audit_command_tests {
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::sync::{Condvar, Mutex};
    use std::time::Duration;

    use serde_json::{json, Value};
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::settings::{AppConfig, BrainBackend};
    use crate::storage::models::{Folder, Meeting, MeetingStatus, NoteRecord};
    use crate::storage::{AttachmentOwner, Db, NewAttachment};

    const TEST_DB_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    fn ensure_test_dev_kek() {
        crate::commands::dev_kek_fixture::ensure_dev_kek();
    }

    fn test_state(tag: &str, reasoner: Arc<dyn LocalReasoner>) -> Arc<AppState> {
        let path =
            crate::storage::db::unique_temp_path(&format!("reminder-command-{tag}"), "sqlite");
        let _ = std::fs::remove_file(&path);
        let db = Arc::new(Db::open_with_key(&path, TEST_DB_KEY).unwrap());
        Arc::new(AppState {
            recorder: Mutex::new(None),
            recording_stop: Mutex::new(None),
            voice_listener: Mutex::new(None),
            voice_listener_lifecycle: Mutex::new(()),
            recording_starting: std::sync::atomic::AtomicBool::new(false),
            voice_command_capture: Mutex::new(None),
            pending_manual_command: Mutex::new(None),
            live_running: std::sync::atomic::AtomicBool::new(false),
            db,
            config: Arc::new(Mutex::new(AppConfig::default())),
            reasoner: crate::reason::ReasonerCell::fixed(reasoner),
            current_meeting: Mutex::new(None),
            focus_meeting: Mutex::new(None),
            live_transcript: Mutex::new(String::new()),
            live_bullets: Mutex::new(String::new()),
            live_bullets_tracker: Mutex::new(crate::transcribe::bullets::BulletsTracker::default()),
            capped_notified: std::sync::atomic::AtomicBool::new(false),
            capture_fault_notified: std::sync::atomic::AtomicBool::new(false),
            reactions_shadow_count: std::sync::atomic::AtomicU64::new(0),
            reactions_emitted: Mutex::new(HashSet::new()),
            in_flight_turns: Mutex::new(std::collections::HashMap::new()),
            user_turn_in_progress: std::sync::atomic::AtomicBool::new(false),
            verify_cache: Mutex::new(std::collections::HashMap::new()),
            unlocked_folders: Arc::new(Mutex::new(HashSet::new())),
            master_kek: Mutex::new(None),
            org_ock_cache: Mutex::new(std::collections::HashMap::new()),
            account_session: Mutex::new(None),
            lifecycle: Mutex::new(()),
            active_salvages: Mutex::new(HashSet::new()),
            share_refresh_lock: tokio::sync::Mutex::new(()),
            org_share_mutation_lock: tokio::sync::Mutex::new(()),
            seal_epoch: std::sync::atomic::AtomicU64::new(0),
            heavy_inference: Arc::new(tokio::sync::Semaphore::new(1)),
        })
    }

    fn seed_meeting_source(state: &AppState) {
        state
            .db
            .insert_folder(&Folder {
                id: "f1".into(),
                name: "Private".into(),
                path: "Private".into(),
                parent_id: None,
                locked: false,
                created_at: "2026-07-29T09:00:00Z".into(),
            })
            .unwrap();
        state
            .db
            .insert_meeting(&Meeting {
                id: "m1".into(),
                started_at: "2026-07-29T10:00:00Z".into(),
                ended_at: None,
                title: Some("Planning".into()),
                duration_s: 0,
                audio_path: None,
                status: MeetingStatus::Summarized,
                folder_id: None,
            })
            .unwrap();
        state
            .db
            .upsert_note(&NoteRecord {
                meeting_id: "m1".into(),
                provider_id: "local".into(),
                markdown: "## Action items\n- [ ] Ship the plan".into(),
                created_at: "2026-07-29T10:05:00Z".into(),
                exported_path: None,
                model_requested: None,
                model_served: None,
                gateway_host: None,
            })
            .unwrap();
        state.db.set_note_folder("m1", Some("f1")).unwrap();
        state
            .db
            .replace_segments(
                "m1",
                &[Segment {
                    idx: 0,
                    start_s: 0.0,
                    end_s: 1.0,
                    text: "We should ship the plan".into(),
                    speaker: Some("me".into()),
                    confidence: Some(0.9),
                }],
            )
            .unwrap();
    }

    #[derive(Debug, PartialEq, Eq)]
    struct RelockPlaintext {
        note: String,
        segment: String,
        timeline: String,
        manual_notes: String,
        document: String,
        attachment: Vec<u8>,
    }

    const RELOCK_ATTACHMENT_ID: &str = "11111111-1111-4111-8111-111111111111";
    const SECOND_RELOCK_ATTACHMENT_ID: &str = "22222222-2222-4222-8222-222222222222";

    fn relock_attachment_bytes() -> Vec<u8> {
        let width = 64u32;
        let height = 40u32;
        let mut bytes = Vec::with_capacity(30);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&22u32.to_le_bytes());
        bytes.extend_from_slice(b"WEBPVP8X");
        bytes.extend_from_slice(&10u32.to_le_bytes());
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        let w = width - 1;
        let h = height - 1;
        bytes.extend_from_slice(&[w as u8, (w >> 8) as u8, (w >> 16) as u8]);
        bytes.extend_from_slice(&[h as u8, (h >> 8) as u8, (h >> 16) as u8]);
        bytes
    }

    fn seed_relock_all_families(state: &AppState) {
        seed_meeting_source(state);
        state
            .db
            .set_timeline_data("m1", r#"{"items":[{"text":"Ship it"}]}"#)
            .unwrap();
        state.db.set_manual_notes("m1", "Typed follow-up").unwrap();
        state
            .db
            .insert_note(
                "doc1",
                "f1",
                "Follow-up.md",
                "Follow-up",
                "Authored follow-up",
                1_900_000_000,
            )
            .unwrap();
        let attachment = relock_attachment_bytes();
        let attachment_hash: [u8; 32] = Sha256::digest(&attachment).into();
        let owner = AttachmentOwner::Meeting {
            meeting_id: "m1".into(),
            provider_id: "local".into(),
        };
        state
            .db
            .insert_attachment(&NewAttachment {
                id: RELOCK_ATTACHMENT_ID,
                owner: &owner,
                mime_type: "image/webp",
                extension: "webp",
                width: 64,
                height: 40,
                sha256: &attachment_hash,
                byte_len: attachment.len(),
                data: &attachment,
                data_blob: None,
                created_at: 1_900_000_001,
            })
            .unwrap();
    }

    fn relock_plaintext(state: &AppState) -> RelockPlaintext {
        RelockPlaintext {
            note: state
                .db
                .notes_in_folder("f1")
                .unwrap()
                .into_iter()
                .find(|note| note.meeting_id == "m1" && note.provider_id == "local")
                .unwrap()
                .markdown,
            segment: state
                .db
                .raw_segments("m1")
                .unwrap()
                .into_iter()
                .find(|segment| segment.idx == 0)
                .unwrap()
                .text,
            timeline: state.db.raw_timeline("m1").unwrap().unwrap().data,
            manual_notes: state.db.raw_manual_notes("m1").unwrap().unwrap().text,
            document: state
                .db
                .raw_documents_in_folder("f1")
                .unwrap()
                .into_iter()
                .find(|document| document.id == "doc1")
                .unwrap()
                .text,
            attachment: state
                .db
                .attachments_in_folder("f1")
                .unwrap()
                .into_iter()
                .find(|attachment| attachment.id == RELOCK_ATTACHMENT_ID)
                .unwrap()
                .data,
        }
    }

    fn seed_session_unlocked_relock_state(tag: &str) -> (Arc<AppState>, [u8; 32]) {
        ensure_test_dev_kek();
        let state = test_state(tag, Arc::new(crate::reason::StubReasoner));
        seed_relock_all_families(&state);
        crate::commands::lock_folder_inner(&state, "f1".into()).unwrap();

        let kek = crate::secrets::get_or_create_master_kek().unwrap();
        let wrapped = state.db.folder_wrapped_key("f1").unwrap().unwrap();
        let ck_bytes =
            crate::crypto::decrypt(&kek, &wrapped, &crate::commands::aad_wrapped_ck("f1")).unwrap();
        let ck: [u8; 32] = ck_bytes.try_into().unwrap();
        *state.master_kek.lock().unwrap() = Some(zeroize::Zeroizing::new(kek));

        for note in state.db.notes_in_folder("f1").unwrap() {
            let blob = note.content_blob.as_ref().unwrap();
            let markdown = String::from_utf8(
                crate::crypto::decrypt(
                    &ck,
                    blob,
                    &crate::commands::aad_content(
                        "f1",
                        &note.meeting_id,
                        &note.provider_id,
                        "note",
                    ),
                )
                .unwrap(),
            )
            .unwrap();
            state
                .db
                .restore_note_markdown(&note.meeting_id, &note.provider_id, &markdown)
                .unwrap();
        }
        crate::commands::unseal_folder_extras(&state, "f1", &ck, None).unwrap();
        state.unlocked_folders.lock().unwrap().insert("f1".into());
        (state, ck)
    }

    fn restore_relock_plaintext_from_blobs(state: &AppState, ck: &[u8; 32]) {
        for note in state.db.notes_in_folder("f1").unwrap() {
            let blob = note.content_blob.as_ref().unwrap();
            let markdown = String::from_utf8(
                crate::crypto::decrypt(
                    ck,
                    blob,
                    &crate::commands::aad_content(
                        "f1",
                        &note.meeting_id,
                        &note.provider_id,
                        "note",
                    ),
                )
                .unwrap(),
            )
            .unwrap();
            state
                .db
                .restore_note_markdown(&note.meeting_id, &note.provider_id, &markdown)
                .unwrap();
        }
        crate::commands::unseal_folder_extras(state, "f1", ck, None).unwrap();
    }

    fn seed_second_session_unlocked_folder(state: &AppState) -> [u8; 32] {
        state
            .db
            .insert_folder(&Folder {
                id: "f2".into(),
                name: "Second private".into(),
                path: "Second private".into(),
                parent_id: None,
                locked: false,
                created_at: "2026-07-29T11:00:00Z".into(),
            })
            .unwrap();
        state
            .db
            .insert_meeting(&Meeting {
                id: "m2".into(),
                started_at: "2026-07-29T11:00:00Z".into(),
                ended_at: Some("2026-07-29T11:30:00Z".into()),
                title: Some("Second meeting".into()),
                duration_s: 1_800,
                audio_path: None,
                status: MeetingStatus::Summarized,
                folder_id: Some("f2".into()),
            })
            .unwrap();
        state
            .db
            .upsert_note(&NoteRecord {
                meeting_id: "m2".into(),
                provider_id: "local".into(),
                markdown: "Second folder plaintext".into(),
                created_at: "2026-07-29T11:30:00Z".into(),
                ..NoteRecord::default()
            })
            .unwrap();
        state.db.set_note_folder("m2", Some("f2")).unwrap();
        let attachment = relock_attachment_bytes();
        let attachment_hash: [u8; 32] = Sha256::digest(&attachment).into();
        let owner = AttachmentOwner::Meeting {
            meeting_id: "m2".into(),
            provider_id: "local".into(),
        };
        state
            .db
            .insert_attachment(&NewAttachment {
                id: SECOND_RELOCK_ATTACHMENT_ID,
                owner: &owner,
                mime_type: "image/webp",
                extension: "webp",
                width: 64,
                height: 40,
                sha256: &attachment_hash,
                byte_len: attachment.len(),
                data: &attachment,
                data_blob: None,
                created_at: 1_900_000_002,
            })
            .unwrap();

        crate::commands::lock_folder_inner(state, "f2".into()).unwrap();
        let kek = crate::secrets::get_or_create_master_kek().unwrap();
        let wrapped = state.db.folder_wrapped_key("f2").unwrap().unwrap();
        let ck_bytes =
            crate::crypto::decrypt(&kek, &wrapped, &crate::commands::aad_wrapped_ck("f2")).unwrap();
        let ck: [u8; 32] = ck_bytes.try_into().unwrap();
        *state.master_kek.lock().unwrap() = Some(zeroize::Zeroizing::new(kek));
        let note = state.db.notes_in_folder("f2").unwrap().remove(0);
        let markdown = String::from_utf8(
            crate::crypto::decrypt(
                &ck,
                note.content_blob.as_ref().unwrap(),
                &crate::commands::aad_content("f2", "m2", "local", "note"),
            )
            .unwrap(),
        )
        .unwrap();
        state
            .db
            .restore_note_markdown("m2", "local", &markdown)
            .unwrap();
        crate::commands::unseal_folder_extras(state, "f2", &ck, None).unwrap();
        state.unlocked_folders.lock().unwrap().insert("f2".into());
        ck
    }

    fn reminder_draft() -> ReminderDraft {
        ReminderDraft {
            title: "Ship the plan".into(),
            details: None,
            due_at: 2_000_000_000_000,
            repeat_every: None,
            repeat_unit: None,
            sources: Vec::new(),
        }
    }

    struct BarrierSelector {
        calls: Arc<AtomicUsize>,
        started: Mutex<Option<mpsc::Sender<()>>>,
        release: Arc<(Mutex<bool>, Condvar)>,
    }

    impl LocalReasoner for BarrierSelector {
        fn id(&self) -> &str {
            "test-local-barrier"
        }

        fn reason(&self, _system: &str, _user: &str) -> crate::error::Result<String> {
            panic!("Smart reminder runtime must not use free-form/provider reasoning")
        }

        fn structured(
            &self,
            _system: &str,
            _user: &str,
            _json_schema: &Value,
        ) -> crate::error::Result<Value> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Some(sender) = self.started.lock().unwrap().take() {
                sender.send(()).unwrap();
            }
            let (released, wake) = &*self.release;
            let mut released = released.lock().unwrap();
            while !*released {
                released = wake.wait(released).unwrap();
            }
            Ok(json!({"keepIds": ["c1"]}))
        }
    }

    enum SelectorReply {
        Value(Value),
        Error,
    }

    struct FixedSelector {
        reply: SelectorReply,
        calls: Arc<AtomicUsize>,
    }

    impl LocalReasoner for FixedSelector {
        fn id(&self) -> &str {
            "test-local"
        }

        fn reason(&self, _system: &str, _user: &str) -> crate::error::Result<String> {
            Ok(String::new())
        }

        fn structured(
            &self,
            _system: &str,
            _user: &str,
            _json_schema: &Value,
        ) -> crate::error::Result<Value> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match &self.reply {
                SelectorReply::Value(value) => Ok(value.clone()),
                SelectorReply::Error => {
                    Err(AppError::Unavailable("test selector unavailable".into()))
                }
            }
        }
    }

    fn candidates() -> Vec<ReminderAuditCandidate> {
        crate::reminder_audit::build_candidates("- [ ] First\n- [ ] Second\n", &[])
    }

    #[tokio::test]
    async fn stub_selector_keeps_the_deterministic_candidates_without_a_model_call() {
        let expected = candidates();
        let selected = select_reminder_audit_candidates(
            Arc::new(crate::reason::StubReasoner),
            expected.clone(),
        )
        .await
        .unwrap();
        assert_eq!(selected, expected);
    }

    #[tokio::test]
    async fn malformed_unknown_and_error_selectors_each_fall_back_after_one_call() {
        for reply in [
            SelectorReply::Value(json!({"keepIds": "c1"})),
            SelectorReply::Value(json!({"keepIds": ["invented"]})),
            SelectorReply::Error,
        ] {
            let expected = candidates();
            let calls = Arc::new(AtomicUsize::new(0));
            let selected = select_reminder_audit_candidates(
                Arc::new(FixedSelector {
                    reply,
                    calls: Arc::clone(&calls),
                }),
                expected.clone(),
            )
            .await
            .unwrap();
            assert_eq!(selected, expected);
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn valid_selector_can_only_keep_known_rows_in_source_order() {
        let calls = Arc::new(AtomicUsize::new(0));
        let selected = select_reminder_audit_candidates(
            Arc::new(FixedSelector {
                reply: SelectorReply::Value(json!({"keepIds": ["c2"]})),
                calls: Arc::clone(&calls),
            }),
            candidates(),
        )
        .await
        .unwrap();
        assert_eq!(
            selected
                .iter()
                .map(|candidate| candidate.id.as_str())
                .collect::<Vec<_>>(),
            vec!["c2"]
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn reminder_audit_runtime_uses_only_the_pinned_local_reasoner() {
        let calls = Arc::new(AtomicUsize::new(0));
        let selector: Arc<dyn LocalReasoner> = Arc::new(FixedSelector {
            reply: SelectorReply::Value(json!({"keepIds": ["c1"]})),
            calls: Arc::clone(&calls),
        });
        let state = test_state("local-only", Arc::clone(&selector));
        seed_meeting_source(&state);

        let rows =
            audit_reminder_suggestions_inner(&state, "meeting".into(), "m1".into(), selector)
                .await
                .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn production_light_dispatch_never_falls_back_to_cloud_roles() {
        let config = Arc::new(Mutex::new(AppConfig {
            brain_backend: BrainBackend::Cloud,
            provider_id: "anthropic".into(),
            role_notes_connection: "anthropic".into(),
            role_ask_connection: "gateway".into(),
            role_live_connection: "claude_code".into(),
            brain_light_model_id: Some("definitely-missing-test-model".into()),
            ..AppConfig::default()
        }));
        let reasoners =
            crate::reason::ReasonerCell::new(config, Arc::new(tokio::sync::Semaphore::new(1)));
        assert_eq!(
            reasoners.light().id(),
            "stub",
            "light class dispatch must degrade locally and never construct a cloud/provider path"
        );
    }

    #[tokio::test]
    async fn echo_suppressed_checklist_text_is_never_suggested() {
        let state = test_state("echo-suppressed", Arc::new(crate::reason::StubReasoner));
        seed_meeting_source(&state);
        state
            .db
            .lock()
            .execute_batch(
                "UPDATE notes SET markdown='' WHERE meeting_id='m1';
                 UPDATE segments
                    SET text='- [ ] Do not repeat captured echo', echo_suppressed=1
                  WHERE meeting_id='m1' AND idx=0;",
            )
            .unwrap();

        let source = read_reminder_audit_source_under_lifecycle(&state, "meeting", "m1").unwrap();
        assert!(
            source.segments.is_empty(),
            "Smart audit must consume the visible merged projection, not raw echo rows"
        );
        let rows = audit_reminder_suggestions_inner(
            &state,
            "meeting".into(),
            "m1".into(),
            Arc::new(crate::reason::StubReasoner),
        )
        .await
        .unwrap();
        assert!(
            rows.is_empty(),
            "suppressed capture echo must never become a Smart reminder"
        );
    }

    #[test]
    fn reminder_snapshot_partitions_future_due_and_dismissed_active_rows_without_duplicates() {
        const NOW: i64 = 2_000_000_000_000;
        let state = test_state("snapshot-partition", Arc::new(crate::reason::StubReasoner));
        let mut future = reminder_draft();
        future.title = "Future".into();
        future.due_at = NOW + 1_000;
        state
            .db
            .create_reminder("r-future", &future, ReminderOrigin::Manual, 1)
            .unwrap();
        let mut overdue = reminder_draft();
        overdue.title = "Overdue".into();
        overdue.due_at = NOW - 1_000;
        state
            .db
            .create_reminder("r-overdue", &overdue, ReminderOrigin::Manual, 1)
            .unwrap();
        let mut completed = reminder_draft();
        completed.title = "Completed".into();
        completed.due_at = NOW - 2_000;
        state
            .db
            .create_reminder("r-completed", &completed, ReminderOrigin::Manual, 1)
            .unwrap();
        assert!(state
            .db
            .complete_reminder("r-completed", completed.due_at, NOW - 1)
            .unwrap());
        assert_eq!(state.db.materialize_due_reminders(NOW).unwrap(), 1);

        let due_snapshot = reminders_snapshot_at(&state, NOW).unwrap();
        assert_eq!(
            due_snapshot
                .upcoming
                .iter()
                .map(|reminder| reminder.id.as_str())
                .collect::<Vec<_>>(),
            vec!["r-future"]
        );
        assert_eq!(due_snapshot.inbox.len(), 1);
        assert_eq!(due_snapshot.inbox[0].reminder.id, "r-overdue");
        assert_eq!(
            due_snapshot
                .completed
                .iter()
                .map(|reminder| reminder.id.as_str())
                .collect::<Vec<_>>(),
            vec!["r-completed"],
            "completed projection must remain intact"
        );
        assert!(
            due_snapshot
                .upcoming
                .iter()
                .all(|reminder| reminder.id != "r-overdue"),
            "a due active reminder must be represented only by its unread Inbox occurrence"
        );

        assert!(state
            .db
            .dismiss_reminder_occurrence(&due_snapshot.inbox[0].occurrence_id, NOW + 1)
            .unwrap());
        let dismissed_snapshot = reminders_snapshot_at(&state, NOW + 1).unwrap();
        assert!(dismissed_snapshot.inbox.is_empty());
        assert_eq!(
            dismissed_snapshot
                .upcoming
                .iter()
                .map(|reminder| reminder.id.as_str())
                .collect::<Vec<_>>(),
            vec!["r-overdue", "r-future"],
            "a dismissed overdue one-off must remain accessible in Upcoming"
        );
        assert_eq!(dismissed_snapshot.completed.len(), 1);
        assert!(matches!(
            state
                .db
                .get_stored_reminder("r-overdue")
                .unwrap()
                .unwrap()
                .state,
            ReminderState::Active
        ));
    }

    #[test]
    fn reminder_snapshot_unread_occurrence_wins_materialize_after_captured_now_race() {
        const SNAPSHOT_NOW: i64 = 2_000_000_000_000;
        let state = test_state("snapshot-race", Arc::new(crate::reason::StubReasoner));
        let mut racing_due = reminder_draft();
        racing_due.title = "Racing due".into();
        racing_due.due_at = SNAPSHOT_NOW + 1;
        state
            .db
            .create_reminder("r-racing", &racing_due, ReminderOrigin::Manual, 1)
            .unwrap();

        assert_eq!(
            state
                .db
                .materialize_due_reminders(SNAPSHOT_NOW + 1)
                .unwrap(),
            1,
            "scheduler race precondition: unread occurrence was materialized after captured now"
        );
        let snapshot = reminders_snapshot_at(&state, SNAPSHOT_NOW).unwrap();
        assert!(snapshot.upcoming.is_empty());
        assert_eq!(snapshot.inbox.len(), 1);
        assert_eq!(snapshot.inbox[0].reminder.id, "r-racing");
    }

    #[test]
    fn smart_promotion_reserves_capacity_for_its_canonical_source() {
        let twenty_other_sources = (0..MAX_REMINDER_SOURCES)
            .map(|index| ReminderSourceAnchor {
                kind: "note".into(),
                id: format!("n{index}"),
            })
            .collect::<Vec<_>>();
        assert!(matches!(
            require_smart_source_capacity(&twenty_other_sources, "meeting", "m1"),
            Err(AppError::InvalidArg(_))
        ));

        let mut including_origin = twenty_other_sources;
        including_origin[MAX_REMINDER_SOURCES - 1] = ReminderSourceAnchor {
            kind: "meeting".into(),
            id: "m1".into(),
        };
        require_smart_source_capacity(&including_origin, "meeting", "m1").unwrap();
    }

    #[test]
    fn source_response_serialization_holds_lifecycle_until_json_is_complete() {
        struct BlockingPayload {
            entered: mpsc::Sender<()>,
            release: mpsc::Receiver<()>,
        }

        impl serde::Serialize for BlockingPayload {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                self.entered.send(()).map_err(serde::ser::Error::custom)?;
                self.release
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(serde::ser::Error::custom)?;
                "source title".serialize(serializer)
            }
        }

        let state = test_state(
            "response-serialization-lifecycle",
            Arc::new(crate::reason::StubReasoner),
        );
        let (serialize_entered_tx, serialize_entered_rx) = mpsc::channel();
        let (release_serialize_tx, release_serialize_rx) = mpsc::channel();
        let (serialize_done_tx, serialize_done_rx) = mpsc::channel();
        let serialize_state = Arc::clone(&state);
        let serialize = std::thread::spawn(move || {
            let result = lifecycle_reminder_response(&serialize_state, || {
                Ok(BlockingPayload {
                    entered: serialize_entered_tx,
                    release: release_serialize_rx,
                })
            });
            serialize_done_tx.send(result.is_ok()).unwrap();
        });
        serialize_entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("response never entered serde serialization");

        let (competing_lock_tx, competing_lock_rx) = mpsc::channel();
        let competing_state = Arc::clone(&state);
        let competing = std::thread::spawn(move || {
            let _lifecycle = super::super::lifecycle_guard(&competing_state);
            competing_lock_tx.send(()).unwrap();
        });
        assert!(
            matches!(
                competing_lock_rx.recv_timeout(Duration::from_millis(150)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "a competing lock crossed the actual serde serialization boundary"
        );

        release_serialize_tx.send(()).unwrap();
        assert!(
            serialize_done_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("response serialization did not finish"),
            "response serialization failed"
        );
        competing_lock_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("lifecycle remained blocked after serialization completed");
        serialize.join().unwrap();
        competing.join().unwrap();
    }

    #[tokio::test]
    async fn accepted_view_holds_lifecycle_through_command_postwork() {
        ensure_test_dev_kek();
        let state = test_state(
            "accept-postwork-lifecycle",
            Arc::new(crate::reason::StubReasoner),
        );
        seed_meeting_source(&state);
        let suggestions = audit_reminder_suggestions_inner(
            &state,
            "meeting".into(),
            "m1".into(),
            Arc::new(crate::reason::StubReasoner),
        )
        .await
        .unwrap();
        let suggestion_id = suggestions[0].id.clone();

        let (postwork_entered_tx, postwork_entered_rx) = mpsc::channel();
        let release_postwork = Arc::new((Mutex::new(false), Condvar::new()));
        let accept_state = Arc::clone(&state);
        let accept_release = Arc::clone(&release_postwork);
        let accept = std::thread::spawn(move || {
            accept_reminder_suggestion_with_postwork(
                &accept_state,
                suggestion_id,
                reminder_draft(),
                |_| {
                    postwork_entered_tx.send(()).unwrap();
                    let (released, wake) = &*accept_release;
                    let mut released = released.lock().unwrap();
                    while !*released {
                        released = wake.wait(released).unwrap();
                    }
                    Ok(())
                },
                |_, view| Ok(view),
            )
        });
        postwork_entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("acceptance never reached command postwork");

        let lock_state = Arc::clone(&state);
        let (lock_done_tx, lock_done_rx) = mpsc::channel();
        let lock = std::thread::spawn(move || {
            let result = crate::commands::lock_folder_inner(&lock_state, "f1".into());
            lock_done_tx.send(result).unwrap();
        });

        // Give the competing lock a bounded opportunity to expose the old gap. It must remain
        // blocked until the source-bearing response has crossed all command postwork.
        let early_lock = lock_done_rx.recv_timeout(Duration::from_millis(150)).ok();
        let lock_landed_early = early_lock.is_some();
        {
            let (released, wake) = &*release_postwork;
            *released.lock().unwrap() = true;
            wake.notify_all();
        }

        let accepted = accept.join().unwrap().unwrap();
        let lock_result = match early_lock {
            Some(result) => result,
            None => lock_done_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("competing lock did not finish after acceptance released lifecycle"),
        };
        lock_result.unwrap();
        lock.join().unwrap();

        assert!(
            !lock_landed_early,
            "a concurrent lock crossed the acceptance command's postwork boundary"
        );
        assert_eq!(accepted.sources.len(), 1);
        assert_eq!(accepted.sources[0].title, "Planning");
    }

    #[tokio::test]
    async fn locked_reminder_list_masks_sources_and_audit_accept_fail_closed() {
        let state = test_state("locked-paths", Arc::new(crate::reason::StubReasoner));
        seed_meeting_source(&state);
        let suggestions = audit_reminder_suggestions_inner(
            &state,
            "meeting".into(),
            "m1".into(),
            Arc::new(crate::reason::StubReasoner),
        )
        .await
        .unwrap();
        assert_eq!(suggestions.len(), 1);

        let mut stored_draft = reminder_draft();
        stored_draft.sources.push(ReminderSourceAnchor {
            kind: "meeting".into(),
            id: "m1".into(),
        });
        state
            .db
            .create_reminder("r1", &stored_draft, ReminderOrigin::Manual, 1)
            .unwrap();

        state
            .db
            .set_folder_locked("f1", true, Some(b"wrapped"))
            .unwrap();
        crate::commands::bump_seal_epoch(&state);

        let snapshot = reminders_snapshot_at(&state, 1_900_000_000_000).unwrap();
        assert_eq!(snapshot.upcoming.len(), 1);
        assert!(
            snapshot.upcoming[0].sources.is_empty(),
            "locked source title/id metadata must not cross the list projection"
        );
        assert!(matches!(
            audit_reminder_suggestions_inner(
                &state,
                "meeting".into(),
                "m1".into(),
                Arc::new(crate::reason::StubReasoner),
            )
            .await,
            Err(AppError::Locked(_))
        ));
        assert!(matches!(
            accept_reminder_suggestion_inner(&state, suggestions[0].id.clone(), reminder_draft()),
            Err(AppError::Locked(_))
        ));
        assert_eq!(
            state.db.list_stored_reminders().unwrap().len(),
            1,
            "locked acceptance must not promote a second reminder"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn inflight_non_text_segment_edit_cannot_persist_suggestions() {
        let calls = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let selector: Arc<dyn LocalReasoner> = Arc::new(BarrierSelector {
            calls: Arc::clone(&calls),
            started: Mutex::new(Some(started_tx)),
            release: Arc::clone(&release),
        });
        let state = test_state("inflight-edit", Arc::clone(&selector));
        seed_meeting_source(&state);

        let worker_state = Arc::clone(&state);
        let audit = tokio::spawn(async move {
            audit_reminder_suggestions_inner(&worker_state, "meeting".into(), "m1".into(), selector)
                .await
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        state
            .db
            .lock()
            .execute(
                "UPDATE segments SET start_s=start_s+0.5
                  WHERE meeting_id='m1' AND idx=0",
                [],
            )
            .unwrap();
        {
            let (released, wake) = &*release;
            *released.lock().unwrap() = true;
            wake.notify_all();
        }

        assert!(matches!(audit.await.unwrap(), Err(AppError::InvalidArg(_))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let derived: i64 = state
            .db
            .lock()
            .query_row(
                "SELECT
                   (SELECT COUNT(*) FROM reminder_audit_cache) +
                   (SELECT COUNT(*) FROM reminder_pending_suggestions)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(derived, 0);
    }

    #[test]
    fn fresh_lock_atomically_purges_all_reminder_audit_rows_and_keeps_accepted_reminder() {
        ensure_test_dev_kek();
        let state = test_state(
            "fresh-lock-reminder-purge",
            Arc::new(crate::reason::StubReasoner),
        );
        seed_meeting_source(&state);
        let content = read_reminder_audit_source_under_lifecycle(&state, "meeting", "m1").unwrap();
        let content_hash = reminder_audit_source_hash(&content);
        let seeded_candidates = crate::reminder_audit::build_candidates(
            "- [ ] First\n- [ ] Second\n- [ ] Third\n",
            &[],
        );
        state
            .db
            .replace_reminder_audit_results_unchecked(
                "meeting",
                "m1",
                &content_hash,
                "test-local",
                &seeded_candidates,
                10,
            )
            .unwrap();
        let rows = state
            .db
            .list_pending_reminder_suggestions(
                "meeting",
                "m1",
                &content_hash,
                "test-local",
                MAX_REMINDER_AUDIT_ROWS,
            )
            .unwrap();
        assert_eq!(rows.len(), 3);
        assert!(state
            .db
            .promote_pending_reminder_suggestion(ReminderSuggestionPromotion {
                suggestion_id: &rows[0].id,
                expected_source_kind: "meeting",
                expected_source_id: "m1",
                expected_content_hash: &content_hash,
                reminder_id: "accepted-before-fresh-lock",
                draft: &reminder_draft(),
                now: 11,
            })
            .unwrap());
        assert!(state
            .db
            .dismiss_pending_reminder_suggestion(&rows[1].id, "meeting", "m1", &content_hash)
            .unwrap());
        let before: (i64, i64, i64) = state
            .db
            .lock()
            .query_row(
                "SELECT
                   (SELECT COUNT(*) FROM reminder_pending_suggestions),
                   (SELECT COUNT(*) FROM reminder_audit_cache),
                   (SELECT COUNT(*) FROM reminder_suggestion_decisions)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            before,
            (1, 1, 2),
            "precondition requires pending plaintext plus accepted and dismissed fingerprints"
        );

        crate::commands::lock_folder_inner(&state, "f1".into()).unwrap();

        let after: (i64, i64, i64) = state
            .db
            .lock()
            .query_row(
                "SELECT
                   (SELECT COUNT(*) FROM reminder_pending_suggestions),
                   (SELECT COUNT(*) FROM reminder_audit_cache),
                   (SELECT COUNT(*) FROM reminder_suggestion_decisions)",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            after,
            (0, 0, 0),
            "the transaction publishing fresh locked=1 must withdraw every reminder audit row"
        );
        let accepted = state
            .db
            .get_stored_reminder("accepted-before-fresh-lock")
            .unwrap()
            .expect("accepted canonical reminder must survive fresh lock");
        assert!(matches!(accepted.origin, ReminderOrigin::Smart));
        assert!(accepted.sources.contains(&ReminderSourceAnchor {
            kind: "meeting".into(),
            id: "m1".into(),
        }));
    }

    #[test]
    fn initial_lock_notifies_after_gate_closes_but_before_plaintext_cleanup() {
        ensure_test_dev_kek();
        let state = test_state(
            "initial-lock-visibility-notice",
            Arc::new(crate::reason::StubReasoner),
        );
        seed_meeting_source(&state);
        let notified = AtomicBool::new(false);

        crate::commands::lock_folder_inner_with_visibility_notice(&state, "f1".into(), || {
            assert!(
                state.db.folder_by_id("f1").unwrap().unwrap().locked,
                "the durable visibility gate must close before renderer invalidation"
            );
            assert!(
                !state
                    .db
                    .latest_reminder_audit_markdown("m1")
                    .unwrap()
                    .unwrap()
                    .is_empty(),
                "the notice must precede the remaining fallible plaintext cleanup"
            );
            notified.store(true, Ordering::SeqCst);
        })
        .unwrap();

        assert!(notified.load(Ordering::SeqCst));
        assert_eq!(
            state
                .db
                .latest_reminder_audit_markdown("m1")
                .unwrap()
                .as_deref(),
            Some(""),
            "the verified seal still completes after the early invalidation"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn inflight_relock_boundary_cannot_persist_or_expose_suggestions() {
        ensure_test_dev_kek();
        let calls = Arc::new(AtomicUsize::new(0));
        let (started_tx, started_rx) = mpsc::channel();
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let selector: Arc<dyn LocalReasoner> = Arc::new(BarrierSelector {
            calls,
            started: Mutex::new(Some(started_tx)),
            release: Arc::clone(&release),
        });
        let state = test_state("inflight-relock", Arc::clone(&selector));
        seed_meeting_source(&state);
        crate::commands::lock_folder_inner(&state, "f1".into()).unwrap();
        let kek = crate::secrets::get_or_create_master_kek().unwrap();
        let wrapped = state.db.folder_wrapped_key("f1").unwrap().unwrap();
        let ck_bytes =
            crate::crypto::decrypt(&kek, &wrapped, &crate::commands::aad_wrapped_ck("f1")).unwrap();
        let ck: [u8; 32] = ck_bytes.try_into().unwrap();
        *state.master_kek.lock().unwrap() = Some(zeroize::Zeroizing::new(kek));
        for note in state.db.notes_in_folder("f1").unwrap() {
            let blob = note.content_blob.as_ref().unwrap();
            let markdown = String::from_utf8(
                crate::crypto::decrypt(
                    &ck,
                    blob,
                    &crate::commands::aad_content(
                        "f1",
                        &note.meeting_id,
                        &note.provider_id,
                        "note",
                    ),
                )
                .unwrap(),
            )
            .unwrap();
            state
                .db
                .restore_note_markdown(&note.meeting_id, &note.provider_id, &markdown)
                .unwrap();
        }
        crate::commands::unseal_folder_extras(&state, "f1", &ck, None).unwrap();
        state.unlocked_folders.lock().unwrap().insert("f1".into());
        let content = read_reminder_audit_source_under_lifecycle(&state, "meeting", "m1").unwrap();
        assert!(
            state
                .db
                .replace_reminder_audit_results(
                    "meeting",
                    "m1",
                    &reminder_audit_source_hash(&content),
                    "old-local-engine",
                    &candidates(),
                    1,
                )
                .unwrap(),
            "precondition: relock has source-derived plaintext to purge"
        );

        let worker_state = Arc::clone(&state);
        let audit = tokio::spawn(async move {
            audit_reminder_suggestions_inner(&worker_state, "meeting".into(), "m1".into(), selector)
                .await
        });
        started_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        crate::commands::relock_all_inner(&state).unwrap();
        assert_eq!(
            state
                .db
                .latest_reminder_audit_markdown("m1")
                .unwrap()
                .as_deref(),
            Some(""),
            "production relock must reblank meeting-note plaintext"
        );
        assert!(
            state
                .db
                .get_segments("m1")
                .unwrap()
                .iter()
                .all(|segment| segment.text.is_empty()),
            "production relock must reblank transcript plaintext"
        );
        {
            let (released, wake) = &*release;
            *released.lock().unwrap() = true;
            wake.notify_all();
        }

        assert!(matches!(audit.await.unwrap(), Err(AppError::Locked(_))));
        let derived: i64 = state
            .db
            .lock()
            .query_row(
                "SELECT
                   (SELECT COUNT(*) FROM reminder_audit_cache) +
                   (SELECT COUNT(*) FROM reminder_pending_suggestions)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(derived, 0);
    }

    #[test]
    fn relock_corrupt_retained_blob_preserves_every_plaintext_family() {
        let corruptions = [
            (
                "note",
                "UPDATE notes SET content_blob = x'00'
                  WHERE meeting_id = 'm1' AND provider_id = 'local'",
            ),
            (
                "segment",
                "UPDATE segments SET text_blob = x'00'
                  WHERE meeting_id = 'm1' AND idx = 0",
            ),
            (
                "timeline",
                "UPDATE timelines SET data_blob = x'00'
                  WHERE meeting_id = 'm1'",
            ),
            (
                "manual-notes",
                "UPDATE meetings SET manual_notes_blob = x'00'
                  WHERE id = 'm1'",
            ),
            (
                "document",
                "UPDATE documents SET text_blob = x'00'
                  WHERE id = 'doc1'",
            ),
            (
                "attachment",
                "UPDATE note_attachments SET data_blob = x'00'
                  WHERE id = '11111111-1111-4111-8111-111111111111'",
            ),
        ];

        for (family, corruption) in corruptions {
            let (state, _ck) =
                seed_session_unlocked_relock_state(&format!("relock-corrupt-{family}"));
            let expected = relock_plaintext(&state);
            state.db.lock().execute(corruption, []).unwrap();

            let error = crate::commands::relock_all_inner(&state)
                .expect_err("a corrupt retained blob must abort before the first blank");
            assert!(
                matches!(error, AppError::Other(_) | AppError::Storage(_)),
                "{family}: unexpected relock error: {error}"
            );
            assert_eq!(
                relock_plaintext(&state),
                expected,
                "{family}: relock failure must preserve every session plaintext family"
            );
        }
    }

    #[test]
    fn relock_valid_retained_blobs_round_trip_every_plaintext_family_byte_identically() {
        let (state, ck) = seed_session_unlocked_relock_state("relock-valid-round-trip");
        let expected = relock_plaintext(&state);

        crate::commands::relock_all_inner(&state).unwrap();
        assert_eq!(
            relock_plaintext(&state),
            RelockPlaintext {
                note: String::new(),
                segment: String::new(),
                timeline: String::new(),
                manual_notes: String::new(),
                document: String::new(),
                attachment: Vec::new(),
            },
            "a verified relock must blank every session plaintext family, including attachments"
        );
        restore_relock_plaintext_from_blobs(&state, &ck);

        assert_eq!(
            relock_plaintext(&state),
            expected,
            "all five retained ciphertext families must restore byte-identically"
        );
    }

    #[test]
    fn relock_audio_failure_retry_round_trips_playback_and_masters_byte_identically() {
        ensure_test_dev_kek();
        let state = test_state(
            "relock-audio-failure-retry",
            Arc::new(crate::reason::StubReasoner),
        );
        seed_meeting_source(&state);

        let directory = std::env::temp_dir().join(format!(
            "murmur-relock-audio-roundtrip-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let playback = directory.join("playback.wav");
        let mic = directory.join("mic-master.wav");
        let system = directory.join("system-master.wav");
        let expected_playback = b"RIFF-playback-byte-identical".to_vec();
        let expected_mic = b"RIFF-mic-master-byte-identical".to_vec();
        let expected_system = b"RIFF-system-master-byte-identical".to_vec();
        std::fs::write(&playback, &expected_playback).unwrap();
        std::fs::write(&mic, &expected_mic).unwrap();
        std::fs::write(&system, &expected_system).unwrap();
        state
            .db
            .set_meeting_audio_path("m1", Some(playback.to_string_lossy().as_ref()))
            .unwrap();
        state
            .db
            .set_meeting_mic_master_path("m1", Some(mic.to_string_lossy().as_ref()))
            .unwrap();
        state
            .db
            .set_meeting_sys_master_path("m1", Some(system.to_string_lossy().as_ref()))
            .unwrap();

        crate::commands::lock_folder_inner(&state, "f1".into()).unwrap();
        let kek = crate::secrets::get_or_create_master_kek().unwrap();
        let wrapped = state.db.folder_wrapped_key("f1").unwrap().unwrap();
        let ck_bytes =
            crate::crypto::decrypt(&kek, &wrapped, &crate::commands::aad_wrapped_ck("f1"))
                .unwrap();
        let ck: [u8; 32] = ck_bytes.try_into().unwrap();
        let retry_kek = kek;
        *state.master_kek.lock().unwrap() = Some(zeroize::Zeroizing::new(kek));

        let playback_enc = std::path::PathBuf::from(format!(
            "{}{}",
            playback.to_string_lossy(),
            crate::commands::ENC_SUFFIX
        ));
        crate::commands::unseal_folder_extras(&state, "f1", &ck, None).unwrap();
        state.unlocked_folders.lock().unwrap().insert("f1".into());

        assert_eq!(std::fs::read(&playback).unwrap(), expected_playback);
        assert_eq!(std::fs::read(&mic).unwrap(), expected_mic);
        assert_eq!(std::fs::read(&system).unwrap(), expected_system);

        std::fs::write(&playback_enc, b"corrupt retained playback ciphertext").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500)).unwrap();
        let failed_relock = crate::commands::relock_all_inner(&state);
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        failed_relock
            .expect_err("corrupt retained audio must abort before deleting session plaintext");
        assert_eq!(std::fs::read(&playback).unwrap(), expected_playback);
        assert_eq!(std::fs::read(&mic).unwrap(), expected_mic);
        assert_eq!(std::fs::read(&system).unwrap(), expected_system);

        *state.master_kek.lock().unwrap() = Some(zeroize::Zeroizing::new(retry_kek));
        state.unlocked_folders.lock().unwrap().insert("f1".into());
        crate::commands::relock_all_inner(&state).unwrap();
        assert!(!playback.exists());
        assert!(!mic.exists());
        assert!(!system.exists());

        crate::commands::unseal_folder_extras(&state, "f1", &ck, None).unwrap();
        assert_eq!(std::fs::read(&playback).unwrap(), expected_playback);
        assert_eq!(std::fs::read(&mic).unwrap(), expected_mic);
        assert_eq!(std::fs::read(&system).unwrap(), expected_system);

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn relock_repairs_blobless_partial_fresh_seals_before_success() {
        let partial_seals = [
            (
                "note",
                "UPDATE notes SET content_blob = NULL
                  WHERE meeting_id = 'm1' AND provider_id = 'local'",
                "SELECT content_blob IS NOT NULL FROM notes
                  WHERE meeting_id = 'm1' AND provider_id = 'local'",
            ),
            (
                "timeline",
                "UPDATE timelines SET data_blob = NULL WHERE meeting_id = 'm1'",
                "SELECT data_blob IS NOT NULL FROM timelines WHERE meeting_id = 'm1'",
            ),
            (
                "manual-notes",
                "UPDATE meetings SET manual_notes_blob = NULL WHERE id = 'm1'",
                "SELECT manual_notes_blob IS NOT NULL FROM meetings WHERE id = 'm1'",
            ),
            (
                "document",
                "UPDATE documents SET text_blob = NULL WHERE id = 'doc1'",
                "SELECT text_blob IS NOT NULL FROM documents WHERE id = 'doc1'",
            ),
            (
                "attachment",
                "UPDATE note_attachments SET data_blob = NULL
                  WHERE id = '11111111-1111-4111-8111-111111111111'",
                "SELECT data_blob IS NOT NULL FROM note_attachments
                  WHERE id = '11111111-1111-4111-8111-111111111111'",
            ),
        ];

        for (family, make_partial, blob_present) in partial_seals {
            let (state, ck) =
                seed_session_unlocked_relock_state(&format!("relock-partial-{family}"));
            let expected = relock_plaintext(&state);
            state.db.lock().execute(make_partial, []).unwrap();

            crate::commands::relock_all_inner(&state).unwrap();
            assert_eq!(
                relock_plaintext(&state),
                RelockPlaintext {
                    note: String::new(),
                    segment: String::new(),
                    timeline: String::new(),
                    manual_notes: String::new(),
                    document: String::new(),
                    attachment: Vec::new(),
                },
                "{family}: successful relock must leave no plaintext family readable at rest"
            );
            let repaired: bool = state
                .db
                .lock()
                .query_row(blob_present, [], |row| row.get(0))
                .unwrap();
            assert!(
                repaired,
                "{family}: relock must persist the decrypt-verified replacement blob"
            );

            restore_relock_plaintext_from_blobs(&state, &ck);
            assert_eq!(
                relock_plaintext(&state),
                expected,
                "{family}: repaired seal must restore every family byte-identically"
            );
        }
    }

    #[test]
    fn single_folder_reblank_helper_rejects_corrupt_attachment_before_any_plaintext_blank() {
        let (state, _ck) = seed_session_unlocked_relock_state("single-folder-attachment-corrupt");
        let expected = relock_plaintext(&state);
        state
            .db
            .lock()
            .execute(
                "UPDATE note_attachments SET data_blob=x'00' WHERE id=?1",
                rusqlite::params![RELOCK_ATTACHMENT_ID],
            )
            .unwrap();

        crate::commands::reblank_folder_extras(&state, "f1")
            .expect_err("single-folder relock must reject corrupt attachment ciphertext");
        assert_eq!(
            relock_plaintext(&state),
            expected,
            "single-folder verification failure must precede every plaintext blank"
        );
    }

    #[test]
    fn relock_rejects_attachment_with_neither_plaintext_nor_blob_and_preserves_other_plaintext() {
        let (state, _ck) = seed_session_unlocked_relock_state("attachment-no-recovery-copy");
        let expected = relock_plaintext(&state);
        state
            .db
            .lock()
            .execute(
                "UPDATE note_attachments
                    SET data=X'', data_blob=NULL, exported_path=NULL
                  WHERE id=?1",
                rusqlite::params![RELOCK_ATTACHMENT_ID],
            )
            .unwrap();

        crate::commands::relock_all_inner(&state)
            .expect_err("a governed attachment with no recovery copy must fail closed");
        let actual = relock_plaintext(&state);
        assert_eq!(actual.note, expected.note);
        assert_eq!(actual.segment, expected.segment);
        assert_eq!(actual.timeline, expected.timeline);
        assert_eq!(actual.manual_notes, expected.manual_notes);
        assert_eq!(actual.document, expected.document);
        assert!(actual.attachment.is_empty());
        let row = state
            .db
            .attachments_in_folder("f1")
            .unwrap()
            .into_iter()
            .find(|attachment| attachment.id == RELOCK_ATTACHMENT_ID)
            .unwrap();
        assert!(row.data_blob.is_none());
        assert!(row.exported_path.is_none());
    }

    #[test]
    fn relock_all_preflights_every_folder_before_corrupt_attachment_can_blank_a_sibling() {
        let (state, _first_ck) =
            seed_session_unlocked_relock_state("multi-folder-attachment-corrupt");
        let first_expected = relock_plaintext(&state);
        let _second_ck = seed_second_session_unlocked_folder(&state);
        let second_note_expected = state.db.notes_in_folder("f2").unwrap().remove(0).markdown;
        let second_attachment_expected =
            state.db.attachments_in_folder("f2").unwrap().remove(0).data;
        state
            .db
            .lock()
            .execute(
                "UPDATE note_attachments SET data_blob=x'00' WHERE id=?1",
                rusqlite::params![SECOND_RELOCK_ATTACHMENT_ID],
            )
            .unwrap();

        crate::commands::relock_all_inner(&state)
            .expect_err("one corrupt attachment must abort the all-folder preflight");
        assert_eq!(
            relock_plaintext(&state),
            first_expected,
            "a sibling folder encountered earlier must retain all of its plaintext"
        );
        assert_eq!(
            state.db.notes_in_folder("f2").unwrap().remove(0).markdown,
            second_note_expected
        );
        assert_eq!(
            state.db.attachments_in_folder("f2").unwrap().remove(0).data,
            second_attachment_expected
        );
    }

    #[test]
    fn startup_repairs_blobless_attachment_before_blanking_and_restores_exact_bytes() {
        let (state, ck) = seed_session_unlocked_relock_state("startup-attachment-blobless");
        let expected = relock_attachment_bytes();
        state
            .db
            .lock()
            .execute(
                "UPDATE note_attachments SET data_blob=NULL WHERE id=?1",
                rusqlite::params![RELOCK_ATTACHMENT_ID],
            )
            .unwrap();
        state.unlocked_folders.lock().unwrap().clear();

        assert!(
            crate::commands::locked_folder_requires_authenticated_repair(&state.db, "f1").unwrap()
        );
        crate::commands::repair_locked_folder_at_rest(&state.db, "f1", &ck).unwrap();
        assert!(
            !crate::commands::locked_folder_requires_authenticated_repair(&state.db, "f1").unwrap(),
            "startup repair must reach a fully sealed attachment shape before reporting success"
        );
        let sealed = state
            .db
            .attachments_in_folder("f1")
            .unwrap()
            .into_iter()
            .find(|attachment| attachment.id == RELOCK_ATTACHMENT_ID)
            .unwrap();
        assert!(sealed.data.is_empty());
        assert!(sealed.data_blob.is_some());

        crate::commands::unseal_attachments_in_folder(&state, "f1", &ck, false).unwrap();
        let restored = state
            .db
            .attachments_in_folder("f1")
            .unwrap()
            .into_iter()
            .find(|attachment| attachment.id == RELOCK_ATTACHMENT_ID)
            .unwrap();
        assert_eq!(restored.data, expected);
    }

    #[test]
    fn startup_authenticates_retained_attachment_before_cleaning_export_residue() {
        let (state, ck) = seed_session_unlocked_relock_state("startup-attachment-export-valid");
        let expected = relock_attachment_bytes();
        let export = std::env::temp_dir().join(format!(
            "murmur-reminder-relock-valid-{}.webp",
            uuid::Uuid::new_v4()
        ));
        std::fs::write(&export, &expected).unwrap();
        state
            .db
            .lock()
            .execute(
                "UPDATE note_attachments SET data=X'', exported_path=?2 WHERE id=?1",
                rusqlite::params![RELOCK_ATTACHMENT_ID, export.to_string_lossy().as_ref()],
            )
            .unwrap();
        state.unlocked_folders.lock().unwrap().clear();

        assert!(
            crate::commands::locked_folder_requires_authenticated_repair(&state.db, "f1").unwrap(),
            "an export path alone must force authenticated startup repair"
        );
        crate::commands::repair_locked_folder_at_rest(&state.db, "f1", &ck).unwrap();
        assert!(!export.exists());
        let sealed = state
            .db
            .attachments_in_folder("f1")
            .unwrap()
            .into_iter()
            .find(|attachment| attachment.id == RELOCK_ATTACHMENT_ID)
            .unwrap();
        assert!(sealed.data.is_empty());
        assert!(sealed.data_blob.is_some());
        assert!(sealed.exported_path.is_none());
        assert!(
            !crate::commands::locked_folder_requires_authenticated_repair(&state.db, "f1").unwrap()
        );

        crate::commands::unseal_attachments_in_folder(&state, "f1", &ck, false).unwrap();
        assert_eq!(
            state
                .db
                .attachments_in_folder("f1")
                .unwrap()
                .into_iter()
                .find(|attachment| attachment.id == RELOCK_ATTACHMENT_ID)
                .unwrap()
                .data,
            expected
        );
    }

    #[test]
    fn startup_corrupt_or_blobless_attachment_export_aborts_without_deleting_residue() {
        for (case, blob_sql) in [("corrupt", "x'00'"), ("missing", "NULL")] {
            let (state, ck) =
                seed_session_unlocked_relock_state(&format!("startup-attachment-export-{case}"));
            let expected = relock_plaintext(&state);
            let export = std::env::temp_dir().join(format!(
                "murmur-reminder-relock-{case}-{}.webp",
                uuid::Uuid::new_v4()
            ));
            std::fs::write(&export, relock_attachment_bytes()).unwrap();
            let sql = format!(
                "UPDATE note_attachments
                    SET data=X'', data_blob={blob_sql}, exported_path=?2
                  WHERE id=?1"
            );
            state
                .db
                .lock()
                .execute(
                    &sql,
                    rusqlite::params![RELOCK_ATTACHMENT_ID, export.to_string_lossy().as_ref()],
                )
                .unwrap();
            state.unlocked_folders.lock().unwrap().clear();

            assert!(
                crate::commands::locked_folder_requires_authenticated_repair(&state.db, "f1")
                    .unwrap(),
                "{case}: tracked export residue must force authenticated startup repair"
            );
            crate::commands::repair_locked_folder_at_rest(&state.db, "f1", &ck)
                .expect_err("unrecoverable attachment export must fail before cleanup");
            assert!(export.exists(), "{case}: export residue must be preserved");
            let row = state
                .db
                .attachments_in_folder("f1")
                .unwrap()
                .into_iter()
                .find(|attachment| attachment.id == RELOCK_ATTACHMENT_ID)
                .unwrap();
            assert!(row.data.is_empty());
            assert!(row.exported_path.is_some());
            assert_eq!(
                RelockPlaintext {
                    attachment: expected.attachment.clone(),
                    ..relock_plaintext(&state)
                },
                expected,
                "{case}: attachment preflight must preserve every other plaintext family"
            );
            std::fs::remove_file(export).unwrap();
        }
    }

    #[test]
    fn full_hash_rejects_edits_beyond_the_bounded_candidate_prefix() {
        let mut segments = (0..=MAX_AUDIT_SEGMENTS)
            .map(|idx| Segment {
                idx: idx as i64,
                start_s: idx as f64,
                end_s: idx as f64 + 0.5,
                text: format!("segment {idx}"),
                speaker: None,
                confidence: None,
            })
            .collect::<Vec<_>>();
        let before = ReminderAuditSourceContent {
            markdown: "- [ ] Keep the bounded candidate\n".into(),
            manual_notes: String::new(),
            segments: segments.clone(),
            source: ReminderSourceView {
                kind: "meeting".into(),
                id: "m1".into(),
                title: "Meeting".into(),
            },
        };
        let expected_hash = reminder_audit_source_hash(&before);
        segments[MAX_AUDIT_SEGMENTS].text = "edited outside candidate prefix".into();
        let after = ReminderAuditSourceContent {
            markdown: before.markdown.clone(),
            manual_notes: before.manual_notes.clone(),
            segments,
            source: before.source.clone(),
        };

        let bounded_before = bounded_audit_segments(&before.segments);
        let bounded_after = bounded_audit_segments(&after.segments);
        assert_eq!(
            bounded_before
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>(),
            bounded_after
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>(),
            "candidate extraction must stay bounded before the edited tail"
        );
        assert!(
            require_reminder_audit_hash(&after, &expected_hash).is_err(),
            "the full canonical hash must still reject the source edit"
        );
    }

    #[test]
    fn audit_hash_covers_source_title_and_manual_notes() {
        let content = ReminderAuditSourceContent {
            markdown: "Summary".into(),
            manual_notes: "- [ ] Follow up".into(),
            segments: Vec::new(),
            source: ReminderSourceView {
                kind: "meeting".into(),
                id: "m1".into(),
                title: "Planning".into(),
            },
        };
        let hash = reminder_audit_source_hash(&content);

        let mut title_edit = ReminderAuditSourceContent {
            markdown: content.markdown.clone(),
            manual_notes: content.manual_notes.clone(),
            segments: content.segments.clone(),
            source: content.source.clone(),
        };
        title_edit.source.title = "Renamed planning".into();
        assert_ne!(reminder_audit_source_hash(&title_edit), hash);

        let mut manual_edit = title_edit;
        manual_edit.source.title = content.source.title.clone();
        manual_edit.manual_notes = "- [ ] Follow up today".into();
        assert_ne!(reminder_audit_source_hash(&manual_edit), hash);

        let note = ReminderAuditSourceContent {
            markdown: "Body".into(),
            manual_notes: String::new(),
            segments: Vec::new(),
            source: ReminderSourceView {
                kind: "note".into(),
                id: "n1".into(),
                title: "Authored title".into(),
            },
        };
        let note_hash = reminder_audit_source_hash(&note);
        let mut renamed_note = ReminderAuditSourceContent {
            markdown: note.markdown.clone(),
            manual_notes: String::new(),
            segments: Vec::new(),
            source: note.source,
        };
        renamed_note.source.title = "Renamed authored title".into();
        assert_ne!(reminder_audit_source_hash(&renamed_note), note_hash);
    }

    #[test]
    fn audit_hash_covers_every_canonical_segment_field() {
        let content = ReminderAuditSourceContent {
            markdown: "Summary".into(),
            manual_notes: String::new(),
            segments: vec![Segment {
                idx: 7,
                start_s: 1.25,
                end_s: 2.5,
                text: "Follow up".into(),
                speaker: Some("me".into()),
                confidence: Some(0.75),
            }],
            source: ReminderSourceView {
                kind: "meeting".into(),
                id: "m1".into(),
                title: "Planning".into(),
            },
        };
        let original = reminder_audit_source_hash(&content);
        let mutations: [fn(&mut Segment); 6] = [
            |segment: &mut Segment| segment.idx += 1,
            |segment: &mut Segment| segment.start_s += 0.25,
            |segment: &mut Segment| segment.end_s += 0.25,
            |segment: &mut Segment| segment.text.push('!'),
            |segment: &mut Segment| segment.speaker = Some("others".into()),
            |segment: &mut Segment| segment.confidence = Some(0.5),
        ];
        for mutate in mutations {
            let mut edited = ReminderAuditSourceContent {
                markdown: content.markdown.clone(),
                manual_notes: content.manual_notes.clone(),
                segments: content.segments.clone(),
                source: content.source.clone(),
            };
            mutate(&mut edited.segments[0]);
            assert_ne!(reminder_audit_source_hash(&edited), original);
        }
    }

    #[test]
    fn meeting_manual_notes_feed_deterministic_candidates() {
        let content = ReminderAuditSourceContent {
            markdown: "No checklist in the generated note.".into(),
            manual_notes: "## Action items\n- [ ] Call the customer".into(),
            segments: Vec::new(),
            source: ReminderSourceView {
                kind: "meeting".into(),
                id: "m1".into(),
                title: "Planning".into(),
            },
        };
        let candidates =
            crate::reminder_audit::build_candidates(&reminder_candidate_markdown(&content), &[]);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].title, "Call the customer");
    }

    #[test]
    fn candidate_markdown_is_bounded_and_reserves_manual_note_space() {
        let content = ReminderAuditSourceContent {
            markdown: "generated line\n".repeat(MAX_AUDIT_MARKDOWN_BYTES),
            manual_notes: "- [ ] Manual follow up\n".repeat(10_000),
            segments: Vec::new(),
            source: ReminderSourceView {
                kind: "meeting".into(),
                id: "m1".into(),
                title: "Planning".into(),
            },
        };
        let bounded = reminder_candidate_markdown(&content);
        assert!(bounded.len() <= MAX_AUDIT_MARKDOWN_BYTES);
        assert!(bounded.contains("Manual follow up"));
    }

    #[test]
    fn hidden_source_merge_preserves_locked_anchor_and_allows_visible_removal() {
        let mut submitted = vec![ReminderSourceAnchor {
            kind: "meeting".into(),
            id: "visible-kept".into(),
        }];
        merge_hidden_source_anchors(
            &mut submitted,
            BTreeSet::from([("note".to_string(), "locked-note".to_string())]),
        );
        assert_eq!(
            submitted,
            vec![
                ReminderSourceAnchor {
                    kind: "meeting".into(),
                    id: "visible-kept".into(),
                },
                ReminderSourceAnchor {
                    kind: "note".into(),
                    id: "locked-note".into(),
                }
            ]
        );
        assert!(
            !submitted
                .iter()
                .any(|source| source.id == "visible-removed"),
            "an omitted visible source must remain explicitly removable"
        );
    }
}

#[cfg(test)]
mod reminder_script_tests {
    use super::{build_reminder_script, escape_applescript, parse_iso_ymd};

    #[test]
    fn parses_strict_iso_only() {
        assert_eq!(parse_iso_ymd("2026-07-01"), Some((2026, 7, 1)));
        assert_eq!(parse_iso_ymd(" 2026-12-31 "), Some((2026, 12, 31)));
        assert_eq!(parse_iso_ymd("2026-13-01"), None); // month out of range
        assert_eq!(parse_iso_ymd("2026-07-32"), None); // day out of range
        assert_eq!(parse_iso_ymd("2026/07/01"), None); // wrong separators
        assert_eq!(parse_iso_ymd("26-07-01"), None); // not 4-digit year
        assert_eq!(parse_iso_ymd(""), None);
    }

    #[test]
    fn due_date_sets_the_date_properties() {
        let s = build_reminder_script("Ship the deck", Some("2026-07-01"));
        // The date is actually attached now (the bug was: only `name` was set).
        assert!(s.contains("set year of theDate to 2026"));
        assert!(s.contains("set month of theDate to 7"));
        assert!(s.contains("set day of theDate to 1"));
        assert!(s.contains("remind me date:theDate"));
        assert!(s.contains("due date:theDate"));
        assert!(s.contains("name:\"Ship the deck\""));
        // `day` is reset to 1 BEFORE year/month so a month change can't overflow the day.
        let reset = s.find("set day of theDate to 1").unwrap();
        let yr = s.find("set year of theDate").unwrap();
        assert!(
            reset < yr,
            "day must be reset to 1 before changing year/month"
        );
    }

    #[test]
    fn no_due_date_is_name_only() {
        let s = build_reminder_script("Call Bob", None);
        assert!(s.contains("name:\"Call Bob\""));
        assert!(!s.contains("due date"));
        assert!(!s.contains("theDate"));
    }

    #[test]
    fn invalid_due_date_falls_back_to_name_only() {
        let s = build_reminder_script("Task", Some("not-a-date"));
        assert!(
            !s.contains("due date"),
            "an unparseable date must not produce date props"
        );
        assert!(s.contains("name:\"Task\""));
    }

    #[test]
    fn item_text_cannot_break_out_of_the_applescript_literal() {
        // A name carrying a quote + a forged statement must stay INSIDE the string literal: the
        // `"` is escaped to `\"`, so `end tell` / the injected `make` never become real statements.
        let evil =
            "pwn\", remind me date:theDate}\nend tell\ntell application \"Finder\" to delete";
        let esc = escape_applescript(evil);
        assert!(
            !esc.contains('\n'),
            "raw newlines flattened (literals can't span lines)"
        );
        // Every `"` in the payload is preceded by a backslash — no bare quote survives to close
        // the literal early. (Checked by scanning: each `"` byte has a `\` immediately before it.)
        let bytes = esc.as_bytes();
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'"' {
                assert!(
                    i > 0 && bytes[i - 1] == b'\\',
                    "unescaped quote survived at {i}"
                );
            }
        }
        let s = build_reminder_script(evil, Some("2026-07-01"));
        // The ONE real `tell` statement (unescaped quotes around Reminders) is intact...
        assert!(
            s.contains("tell application \"Reminders\""),
            "the real Reminders statement must survive"
        );
        // ...and the injected Finder `tell` never becomes real code: its quotes are escaped, so it
        // stays as inert data inside the name literal (no `tell application "Finder"` with REAL quotes).
        assert!(
            !s.contains("tell application \"Finder\""),
            "injected statement must remain escaped data, not executable code"
        );
        // The whole program is a single line (newlines in the payload were flattened), so a forged
        // `end tell` can never start its own statement line.
        assert!(
            !s.lines().any(|l| l.trim() == "end tell"),
            "no standalone injected `end tell` statement line"
        );
        // Every embedded double-quote from the payload is backslash-escaped in the program.
        assert!(s.contains("\\\""), "payload quotes are escaped");
    }
}
