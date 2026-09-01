use std::path::Path;
use std::process::Stdio;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

use crate::error::AppError;
use crate::summarize::provider::*;
use crate::summarize::template;

/// Hard wall-clock ceiling for a single `claude -p` run. A wedged CLI (network stall, hung
/// node child) is killed rather than blocking the pipeline forever (F6).
const CLAUDE_TIMEOUT: Duration = Duration::from_secs(180);

/// Separate bounded budget for SIGKILL + reap and for draining already-closed output pipes. The
/// execution deadline never turns into a second unbounded wait during cleanup.
const CLAUDE_REAP_TIMEOUT: Duration = Duration::from_secs(5);
const CLAUDE_AVAILABILITY_TIMEOUT: Duration = Duration::from_secs(5);
const CLAUDE_STDOUT_LIMIT_BYTES: usize = 8 * 1024 * 1024;
const CLAUDE_STDERR_LIMIT_BYTES: usize = 1024 * 1024;

#[cfg(unix)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum ClaudeGroupState {
    Active,
    Unproven,
}

#[cfg(unix)]
fn claude_process_groups() -> &'static Mutex<std::collections::HashMap<i32, ClaudeGroupState>> {
    static GROUPS: OnceLock<Mutex<std::collections::HashMap<i32, ClaudeGroupState>>> =
        OnceLock::new();
    GROUPS.get_or_init(|| Mutex::new(std::collections::HashMap::new()))
}

struct ClaudeProcessGroup {
    #[cfg(unix)]
    pgid: i32,
    proven_dead: bool,
}

impl ClaudeProcessGroup {
    fn register(child: &Child) -> Self {
        #[cfg(unix)]
        {
            let pgid = child
                .id()
                .and_then(|pid| i32::try_from(pid).ok())
                .unwrap_or(0);
            if pgid > 0 {
                claude_process_groups()
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(pgid, ClaudeGroupState::Active);
            }
            Self {
                pgid,
                proven_dead: false,
            }
        }
        #[cfg(not(unix))]
        {
            let _ = child;
            Self { proven_dead: false }
        }
    }

    fn pgid(&self) -> Option<i32> {
        #[cfg(unix)]
        {
            (self.pgid > 0).then_some(self.pgid)
        }
        #[cfg(not(unix))]
        {
            None
        }
    }

    fn mark_proven_dead(&mut self) {
        #[cfg(unix)]
        if self.pgid > 0 {
            claude_process_groups()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&self.pgid);
        }
        self.proven_dead = true;
    }
}

impl Drop for ClaudeProcessGroup {
    fn drop(&mut self) {
        if self.proven_dead {
            return;
        }
        #[cfg(unix)]
        if self.pgid > 0 {
            // If recording cancellation already removed this exact PGID after proving the group
            // empty, do not resurrect it. Otherwise preserve an unproven marker after cancellation,
            // panic, or teardown failure so later egress/recording admission fails closed.
            if let Some(state) = claude_process_groups()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get_mut(&self.pgid)
            {
                *state = ClaudeGroupState::Unproven;
            }
        }
    }
}

#[cfg(unix)]
pub(crate) fn isolate_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.as_std_mut().process_group(0);
}

#[cfg(not(unix))]
pub(crate) fn isolate_process_group(_command: &mut Command) {}

/// Classify the errno of a `kill(-pgid, …)` that delivered to nobody.
///
/// `true` = the attempt found nothing to signal, which is NOT a teardown failure: the caller
/// proceeds to the liveness proof, the only step allowed to conclude a group is dead. `false` = an
/// errno we cannot explain, which stays a hard error.
///
/// Both benign codes name the same kernel outcome — "no member of this group received the signal":
///
/// * ESRCH: the group is already absent.
/// * EPERM: macOS reports "the group iterate found no eligible member" as EPERM, which is not the
///   claim "you may not kill this group". Measured under CPU starvation (6 failures in 40 runs of
///   `a_timed_out_child_leaves_no_unproven_process_group`), the direct child was simultaneously
///   invisible to `kill(pid, 0)` and to `getpgid(pid)` — both ESRCH, with `ps` listing nothing in
///   the group — while `waitpid(WNOHANG)` still reported it running. Every process we spawn runs
///   as our own uid, so a genuine permission denial is not reachable on this path.
///
/// Treating EPERM as fatal is what put this class in CI: teardown returned early, `mark_proven_dead`
/// never ran, and `Drop` left the group `Unproven`. That marker is sticky and `perf::…` refuses
/// recording admission while any group carries it, so a teardown that merely raced a child's
/// creation or exit could cost a user the ability to record until the app restarted.
///
/// This does not weaken the invariant. Proving a group dead still requires `process_group_is_alive`
/// to report ESRCH; EPERM there still counts as ALIVE, so no group is ever marked proven-dead on
/// the strength of this errno.
#[cfg(unix)]
fn group_signal_errno_is_benign(raw: Option<i32>) -> bool {
    matches!(raw, Some(3) | Some(1))
}

#[cfg(unix)]
fn signal_process_group(pgid: i32, signal: i32) -> crate::error::Result<()> {
    extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    // SAFETY: `pgid` is the positive PID returned for a child spawned into its own process group.
    // A negative pid targets that group; no pointer crosses the FFI boundary.
    if unsafe { kill(-pgid, signal) } == 0 {
        return Ok(());
    }
    let error = std::io::Error::last_os_error();
    if group_signal_errno_is_benign(error.raw_os_error()) {
        Ok(())
    } else {
        Err(AppError::Summarize(format!(
            "failed signaling external cloud CLI process group: {error}"
        )))
    }
}

#[cfg(unix)]
fn process_group_is_alive(pgid: i32) -> crate::error::Result<bool> {
    extern "C" {
        fn kill(pid: i32, signal: i32) -> i32;
    }
    // SAFETY: signal 0 performs only an existence/permission check for the owned process group.
    if unsafe { kill(-pgid, 0) } == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(3) => Ok(false), // ESRCH
        Some(1) => Ok(true),  // EPERM still proves a group exists.
        _ => Err(AppError::Summarize(format!(
            "failed checking external cloud CLI process group: {error}"
        ))),
    }
}

#[cfg(not(unix))]
fn process_group_is_alive(_pgid: i32) -> crate::error::Result<bool> {
    Ok(false)
}

pub(crate) fn has_unproven_process_group() -> bool {
    #[cfg(unix)]
    {
        claude_process_groups()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .values()
            .any(|state| *state == ClaudeGroupState::Unproven)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

type PipeReadTask = tokio::task::JoinHandle<std::io::Result<Vec<u8>>>;

fn spawn_pipe_reader<R>(mut pipe: R, limit: usize) -> PipeReadTask
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 8192];
        let mut exceeded = false;
        loop {
            let read = pipe.read(&mut chunk).await?;
            if read == 0 {
                return if exceeded {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "external cloud CLI output exceeded bounded limit",
                    ))
                } else {
                    Ok(bytes)
                };
            }
            let remaining = limit.saturating_sub(bytes.len());
            let retained = read.min(remaining);
            bytes.extend_from_slice(&chunk[..retained]);
            if retained < read {
                exceeded = true;
            }
            // Keep draining after the cap so a full pipe cannot prevent process exit/reap.
        }
    })
}

