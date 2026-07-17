use std::path::Path;
use std::process::Stdio;
use std::sync::OnceLock;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::error::AppError;
use crate::summarize::provider::*;
use crate::summarize::template;

/// Hard wall-clock ceiling for a single `claude -p` run. A wedged CLI (network stall, hung
/// node child) is killed rather than blocking the pipeline forever (F6).
const CLAUDE_TIMEOUT: Duration = Duration::from_secs(180);

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
fn harden_env(cmd: &mut Command, inherit: bool) {
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
fn shell_path() -> &'static str {
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
fn resolve_binary(binary: &str) -> crate::error::Result<String> {
    if binary.contains('/') {
        let p = Path::new(binary);
        vet_binary(p)?;
        return Ok(p.to_string_lossy().into_owned());
    }
    for dir in shell_path().split(':').filter(|d| !d.is_empty()) {
        let candidate = Path::new(dir).join(binary);
        if candidate.is_file() && vet_binary(&candidate).is_ok() {
            return Ok(candidate.to_string_lossy().into_owned());
        }
    }
    Err(AppError::Unavailable(format!(
        "`{binary}` not found on a trusted PATH (or failed integrity checks)"
    )))
}

/// Reject a binary path unless it is a regular file, owned by the current uid, and not
/// world-writable (F3). A symlink is resolved first so the *target's* metadata is what we check.
fn vet_binary(path: &Path) -> crate::error::Result<()> {
    let canonical = std::fs::canonicalize(path)
        .map_err(|e| AppError::Unavailable(format!("claude binary path is not resolvable: {e}")))?;
    let meta = std::fs::metadata(&canonical)
        .map_err(|e| AppError::Unavailable(format!("cannot stat claude binary: {e}")))?;
    if !meta.is_file() {
        return Err(AppError::Unavailable(
            "configured claude binary is not a regular file".to_string(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        use std::os::unix::fs::PermissionsExt;
        // Must be owned by us — a binary owned by another (unprivileged) user could be swapped.
        // SAFETY: getuid() is always-succeeds FFI.
        let our_uid = unsafe { libc_getuid() };
        if meta.uid() != our_uid {
            return Err(AppError::Unavailable(
                "claude binary is not owned by the current user".to_string(),
            ));
        }
        // World-writable (o+w) means anyone could replace its contents.
        if meta.permissions().mode() & 0o002 != 0 {
            return Err(AppError::Unavailable(
                "claude binary is world-writable — refusing to execute".to_string(),
            ));
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
        let bin = match resolve_binary(&self.binary) {
            Ok(b) => b,
            Err(e) => {
                return Availability::Unavailable {
                    reason: e.to_string(),
                }
            }
        };
        let mut cmd = Command::new(&bin);
        cmd.arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        harden_env(&mut cmd, self.inherit_env); // F2: env_clear + minimal PATH so no secrets leak to the child.
        match cmd.status().await {
            Ok(status) if status.success() => Availability::Available,
            Ok(status) => Availability::Unavailable {
                reason: format!(
                    "`{bin} --version` exited with status {}",
                    status.code().unwrap_or(-1)
                ),
            },
            Err(e) => Availability::Unavailable {
                reason: format!("`{bin}` not found ({e})"),
            },
        }
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

        let bin = resolve_binary(&self.binary)?;
        let mut cmd = build_claude_command(&bin, &system_prompt, &self.model, self.inherit_env);
        let mut child = cmd
            .spawn()
            .map_err(|e| AppError::Summarize(format!("failed to spawn `{bin}`: {e}")))?;

        // Write the transcript to stdin, then drop the handle to signal EOF.
        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| AppError::Summarize("failed to open claude stdin".into()))?;
            stdin
                .write_all(stdin_content.as_bytes())
                .await
                .map_err(|e| AppError::Summarize(format!("failed writing to claude stdin: {e}")))?;
            stdin
                .shutdown()
                .await
                .map_err(|e| AppError::Summarize(format!("failed closing claude stdin: {e}")))?;
        }

        // F6: bound the run. On timeout, kill the child (kill_on_drop also covers this) and fail.
        let output = match tokio::time::timeout(CLAUDE_TIMEOUT, child.wait_with_output()).await {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => {
                return Err(AppError::Summarize(format!(
                    "failed waiting on claude: {e}"
                )))
            }
            Err(_elapsed) => {
                // The future is dropped here → kill_on_drop(true) reaps the process.
                return Err(AppError::Summarize(format!(
                    "claude timed out after {}s",
                    CLAUDE_TIMEOUT.as_secs()
                )));
            }
        };

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
        let bin = resolve_binary(&self.binary)?;
        // Same hermetic seam as `summarize`: empty allowlist + `--strict-mcp-config`. The Ask agentic
        // loop uses Murmur's OWN JSON tool protocol (the model emits `{"tool":…}` text that
        // `GatedToolExecutor` parses + executes) — it needs ZERO claude native tools or MCP, so this
        // isolation FIXES the Ask self-loop (no more `mcp__murmur*`) rather than breaking Ask.
        let mut cmd = build_claude_command(&bin, system, &self.model, self.inherit_env);
        let mut child = cmd
            .spawn()
            .map_err(|e| AppError::Summarize(format!("failed to spawn `{bin}`: {e}")))?;
        {
            let mut stdin = child
                .stdin
                .take()
                .ok_or_else(|| AppError::Summarize("failed to open claude stdin".into()))?;
            stdin
                .write_all(user.as_bytes())
                .await
                .map_err(|e| AppError::Summarize(format!("failed writing to claude stdin: {e}")))?;
            stdin
                .shutdown()
                .await
                .map_err(|e| AppError::Summarize(format!("failed closing claude stdin: {e}")))?;
        }
        // F6: bound the run; on timeout the dropped future + kill_on_drop reaps the child.
        let output = match tokio::time::timeout(CLAUDE_TIMEOUT, child.wait_with_output()).await {
            Ok(Ok(out)) => out,
            Ok(Err(e)) => {
                return Err(AppError::Summarize(format!(
                    "failed waiting on claude: {e}"
                )))
            }
            Err(_elapsed) => {
                return Err(AppError::Summarize(format!(
                    "claude timed out after {}s",
                    CLAUDE_TIMEOUT.as_secs()
                )))
            }
        };
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
/// `kill_on_drop` (F6), and the hardened env (F2). The caller only writes stdin + reaps the child.
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
    harden_env(&mut cmd, inherit_env); // F2: env_clear + minimal PATH (or inherit-minus-secrets).
    cmd
}

/// The `--model <id>` argument pair to append for a given model override, or `&[]` when the
/// override is empty/blank (let the CLI use its default). Pure + unit-testable — the exact
/// branch used at both call sites in `summarize`/`complete` (`if !self.model.trim().is_empty()`).
fn model_args(model: &str) -> Vec<String> {
    if model.trim().is_empty() {
        Vec::new()
    } else {
        vec!["--model".to_string(), model.to_string()]
    }
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
        assert!(args.contains(&"-p".to_string()), "headless print mode: {args:?}");
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