async fn kill_and_prove_group_dead(
    pgid: Option<i32>,
    deadline: tokio::time::Instant,
) -> crate::error::Result<()> {
    let Some(pgid) = pgid else {
        return Ok(());
    };
    loop {
        // Re-deliver on every pass rather than once before the loop. A member can become visible
        // to `kill(2)` AFTER the first delivery — a process still being created is invisible to
        // both the group signal and `getpgid`, and the kernel reports that as "nothing was
        // signaled", not as an error. A single up-front SIGKILL would miss it and then wait out
        // the deadline; re-sending costs one syscall per 10 ms and cannot miss the window.
        #[cfg(unix)]
        signal_process_group(pgid, 9)?;
        if !process_group_is_alive(pgid)? {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(AppError::Summarize(
                "external cloud CLI process group remained alive after teardown deadline".into(),
            ));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Recording-priority cancellation for active OR previously unproven external cloud CLI groups.
/// A CLI may have descendants holding pipes/network after its direct parent exits; Start may
/// proceed only after every isolated group is proven absent.
pub(crate) async fn kill_for_recording(timeout: Duration) -> crate::error::Result<bool> {
    #[cfg(not(unix))]
    {
        let _ = timeout;
        return Ok(true);
    }
    #[cfg(unix)]
    {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let groups = claude_process_groups()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .keys()
                .copied()
                .collect::<Vec<_>>();
            if groups.is_empty() {
                return Ok(true);
            }
            for pgid in groups {
                signal_process_group(pgid, 9)?;
                if !process_group_is_alive(pgid)? {
                    claude_process_groups()
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .remove(&pgid);
                }
            }
            if claude_process_groups()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
            {
                return Ok(true);
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(false);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

async fn kill_and_reap_external_cli(
    child: &mut Child,
    group: &mut ClaudeProcessGroup,
    provider_label: &str,
) -> crate::error::Result<()> {
    let deadline = tokio::time::Instant::now() + CLAUDE_REAP_TIMEOUT;
    let pre_kill_error = match child.try_wait() {
        Ok(Some(_)) => None,
        Ok(None) => None,
        // Still attempt kill when status observation itself fails. If teardown also fails, retain
        // this diagnostic rather than abandoning a potentially live cloud CLI child.
        Err(error) => Some(error),
    };

    #[cfg(unix)]
    if let Some(pgid) = group.pgid() {
        signal_process_group(pgid, 9)?;
    }
    match child.start_kill() {
        Ok(()) => {}
        // Nothing was left to signal: the child already exited (std records that as InvalidInput)
        // or its pid is already gone (ESRCH). Both are the ordinary end of a wedged child that
        // lost the race with our own deadline, not a failure to tear it down — fall through to the
        // reap and the group proof, which are the steps that actually decide.
        Err(kill_error)
            if kill_error.kind() == std::io::ErrorKind::InvalidInput
                || kill_error.raw_os_error() == Some(3) => {}
        Err(kill_error) => {
            return match child.try_wait() {
                Ok(Some(_)) => {
                    kill_and_prove_group_dead(group.pgid(), deadline).await?;
                    group.mark_proven_dead();
                    Ok(())
                }
                Ok(None) => Err(AppError::Summarize(match pre_kill_error {
                    Some(precheck) => format!(
                        "failed checking {provider_label} status before teardown: {precheck}; failed to kill {provider_label}: {kill_error}"
                    ),
                    None => format!("failed to kill {provider_label} after deadline: {kill_error}"),
                })),
                Err(wait_error) => Err(AppError::Summarize(format!(
                    "failed to kill {provider_label} after deadline: {kill_error}; status check failed: {wait_error}"
                ))),
            };
        }
    }

    let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
    match tokio::time::timeout(remaining, child.wait()).await {
        Ok(Ok(_)) => {
            // A FRESH budget, not the remainder of the one `child.wait()` just spent. These are
            // two different waits on two different things: reaping OUR direct child, and then
            // proving the whole isolated group empty — which depends on the OS reaping
            // grandchildren the CLI spawned and that our kill orphaned. `kill(-pgid, 0)` reports
            // a zombie as alive, so the group can look occupied for a moment after every process
            // in it is dead, and that moment is not ours to control.
            //
            // Sharing one deadline meant a slow child reap silently ate the group proof's entire
            // budget: `child.wait()` taking 4.9s of 5s left the proof 100ms, it timed out, and
            // the group was marked Unproven — permanently, since that marker is sticky. That is
            // not a cosmetic failure. `perf::…` refuses recording admission while any group is
            // unproven, so a teardown that merely ran late could leave a user unable to start
            // recording until the app restarted.
            kill_and_prove_group_dead(
                group.pgid(),
                tokio::time::Instant::now() + CLAUDE_REAP_TIMEOUT,
            )
            .await?;
            group.mark_proven_dead();
            Ok(())
        }
        Ok(Err(error)) => Err(AppError::Summarize(format!(
            "failed to reap {provider_label} after kill: {error}"
        ))),
        Err(_) => Err(AppError::Summarize(format!(
            "{provider_label} did not reap within {}s after kill",
            CLAUDE_REAP_TIMEOUT.as_secs()
        ))),
    }
}

async fn collect_external_cli_output(
    mut stdout_task: PipeReadTask,
    mut stderr_task: PipeReadTask,
    provider_label: &str,
) -> crate::error::Result<(Vec<u8>, Vec<u8>)> {
    let drains = async { tokio::join!(&mut stdout_task, &mut stderr_task) };

    match tokio::time::timeout(CLAUDE_REAP_TIMEOUT, drains).await {
        Ok((stdout, stderr)) => {
            let stdout = stdout
                .map_err(|error| {
                    AppError::Summarize(format!("{provider_label} stdout reader failed: {error}"))
                })?
                .map_err(|error| {
                    AppError::Summarize(format!("failed reading {provider_label} stdout: {error}"))
                })?;
            let stderr = stderr
                .map_err(|error| {
                    AppError::Summarize(format!("{provider_label} stderr reader failed: {error}"))
                })?
                .map_err(|error| {
                    AppError::Summarize(format!("failed reading {provider_label} stderr: {error}"))
                })?;
            Ok((stdout, stderr))
        }
        Err(_) => {
            stdout_task.abort();
            stderr_task.abort();
            Err(AppError::Summarize(format!(
                "{provider_label} output pipes did not close after process exit"
            )))
        }
    }
}

/// Own one complete CLI transaction: stdin write + EOF + process wait all share the same hard
/// deadline. Stdout/stderr are drained concurrently so neither pipe can back-pressure the child.
/// Every error path explicitly kill/reaps before returning; `kill_on_drop` remains the cancellation
/// belt when the caller itself drops this future.
pub(crate) async fn run_external_cli_child(
    child: Child,
    stdin_content: &[u8],
    provider_label: &str,
) -> crate::error::Result<std::process::Output> {
    run_external_cli_child_with_timeout(child, stdin_content, provider_label, CLAUDE_TIMEOUT).await
}

/// The same process-group-safe transaction as [`run_external_cli_child`], with a caller-selected
/// wall-clock budget for short local probes. The teardown and bounded-output guarantees are
/// identical; callers cannot accidentally turn a readiness probe into a generation-length wait.
pub(crate) async fn run_external_cli_child_with_timeout(
    mut child: Child,
    stdin_content: &[u8],
    provider_label: &str,
    timeout: Duration,
) -> crate::error::Result<std::process::Output> {
    let mut process_group = ClaudeProcessGroup::register(&child);
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let teardown =
                kill_and_reap_external_cli(&mut child, &mut process_group, provider_label).await;
            return match teardown {
                Ok(()) => Err(AppError::Summarize(format!(
                    "failed to open {provider_label} stdout"
                ))),
                Err(error) => Err(AppError::Summarize(format!(
                    "failed to open {provider_label} stdout; teardown failed: {error}"
                ))),
            };
        }
    };
    let stderr = match child.stderr.take() {
        Some(stderr) => stderr,
        None => {
            let teardown =
                kill_and_reap_external_cli(&mut child, &mut process_group, provider_label).await;
            return match teardown {
                Ok(()) => Err(AppError::Summarize(format!(
                    "failed to open {provider_label} stderr"
                ))),
                Err(error) => Err(AppError::Summarize(format!(
                    "failed to open {provider_label} stderr; teardown failed: {error}"
                ))),
            };
        }
    };
    let stdout_task = spawn_pipe_reader(stdout, CLAUDE_STDOUT_LIMIT_BYTES);
    let stderr_task = spawn_pipe_reader(stderr, CLAUDE_STDERR_LIMIT_BYTES);

    let execution = tokio::time::timeout(timeout, async {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| AppError::Summarize(format!("failed to open {provider_label} stdin")))?;
        stdin.write_all(stdin_content).await.map_err(|error| {
            AppError::Summarize(format!("failed writing to {provider_label} stdin: {error}"))
        })?;
        stdin.shutdown().await.map_err(|error| {
            AppError::Summarize(format!("failed closing {provider_label} stdin: {error}"))
        })?;
        drop(stdin);
        child.wait().await.map_err(|error| {
            AppError::Summarize(format!("failed waiting on {provider_label}: {error}"))
        })
    })
    .await;

    let status = match execution {
        Ok(Ok(status)) => status,
        Ok(Err(primary)) => {
            let teardown =
                kill_and_reap_external_cli(&mut child, &mut process_group, provider_label).await;
            stdout_task.abort();
            stderr_task.abort();
            return match teardown {
                Ok(()) => Err(primary),
                Err(error) => Err(AppError::Summarize(format!(
                    "{primary}; teardown failed: {error}"
                ))),
            };
        }
        Err(_) => {
            let teardown =
                kill_and_reap_external_cli(&mut child, &mut process_group, provider_label).await;
            stdout_task.abort();
            stderr_task.abort();
            return match teardown {
                Ok(()) => Err(AppError::Summarize(format!(
                    "{provider_label} timed out after {}s",
                    timeout.as_secs()
                ))),
                Err(error) => Err(AppError::Summarize(format!(
                    "{provider_label} timed out after {}s; teardown failed: {error}",
                    timeout.as_secs()
                ))),
            };
        }
    };

    // The direct CLI exited, but Node/tooling descendants may still own network connections or
    // inherited stdout/stderr. Kill the isolated group and prove it empty before releasing output
    // or the caller's external-egress lease.
    kill_and_prove_group_dead(
        process_group.pgid(),
        tokio::time::Instant::now() + CLAUDE_REAP_TIMEOUT,
    )
    .await?;
    process_group.mark_proven_dead();

    let (stdout, stderr) =
        collect_external_cli_output(stdout_task, stderr_task, provider_label).await?;
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

/// Minimal, non-secret environment a child `claude` (and the `node` it spawns) needs. We start
/// from an EMPTY environment (`env_clear`) and re-add ONLY these, so `MURMUR_DEV_*`, API keys,
/// tokens, and anything else in the app's environment can NEVER be inherited by the child (F2).
const PASSTHROUGH_ENV: &[&str] = &[
    "HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "TMPDIR", "SHELL",
];

/// Vars that MUST NEVER reach the `claude` child even when the user opted into env inheritance: the
/// DB encryption keys decrypt the ENTIRE library and the account master key (`MURMUR_DEV_ACCOUNT_MK`)
/// unwraps every retained share key — and the child talks to the cloud, so all three key-material dev
/// hatches are stripped unconditionally. Everything else is the user's call (the inherit opt-in).
///
/// F2 catch-all: in ADDITION to these three explicit names, [`harden_env`] strips ANY var whose name
/// begins with [`MURMUR_ENV_PREFIX`], so a FUTURE `MURMUR_*` secret env var can never leak to the
/// child under inherit mode even if someone forgets to add it here. Keep this list for the explicit,
/// documented-intent cases; the prefix sweep is the safety net.
const NEVER_INHERIT_ENV: &[&str] = &["MURMUR_DEV_DEK", "MURMUR_DEV_KEK", "MURMUR_DEV_ACCOUNT_MK"];

/// Prefix for the F2 catch-all env sweep: under inherit mode EVERY var whose name starts with this is
/// removed from the child, so a new Murmur-owned secret env var never reaches the cloud-bound `claude`
/// child without anyone touching this file.
const MURMUR_ENV_PREFIX: &str = "MURMUR_";

/// Apply the environment policy to a tokio `Command`.
///
/// - `inherit = false` (DEFAULT, the F2 hardening): start from an EMPTY env (`env_clear`), set `PATH`
///   to the resolved login-shell PATH, and re-add ONLY the minimal non-secret [`PASSTHROUGH_ENV`], so
///   `MURMUR_DEV_*`, API keys, tokens, and anything else can NEVER be inherited by the child.
/// - `inherit = true` (OPT-IN, config `claude_code_inherit_env`): the child INHERITS our environment
///   (so an env `ANTHROPIC_API_KEY` / `ANTHROPIC_BASE_URL` / proxy var works like older versions),
///   EXCEPT [`NEVER_INHERIT_ENV`] which is always removed. `PATH` is still pinned to the login shell's
///   so a GUI-launched app can find `claude` + the `node` it spawns.
pub(crate) fn harden_env(cmd: &mut Command, inherit: bool) {
    if inherit {
        cmd.env("PATH", shell_path());
        for key in NEVER_INHERIT_ENV {
            cmd.env_remove(key);
        }
        // F2 catch-all: sweep EVERY inherited `MURMUR_*` var out of the child so a future
        // Murmur-owned secret env var never reaches the cloud-bound `claude` without being added to
        // NEVER_INHERIT_ENV above. `env_remove` overrides the inherited value with an explicit unset.
        for (key, _) in std::env::vars_os() {
            if key
                .to_str()
                .is_some_and(|k| k.starts_with(MURMUR_ENV_PREFIX))
            {
                cmd.env_remove(&key);
            }
        }
        return;
    }
    cmd.env_clear();
    cmd.env("PATH", shell_path());
    for key in PASSTHROUGH_ENV {
        if let Ok(val) = std::env::var(key) {
            cmd.env(key, val);
        }
    }
}

/// Default binary name (resolved via PATH) used to invoke the Claude Code CLI.
const DEFAULT_BINARY: &str = "claude";

/// Tool-isolation for the headless `claude -p` run. These runs ONLY read their prompt + stdin and
/// emit text (a note, or — for the Ask agentic loop — a JSON step that Murmur's OWN
/// `GatedToolExecutor` parses and dispatches), so they legitimately need ZERO claude tools.
///
/// We pass an EMPTY `--allowedTools ""` (an ALLOWLIST, not the former denylist). This is fail-CLOSED:
/// unlike a denylist of the tools that happened to exist when it was written, an empty allowlist also
/// blocks any FUTURE built-in claude tool. Empirically verified against the installed CLI — an empty
/// allowlist + `--strict-mcp-config` still runs a normal `-p`/`--system-prompt`/stdin request and
/// emits clean text (exit 0).
///
/// This alone does NOT stop MCP tools (`mcp__*`): the claude CLI discovers MCP servers from FILES
/// (`~/.claude.json`, project `.mcp.json`) — untouched by `env_clear`. [`STRICT_MCP_CONFIG_FLAG`] is
/// the load-bearing MCP closure: `--strict-mcp-config` with NO `--mcp-config` loads ZERO MCP servers,
/// so the "hermetic, nothing-leaves" run can NEVER invoke the user's ambient Gmail/Drive/Slack/… (or
/// a self-referential murmur) server. Both flags are applied at both spawn sites.
const ALLOWED_TOOLS: &str = "";

/// The flag that restricts MCP discovery to `--mcp-config` files only; passed WITHOUT any
/// `--mcp-config`, it loads zero MCP servers (the F1 MCP side-channel closure). Verified present in
/// the installed CLI (`claude --help`: "Only use MCP servers from --mcp-config, ignoring all other
/// MCP configurations").
const STRICT_MCP_CONFIG_FLAG: &str = "--strict-mcp-config";

/// The user's real shell PATH + common install dirs. A macOS GUI app (launched from
/// Finder / `open`) inherits only a minimal PATH (`/usr/bin:/bin:…`), so `claude` (and the
/// `node` it spawns) won't be found. Recover the real PATH from the login shell and
/// augment it with common locations. Cached — the shell probe runs at most once.
pub(crate) fn shell_path() -> &'static str {
    static CACHE: OnceLock<String> = OnceLock::new();
    CACHE.get_or_init(|| {
        let mut parts: Vec<String> = Vec::new();
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        if let Ok(out) = std::process::Command::new(&shell)
            .args(["-lic", "printf '%s' \"$PATH\""])
            .output()
        {
            if let Some(p) = String::from_utf8_lossy(&out.stdout)
                .lines()
                .rev()
                .find(|l| l.contains('/'))
            {
                parts.push(p.trim().to_string());
            }
        }
        if let Some(home) = dirs::home_dir() {
            for d in [
                ".local/bin",
                ".bun/bin",
                ".deno/bin",
                ".volta/bin",
                ".npm-global/bin",
            ] {
                parts.push(home.join(d).to_string_lossy().into_owned());
            }
        }
        parts.push("/opt/homebrew/bin".to_string());
        parts.push("/usr/local/bin".to_string());
        if let Ok(p) = std::env::var("PATH") {
            parts.push(p);
        }
        parts.join(":")
    })
}

/// Resolve `binary` to a VETTED ABSOLUTE path we are willing to execute (F3).
///
/// - A configured value containing `/` is taken as an explicit path and validated directly.
/// - A bare name is located by walking [`shell_path`] (never a raw `PATH` lookup by the OS) and
///   each candidate is validated before acceptance.
///
/// Validation ([`vet_binary`]) requires a regular file owned by the current user and not
/// world-writable, so a binary an attacker could swap (world-writable, or planted under a dir
/// they own) is rejected. On success returns the absolute, validated path; otherwise the error
/// explains why — and is intentionally PII-free.
fn resolve_binary(binary: &str, provider_label: &str) -> crate::error::Result<String> {
    if binary.contains('/') {
        let p = Path::new(binary);
        vet_binary(p, provider_label)?;
        return Ok(p.to_string_lossy().into_owned());
    }
    for dir in shell_path().split(':').filter(|d| !d.is_empty()) {
        let candidate = Path::new(dir).join(binary);
        if candidate.is_file() && vet_binary(&candidate, provider_label).is_ok() {
            return Ok(candidate.to_string_lossy().into_owned());
        }
    }
    Err(AppError::Unavailable(format!(
        "`{binary}` not found on a trusted PATH (or failed integrity checks)"
    )))
}

/// Reject a binary path unless it is a regular file, owned by the current uid, and not
/// world-writable (F3). A symlink is resolved first so the *target's* metadata is what we check.
pub(crate) fn vet_binary(path: &Path, provider_label: &str) -> crate::error::Result<()> {
    let canonical = std::fs::canonicalize(path).map_err(|e| {
        AppError::Unavailable(format!(
            "{provider_label} binary path is not resolvable: {e}"
        ))
    })?;
    let meta = std::fs::metadata(&canonical)
        .map_err(|e| AppError::Unavailable(format!("cannot stat {provider_label} binary: {e}")))?;
    if !meta.is_file() {
        return Err(AppError::Unavailable(format!(
            "configured {provider_label} binary is not a regular file"
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        // Must be owned by us — a binary owned by another (unprivileged) user could be swapped.
        // SAFETY: getuid() is always-succeeds FFI.
        let our_uid = unsafe { libc_getuid() };
        if meta.uid() != our_uid {
            return Err(AppError::Unavailable(format!(
                "{provider_label} binary is not owned by the current user"
            )));
        }
        // World-writable (o+w) means anyone could replace its contents.
        if meta.permissions().mode() & 0o002 != 0 {
            return Err(AppError::Unavailable(format!(
                "{provider_label} binary is world-writable — refusing to execute"
            )));
        }
    }
    Ok(())
}

// `getuid(2)` without pulling in the `libc` crate (not an approved dep). Declared locally; the
// symbol is in libSystem, always linked on macOS.
#[cfg(unix)]
extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

/// Probe one external cloud CLI under the same admission, process-group, timeout, environment and
/// teardown guarantees as real generation. Callers receive a non-failing [`Availability`] result.
fn build_external_cli_probe_command(
    bin: &str,
    args: &[&str],
    inherit_env: bool,
    current_dir: Option<&Path>,
) -> Command {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    isolate_process_group(&mut cmd);
    harden_env(&mut cmd, inherit_env);
    if let Some(current_dir) = current_dir {
        cmd.current_dir(current_dir);
    }
    cmd
}

async fn probe_external_cli(
    binary: &str,
    args: &[&str],
    inherit_env: bool,
    current_dir: Option<&Path>,
    provider_label: &str,
) -> Availability {
    let _process_lease = match crate::perf::acquire_external_egress_lease(None) {
        Ok(lease) => lease,
        Err(error) => {
            return Availability::Unavailable {
                reason: error.to_string(),
            }
        }
    };
    let bin = match resolve_binary(binary, provider_label) {
        Ok(bin) => bin,
        Err(error) => {
            return Availability::Unavailable {
                reason: error.to_string(),
            }
        }
    };
    let invocation = format!("`{} {}`", bin, args.join(" "));
    let mut cmd = build_external_cli_probe_command(&bin, args, inherit_env, current_dir);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            return Availability::Unavailable {
                reason: format!("failed to start {provider_label} probe {invocation} ({error})"),
            }
        }
    };
    let mut process_group = ClaudeProcessGroup::register(&child);
    let status = match tokio::time::timeout(CLAUDE_AVAILABILITY_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) => {
            match kill_and_prove_group_dead(
                process_group.pgid(),
                tokio::time::Instant::now() + CLAUDE_REAP_TIMEOUT,
            )
            .await
            {
                Ok(()) => {
                    process_group.mark_proven_dead();
                    Ok(status)
                }
                Err(error) => Err(format!(
                    "{provider_label} probe {invocation} teardown failed ({error})"
                )),
            }
        }
        Ok(Err(error)) => {
            match kill_and_reap_external_cli(
                    &mut child,
                    &mut process_group,
                    provider_label,
                )
                .await
                {
                    Ok(()) => Err(format!(
                        "{provider_label} probe {invocation} wait failed ({error})"
                    )),
                    Err(teardown) => Err(format!(
                        "{provider_label} probe {invocation} wait failed ({error}); teardown failed ({teardown})"
                    )),
                }
        }
        Err(_) => {
            match kill_and_reap_external_cli(&mut child, &mut process_group, provider_label).await {
                Ok(()) => Err(format!(
                    "{provider_label} probe {invocation} timed out after {}s",
                    CLAUDE_AVAILABILITY_TIMEOUT.as_secs()
                )),
                Err(error) => Err(format!(
                    "{provider_label} probe {invocation} timed out and teardown failed ({error})"
                )),
            }
        }
    };
    match status {
        Ok(status) if status.success() => Availability::Available,
        Ok(status) => Availability::Unavailable {
            reason: format!(
                "{provider_label} probe {invocation} exited with status {}",
                status.code().unwrap_or(-1)
            ),
        },
        Err(reason) => Availability::Unavailable { reason },
    }
}

/// Spawns the local `claude -p` CLI in headless print mode to generate the note.
///
/// Per the design lessons the run is kept hermetic via `--system-prompt` (the note-format template),
/// an EMPTY `--allowedTools ""` (fail-closed tool isolation — no built-in tool, now or in future),
/// and `--strict-mcp-config` (no `--mcp-config` ⇒ ZERO ambient MCP servers, so the run cannot reach
/// the user's Gmail/Drive/Slack/… servers or the local murmur MCP). The produced Markdown is
/// validated to start with a YAML front-matter fence line of three dashes.
pub struct ClaudeCodeProvider {
    binary: String,
    system_prompt: String,
    /// Optional model OVERRIDE passed to the CLI as `--model <id>`. Empty `""` (the default) means
    /// "let the `claude` CLI pick its own default model" — no `--model` flag is added in that case.
    /// (The CLI has no reasoning-effort flag, so `provider_effort` is intentionally NOT wired here.)
    model: String,
    /// Opt-in (config `claude_code_inherit_env`): inherit the shell environment into the `claude`
    /// child (restores older-version behavior where an env `ANTHROPIC_API_KEY` reached the CLI).
    /// Default false = the hardened env-cleared run. Even when true the DB encryption keys
    /// (`MURMUR_DEV_DEK`/`MURMUR_DEV_KEK`) are ALWAYS stripped — see [`harden_env`].
    inherit_env: bool,
}

impl ClaudeCodeProvider {
    pub fn new() -> Self {
        Self {
            binary: DEFAULT_BINARY.to_string(),
            system_prompt: template::default_template(),
            model: String::new(),
            inherit_env: false,
        }
    }

    pub fn with_binary(path: String) -> Self {
        let binary = if path.trim().is_empty() {
            DEFAULT_BINARY.to_string()
        } else {
            path
        };
        Self {
            binary,
            system_prompt: template::default_template(),
            model: String::new(),
            inherit_env: false,
        }
    }

    /// Set the model override (builder-style). Empty/blank ⇒ no `--model` flag (CLI default).
    pub fn with_model(mut self, model: String) -> Self {
        self.model = model;
        self
    }

    /// Opt-in to inheriting the shell environment into the `claude` child (builder-style). When the
    /// user's config has `claude_code_inherit_env = true`, an env `ANTHROPIC_API_KEY` (and proxy /
    /// base-url vars) reach the CLI again — the DB keys are still always stripped (see [`harden_env`]).
    pub fn with_inherit_env(mut self, inherit: bool) -> Self {
        self.inherit_env = inherit;
        self
    }
}

impl Default for ClaudeCodeProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl SummarizerProvider for ClaudeCodeProvider {
    fn id(&self) -> &str {
        "claude_code"
    }

    /// Probe whether the `claude` binary is reachable by running `claude --version`.
    /// Non-failing: any spawn/exec/validation error is reported as `Unavailable`.
    async fn availability(&self) -> Availability {
        probe_external_cli(
            &self.binary,
            &["--version"],
            self.inherit_env,
            None,
            "Claude Code",
        )
        .await
    }

    /// Spawn `claude -p --system-prompt <tpl> --disallowedTools <all>`, feed the meeting
    /// content on stdin, and validate the output begins with a `---` front-matter line.
    async fn summarize(&self, req: &SummarizeRequest) -> crate::error::Result<String> {
        // The system prompt carries the canonical note-format instructions; the request's
        // template overrides it if a caller supplied a custom one.
        let system_prompt = if req.template.trim().is_empty() {
            self.system_prompt.clone()
        } else {
            req.template.clone()
        };

        // stdin = metadata + vault link targets + transcript (the "user" content).
        let stdin_content = template::render_user_content(req);

        let bin = resolve_binary(&self.binary, "Claude Code")?;
        let mut cmd = build_claude_command(&bin, &system_prompt, &self.model, self.inherit_env);
        let child = cmd
            .spawn()
            .map_err(|e| AppError::Summarize(format!("failed to spawn `{bin}`: {e}")))?;
        let output = run_external_cli_child(child, stdin_content.as_bytes(), "Claude Code").await?;

        if !output.status.success() {
            // F6: never echo claude's stderr at a PII-retaining level (it can carry prompt/transcript
            // echoes). Log only code + length; surface an ACTIONABLE, PII-free error; and ONLY when
            // the user opted in (MURMUR_DEBUG_CLAUDE_STDERR=1) capture the real stderr to a debug file.
            let code = output.status.code().unwrap_or(-1);
            tracing::debug!(
                target: "summarize",
                code,
                stderr_len = output.stderr.len(),
                "claude exited non-zero"
            );
            let debug_path = capture_claude_stderr(&output.stderr);
            return Err(AppError::Summarize(claude_failure_message(
                code,
                &self.model,
                debug_path.as_deref(),
            )));
        }

        let stdout = String::from_utf8(output.stdout)
            .map_err(|e| AppError::Summarize(format!("claude produced non-UTF8 output: {e}")))?;
        let note = stdout.trim_start_matches('\u{feff}').trim_start();

        // Design-lesson invariant: the note must begin with a YAML front-matter fence.
        if !starts_with_frontmatter(note) {
            return Err(AppError::Summarize(
                "claude output did not start with a `---` YAML front-matter line".into(),
            ));
        }

        Ok(note.to_string())
    }

    async fn complete(&self, system: &str, user: &str) -> crate::error::Result<String> {
        let bin = resolve_binary(&self.binary, "Claude Code")?;
        // Same hermetic seam as `summarize`: empty allowlist + `--strict-mcp-config`. The Ask agentic
        // loop uses Murmur's OWN JSON tool protocol (the model emits `{"tool":…}` text that
        // `GatedToolExecutor` parses + executes) — it needs ZERO claude native tools or MCP, so this
        // isolation FIXES the Ask self-loop (no more `mcp__murmur*`) rather than breaking Ask.
        let mut cmd = build_claude_command(&bin, system, &self.model, self.inherit_env);
        let child = cmd
            .spawn()
            .map_err(|e| AppError::Summarize(format!("failed to spawn `{bin}`: {e}")))?;
        let output = run_external_cli_child(child, user.as_bytes(), "Claude Code").await?;
        if !output.status.success() {
            // F6: suppress stderr content (may carry prompt/transcript text); code only. Same
            // actionable error + opt-in capture as `summarize`.
            let code = output.status.code().unwrap_or(-1);
            tracing::debug!(
                target: "summarize",
                code,
                stderr_len = output.stderr.len(),
                "claude (complete) exited non-zero"
            );
            let debug_path = capture_claude_stderr(&output.stderr);
            return Err(AppError::Summarize(claude_failure_message(
                code,
                &self.model,
                debug_path.as_deref(),
            )));
        }
        String::from_utf8(output.stdout)
            .map(|s| s.trim().to_string())
            .map_err(|e| AppError::Summarize(format!("claude produced non-UTF8 output: {e}")))
    }
}

/// Build the fully-configured `claude -p` [`Command`] shared by BOTH spawn sites (`summarize` and
/// `complete`) — the single testable SEAM for the hermeticity flags (F1). Sets, in order:
/// `-p`, `--system-prompt <system>`, `--allowedTools ""` (fail-closed tool isolation),
/// `--strict-mcp-config` (zero ambient MCP servers), the optional `--model <id>`, piped stdio,
/// `kill_on_drop` (F6), and the hardened env (F2). [`run_external_cli_child`] owns stdin + wait + teardown
/// under one deadline.
///
/// Keeping this in ONE place is what lets a unit test assert (via `get_args()`) that every real spawn
/// carries `--allowedTools ""` + `--strict-mcp-config` — a drift-proof guard against the egress hole
/// re-opening if a future edit adds a third spawn site.
fn build_claude_command(bin: &str, system_prompt: &str, model: &str, inherit_env: bool) -> Command {
    let mut cmd = Command::new(bin);
    cmd.arg("-p")
        .arg("--system-prompt")
        .arg(system_prompt)
        // F1 tool isolation: empty allowlist = no built-in tool (fail-closed against future tools).
        .arg("--allowedTools")
        .arg(ALLOWED_TOOLS)
        // F1 MCP closure: `--strict-mcp-config` with NO `--mcp-config` ⇒ zero ambient MCP servers, so
        // the CLI cannot discover the user's Gmail/Drive/Slack/… (or the local murmur) MCP from
        // `~/.claude.json` / `.mcp.json`. Closes the un-redacted, un-consented egress + Ask self-loop.
        .arg(STRICT_MCP_CONFIG_FLAG)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true); // F6: a dropped future (cancel/panic/timeout) reaps the child.
                             // Brain/AI model override: add `--model` only when the user picked one.
    cmd.args(model_args(model));
    isolate_process_group(&mut cmd);
    harden_env(&mut cmd, inherit_env); // F2: env_clear + minimal PATH (or inherit-minus-secrets).
    cmd
}

/// The `--model <id>` argument pair to append for a given model override, or `&[]` when the
/// override is empty/blank (let the CLI use its default). Pure + unit-testable — the exact
/// branch used at both call sites in `summarize`/`complete` (`if !self.model.trim().is_empty()`).
///
/// `pub(crate)` so the A6 ledger-versus-wire test can ask this arm what it would ACTUALLY send,
/// instead of re-implementing the rule in the test and proving only that two copies agree.
pub(crate) fn model_args(model: &str) -> Vec<String> {
    // Since the Settings catalog became a hint rather than an allowlist, this string can be
    // anything the user typed. `--model` takes the NEXT argv entry, so an id like
    // `--sandbox danger-full-access` would be parsed by the CLI as a flag. Reject anything that is
    // not a plain slug and fall back to the CLI's own default.
    if !crate::summarize::provider::valid_model_id(model) {
        return Vec::new();
    }
    vec!["--model".to_string(), model.trim().to_string()]
}

/// True iff `text`'s first non-empty line is exactly a three-dash YAML fence (allowing
/// trailing whitespace on the line).
fn starts_with_frontmatter(text: &str) -> bool {
    match text.lines().next() {
        Some(first) => first.trim_end() == "---",
        None => false,
    }
}

/// Build the PII-free, ACTIONABLE error for a non-zero `claude` exit. The model id (the user's own
/// pick — NOT transcript content) is named when a `--model` override was in play, because the #1
/// real cause is the CLI / a LiteLLM proxy not knowing that id (the model-picker regression: 0.1.0
/// passed no `--model` and worked), and the fix is one click. `debug_path` is `Some` when
/// `MURMUR_DEBUG_CLAUDE_STDERR` captured the real stderr — then we name the file instead of telling
/// the user to set the flag. Never includes stderr content (may carry transcript echoes).
fn claude_failure_message(code: i32, model: &str, debug_path: Option<&str>) -> String {
    let model = model.trim();
    let mut msg = format!("claude exited with status {code}");
    if model.is_empty() {
        msg.push_str(
            " — `claude` could not complete the request. Check that `claude -p \"hi\"` works in a \
             terminal and that the CLI is signed in / its auth is configured",
        );
    } else {
        msg.push_str(&format!(
            " — the selected model `{model}` may not be available in your `claude` setup \
             (a proxy / LiteLLM endpoint may not know that id). Fix: Settings → Brain/AI → \
             Model → \"Default (provider's pick)\", or pick a model your setup supports",
        ));
    }
    match debug_path {
        Some(p) => msg.push_str(&format!(". Real stderr was captured to {p}")),
        None => msg.push_str(
            ". stderr suppressed (may contain transcript content) — set MURMUR_DEBUG_CLAUDE_STDERR=1 \
             to capture it to a debug file",
        ),
    }
    msg
}

/// When `MURMUR_DEBUG_CLAUDE_STDERR=1`, write the failed child's stderr to
/// `<app-data>/<app_dir>/debug/claude-stderr.log` and return its path, so a stuck user can read the
/// REAL diagnostic (the model 404 / auth message) that we otherwise suppress. OFF by default; the
/// file is local + user-controlled, and we capture ONLY stderr (the diagnostic stream) — never
/// stdout (the note content). Best-effort: any IO/env miss yields `None` and never breaks summarize.
fn capture_claude_stderr(stderr: &[u8]) -> Option<String> {
    if std::env::var("MURMUR_DEBUG_CLAUDE_STDERR").ok().as_deref() != Some("1") {
        return None;
    }
    let dir = dirs::data_dir()?
        .join(crate::state::app_dir_name())
        .join("debug");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join("claude-stderr.log");
    std::fs::write(&path, stderr).ok()?;
    Some(path.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_args_adds_flag_only_when_set() {
        // Empty/blank ⇒ no flag (CLI default).
        assert!(model_args("").is_empty());
        assert!(model_args("   ").is_empty());
        // A real id ⇒ the `--model <id>` pair.
        assert_eq!(
            model_args("claude-opus-4-8"),
            vec!["--model".to_string(), "claude-opus-4-8".to_string()]
        );
    }

    /// A6, asserted on the ARGV the CLI would actually receive — not on the predicate.
    ///
    /// Testing `!valid_model_id(hostile)` alone proves the predicate rejects a string; it does not
    /// prove this call site consults it. `--model` consumes the NEXT argv entry, so the property
    /// that matters is that a hostile value never becomes an argument at all: no `--model`, and no
    /// trace of the value anywhere in the vector.
    #[test]
    fn a_hostile_model_id_never_reaches_argv() {
        for hostile in [
            "--sandbox danger-full-access",
            "-m",
            "--dangerously-skip-permissions",
            "claude-opus-5 --sandbox danger-full-access",
            "claude\nopus",
            "../../etc/passwd",
            "$(rm -rf ~)",
        ] {
            let argv = model_args(hostile);
            assert!(
                argv.is_empty(),
                "{hostile:?} must not produce any argument; got {argv:?}"
            );
            assert!(
                !argv.iter().any(|arg| arg.contains(hostile.trim())),
                "no fragment of {hostile:?} may survive into argv"
            );
        }
        // ...while a legitimate UNLISTED id — the whole point of a hint catalog — still gets sent.
        assert_eq!(
            model_args("claude-opus-6"),
            vec!["--model".to_string(), "claude-opus-6".to_string()],
            "a valid id absent from every bundled catalog must still reach the CLI"
        );
        // Surrounding whitespace is trimmed rather than rejected, and never splits into two args.
        assert_eq!(
            model_args("  llama3.1:8b  "),
            vec!["--model".to_string(), "llama3.1:8b".to_string()]
        );
    }

    /// The same property asserted on the CONSTRUCTED COMMAND, not only on the helper.
    ///
    /// `a_hostile_model_id_never_reaches_argv` pins `model_args`, which proves the helper is sound
    /// but not that the command builder routes through it — `build_claude_command` takes `model`
    /// as its own parameter and could keep a private `if !model.trim().is_empty()` branch, which is
    /// exactly the shape that used to be there. The codex arm already asserts on its built command;
    /// this closes the same gap here so neither arm rests on an untested call edge.
    #[test]
    fn a_hostile_model_id_never_reaches_the_built_claude_argv() {
        for hostile in [
            "--sandbox danger-full-access",
            "-m",
            "--dangerously-skip-permissions",
            "claude\nopus",
            "../../etc/passwd",
        ] {
            let args = args_of(&build_claude_command("claude", "SYSTEM", hostile, false));
            assert!(
                !args.iter().any(|arg| arg == "--model"),
                "{hostile:?} produced a --model flag in the built command: {args:?}"
            );
            // Compare whole argv ENTRIES, not substrings: the hermetic flags legitimately contain
            // `-m` (`--strict-mcp-config`), so a `contains` check reports a leak that is not one.
            // Each whitespace-separated token is checked so a builder that split the value into
            // several args would still be caught.
            for token in hostile.split_whitespace() {
                assert!(
                    !args.iter().any(|arg| arg == token),
                    "token {token:?} of {hostile:?} reached the built argv: {args:?}"
                );
            }
        }
        // ...and a legitimate unlisted id DOES reach the command, or the guard would be vacuous:
        // a builder that never passed `--model` at all would satisfy every assertion above.
        let args = args_of(&build_claude_command(
            "claude",
            "SYSTEM",
            "claude-opus-6",
            false,
        ));
        let flag = args.iter().position(|arg| arg == "--model");
        assert!(flag.is_some(), "a valid id must still reach argv: {args:?}");
        assert_eq!(args[flag.unwrap() + 1], "claude-opus-6");
    }

    #[test]
    fn with_model_threads_the_override() {
        // The builder stores the override; the empty default leaves it unset (no flag).
        let p = ClaudeCodeProvider::with_binary("claude".to_string());
        assert!(p.model.is_empty());
        let p = ClaudeCodeProvider::with_binary("claude".to_string())
            .with_model("claude-sonnet-4-6".to_string());
        assert_eq!(p.model, "claude-sonnet-4-6");
        assert_eq!(
            model_args(&p.model),
            vec!["--model".to_string(), "claude-sonnet-4-6".to_string()]
        );
    }

    #[test]
    fn with_inherit_env_threads_the_flag() {
        assert!(
            !ClaudeCodeProvider::with_binary("claude".into()).inherit_env,
            "default is hardened"
        );
        assert!(
            ClaudeCodeProvider::with_binary("claude".into())
                .with_inherit_env(true)
                .inherit_env,
            "opt-in flag is threaded"
        );
    }

    /// Collect a `Command`'s args as owned `String`s for assertions.
    fn args_of(cmd: &Command) -> Vec<String> {
        cmd.as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn build_claude_command_is_hermetic_summarize_shape() {
        // F1: the `summarize` command MUST isolate tools (empty allowlist) AND close the MCP side
        // channel (`--strict-mcp-config`) so the "hermetic, nothing-leaves" run cannot invoke the
        // user's ambient MCP servers. RED before those flags were added, GREEN after.
        let cmd = build_claude_command("claude", "SYSTEM PROMPT", "", false);
        let args = args_of(&cmd);
        assert!(
            args.contains(&"-p".to_string()),
            "headless print mode: {args:?}"
        );
        assert!(
            args.contains(&"--strict-mcp-config".to_string()),
            "F1: must pass --strict-mcp-config to load ZERO ambient MCP servers: {args:?}"
        );
        // Empty ALLOWLIST (fail-closed) — the flag with an empty value, NOT the old denylist.
        let allow_at = args
            .iter()
            .position(|a| a == "--allowedTools")
            .expect("F1: must pass --allowedTools (allowlist) — see args");
        assert_eq!(
            args.get(allow_at + 1),
            Some(&String::new()),
            "F1: --allowedTools value must be EMPTY (no tool allowed): {args:?}"
        );
        // The former denylist flag must be GONE (we switched to an allowlist).
        assert!(
            !args.contains(&"--disallowedTools".to_string()),
            "the denylist flag was replaced by the empty allowlist: {args:?}"
        );
        // The system prompt is still threaded through.
        assert!(
            args.contains(&"SYSTEM PROMPT".to_string()),
            "system prompt threaded: {args:?}"
        );
    }

    #[test]
    fn build_claude_command_is_hermetic_complete_shape() {
        // F1: the `complete` (Ask agentic loop) command MUST carry the SAME isolation. Ask uses
        // Murmur's own JSON tool protocol, so disabling claude's native tools + MCP FIXES the self-
        // loop rather than breaking Ask. Both spawn sites share `build_claude_command`, so asserting
        // it once with the complete-style inputs (no front-matter template) covers the second site.
        let cmd = build_claude_command("claude", "ask system", "claude-opus-4-8", false);
        let args = args_of(&cmd);
        assert!(
            args.contains(&"--strict-mcp-config".to_string()),
            "F1: complete must also pass --strict-mcp-config: {args:?}"
        );
        let allow_at = args
            .iter()
            .position(|a| a == "--allowedTools")
            .expect("F1: complete must pass --allowedTools: {args:?}");
        assert_eq!(
            args.get(allow_at + 1),
            Some(&String::new()),
            "F1: complete --allowedTools value must be EMPTY: {args:?}"
        );
        // The model override still rides through the shared seam.
        assert_eq!(
            args.windows(2)
                .find(|w| w[0] == "--model")
                .map(|w| w[1].clone()),
            Some("claude-opus-4-8".to_string()),
            "the --model override rides the shared seam: {args:?}"
        );
    }

    /// A timed-out child is torn down and leaves NO unproven process group.
    ///
    /// SCOPE, stated plainly because the name of this test used to overclaim: it is a smoke test
    /// for the teardown path, NOT a regression test for the budget split below it. It passes both
    /// with and without that change, and it was verified to do so rather than assumed.
    ///
    /// The reason is that the coupling cannot be driven from a test. Teardown does two different
    /// waits — reaping OUR direct child, then proving the isolated group empty, which depends on
    /// the OS reaping grandchildren our kill orphaned. Sharing one deadline means a slow child
    /// reap eats the group proof's budget. But the kill is SIGKILL, which cannot be trapped or
    /// delayed, so there is no way to make the first wait slow on demand and no way to reach the
    /// failing branch deterministically. The fix (a fresh budget for the second wait) rests on
    /// reading the code, not on a red test, and that is recorded here rather than papered over
    /// with a green one that proves something else.
    ///
    /// What this test DOES pin, and what would have caught a real regression in it: a wedged child
    /// with a grandchild is killed, reaped, and its group proven dead — so `has_unproven_process_group`
    /// stays false. That marker is sticky and recording admission refuses while any group carries
    /// it, so a teardown that stopped proving groups dead would silently cost the user the ability
    /// to record until they restarted the app.
    /// Teardown must conclude nothing from the errno of a group signal that reached nobody.
    ///
    /// RED before the fix this pins: EPERM made `signal_process_group` return an error, so
    /// `kill_and_reap_external_cli` returned early, `mark_proven_dead` never ran, and `Drop` left
    /// the group `Unproven` — a sticky marker that makes `perf::…` refuse recording admission
    /// until the app restarts. It reached CI as an intermittent failure of three external-CLI
    /// teardown tests, and reproduces at ~15% under CPU starvation, where a child is briefly
    /// invisible to `kill(pid, 0)` and `getpgid` while `waitpid(WNOHANG)` still calls it running.
    ///
    /// The control arm is the point: an unexplained errno must still fail closed, or this guard
    /// would wave through every teardown and measure nothing.
    #[cfg(unix)]
    #[test]
    fn a_group_signal_that_reached_nobody_is_not_a_teardown_failure() {
        assert!(
            group_signal_errno_is_benign(Some(3)),
            "ESRCH: the group is already absent"
        );
        assert!(
            group_signal_errno_is_benign(Some(1)),
            "EPERM: macOS names 'no eligible member in this group' this way; the liveness proof, \
             not this errno, decides whether the group is dead"
        );
        assert!(
            !group_signal_errno_is_benign(Some(22)),
            "EINVAL is a real failure to deliver and must stay fatal"
        );
        assert!(
            !group_signal_errno_is_benign(None),
            "a failure with no errno is unexplained and must stay fatal"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_timed_out_child_leaves_no_unproven_process_group() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let script = crate::storage::db::unique_temp_path("murmur-teardown-budget", "sh");
        let mut file = std::fs::File::create(&script).unwrap();
        // A grandchild, so the group holds more than the direct child: that is the shape whose
        // reaping the app does not control, and the reason the second wait exists at all.
        writeln!(file, "#!/bin/sh").unwrap();
        writeln!(file, "/bin/sleep 30 &").unwrap();
        writeln!(file, "/bin/sleep 30").unwrap();
        drop(file);
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();

        let mut command = Command::new(&script);
        command.stdin(std::process::Stdio::piped());
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());
        isolate_process_group(&mut command);
        let child = command.spawn().unwrap();

        let result = run_external_cli_child_with_timeout(
            child,
            b"",
            "teardown budget probe",
            Duration::from_millis(200),
        )
        .await;

        // The probe itself must fail as a timeout — otherwise the fixture never reached the
        // teardown path and this oracle proves nothing at all.
        let Err(AppError::Summarize(reason)) = result else {
            panic!("a wedged child must time out");
        };
        assert!(
            reason.contains("timed out after"),
            "the fixture must reach the timeout path: {reason}"
        );
        assert!(
            !reason.contains("teardown failed"),
            "teardown must complete inside its budget: {reason}"
        );
        assert!(
            !has_unproven_process_group(),
            "a timed-out child left an unproven process group"
        );

        let _ = std::fs::remove_file(script);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn readiness_failure_keeps_the_binary_and_probe_argv_actionable() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let script =
            crate::storage::db::unique_temp_path("murmur-claude-readiness-diagnostic", "sh");
        let mut file = std::fs::File::create(&script).unwrap();
        writeln!(file, "#!/bin/sh").unwrap();
        writeln!(file, "exit 7").unwrap();
        drop(file);
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();

        let path = script.to_string_lossy().into_owned();
        let availability =
            probe_external_cli(&path, &["--version"], false, None, "Claude Code").await;
        let Availability::Unavailable { reason } = availability else {
            panic!("non-zero readiness probe must be unavailable");
        };
        assert!(reason.contains(&path), "binary path must remain actionable");
        assert!(
            reason.contains("--version"),
            "probe argv must remain actionable"
        );
        assert!(
            reason.contains("status 7"),
            "exit status must remain visible"
        );
        let _ = std::fs::remove_file(script);
    }

    #[test]
    fn inherit_env_strips_all_murmur_prefixed_vars() {
        // F2 catch-all: under inherit mode, a NEW `MURMUR_*` secret env var (not in NEVER_INHERIT_ENV)
        // must STILL be stripped from the child by the prefix sweep, so a future secret never leaks to
        // the cloud-bound `claude` without anyone touching this file.
        let var = "MURMUR_SOMETHING_NEW";
        // SAFETY: single-threaded test process; we set + read + remove this one var synchronously.
        unsafe { std::env::set_var(var, "leak-me") };
        let mut cmd = Command::new("true");
        harden_env(&mut cmd, true);
        let envs: std::collections::HashMap<&std::ffi::OsStr, Option<&std::ffi::OsStr>> =
            cmd.as_std().get_envs().collect();
        let stripped = envs.get(std::ffi::OsStr::new(var));
        unsafe { std::env::remove_var(var) };
        assert_eq!(
            stripped,
            Some(&None),
            "a new MURMUR_* var must be stripped in inherit mode by the prefix sweep"
        );
    }

    #[test]
    fn inherit_env_always_strips_db_encryption_keys() {
        // OPT-IN inherit: the child inherits our env — EXCEPT the DB encryption keys, which are
        // explicitly REMOVED (mapped to None via env_remove) so they can NEVER reach a cloud-bound
        // subprocess. This is the load-bearing guard that keeps the opt-in safe.
        let mut cmd = Command::new("true");
        harden_env(&mut cmd, true);
        let envs: std::collections::HashMap<&std::ffi::OsStr, Option<&std::ffi::OsStr>> =
            cmd.as_std().get_envs().collect();
        assert_eq!(
            envs.get(std::ffi::OsStr::new("MURMUR_DEV_DEK")),
            Some(&None),
            "the DB DEK must be stripped even in inherit mode"
        );
        assert_eq!(
            envs.get(std::ffi::OsStr::new("MURMUR_DEV_KEK")),
            Some(&None),
            "the DB KEK must be stripped even in inherit mode"
        );
        assert_eq!(
            envs.get(std::ffi::OsStr::new("MURMUR_DEV_ACCOUNT_MK")),
            Some(&None),
            "the account MK dev hatch must be stripped even in inherit mode"
        );
        // PATH is still pinned so a GUI-launched app can find `claude` + its `node`.
        let path = envs.get(std::ffi::OsStr::new("PATH"));
        assert!(
            path.is_some() && path.unwrap().is_some(),
            "PATH is pinned in inherit mode"
        );
    }

    #[test]
    fn failure_message_names_the_model_and_offers_default() {
        // The #1 real cause: a `--model` override the user's claude CLI / proxy doesn't know
        // (regression from the model picker). The error MUST name the model + the one-click fix.
        let m = claude_failure_message(1, "claude-sonnet-4-6", None);
        assert!(
            m.contains("claude-sonnet-4-6"),
            "names the offending model: {m}"
        );
        assert!(
            m.contains("Default"),
            "offers the Model = Default workaround: {m}"
        );
        assert!(
            m.contains("MURMUR_DEBUG_CLAUDE_STDERR"),
            "tells the user how to capture the real stderr: {m}"
        );
    }

    #[test]
    fn failure_message_is_generic_without_a_model_override() {
        // No model picked ⇒ no model hint; point at the terminal auth check instead.
        let m = claude_failure_message(1, "   ", None);
        assert!(
            !m.contains("selected model"),
            "no model hint when none set: {m}"
        );
        assert!(m.contains("claude -p"), "points at the terminal check: {m}");
    }

    #[test]
    fn failure_message_points_to_the_debug_file_when_captured() {
        // When MURMUR_DEBUG_CLAUDE_STDERR captured stderr, name the file instead of telling the
        // user to set the flag (they already did).
        let m = claude_failure_message(2, "", Some("/x/claude-stderr.log"));
        assert!(
            m.contains("/x/claude-stderr.log"),
            "names the capture path: {m}"
        );
        assert!(
            !m.contains("MURMUR_DEBUG_CLAUDE_STDERR"),
            "no 'set the flag' hint once already captured: {m}"
        );
    }

    #[test]
    fn frontmatter_detection() {
        assert!(starts_with_frontmatter("---\ntitle: x\n---\n# X"));
        assert!(starts_with_frontmatter("---  \ntitle: x"));
        assert!(!starts_with_frontmatter("Here is your note:\n---"));
        assert!(!starts_with_frontmatter("----\ntitle: x"));
        assert!(!starts_with_frontmatter(""));
    }
}
