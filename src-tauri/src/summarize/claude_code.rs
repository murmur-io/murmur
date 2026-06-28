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
const PASSTHROUGH_ENV: &[&str] = &["HOME", "USER", "LOGNAME", "LANG", "LC_ALL", "TMPDIR", "SHELL"];

/// Apply the F2 hardened environment to a tokio `Command`: clear everything, set `PATH` to the
/// resolved shell PATH, and re-add only the minimal non-secret vars that exist in our process.
fn harden_env(cmd: &mut Command) {
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

/// Tools we explicitly disallow so the headless `claude -p` run is hermetic — it must
/// only read its prompt + stdin and emit the note, never touch the filesystem, network,
/// or shell. Passed to `--disallowedTools` (space-separated list per the CLI).
const DISALLOWED_TOOLS: &[&str] = &[
    "Bash",
    "Edit",
    "Write",
    "Read",
    "Glob",
    "Grep",
    "WebFetch",
    "WebSearch",
    "Task",
    "NotebookEdit",
];

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
    let canonical = std::fs::canonicalize(path).map_err(|e| {
        AppError::Unavailable(format!("claude binary path is not resolvable: {e}"))
    })?;
    let meta = std::fs::metadata(&canonical).map_err(|e| {
        AppError::Unavailable(format!("cannot stat claude binary: {e}"))
    })?;
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
/// Per the design lessons the run is kept hermetic via `--system-prompt` (the note-format
/// template) and `--disallowedTools` (every tool, so it cannot read the vault or hit the
/// network), and the produced Markdown is validated to start with a YAML front-matter
/// fence line of three dashes.
pub struct ClaudeCodeProvider {
    binary: String,
    system_prompt: String,
    /// Optional model OVERRIDE passed to the CLI as `--model <id>`. Empty `""` (the default) means
    /// "let the `claude` CLI pick its own default model" — no `--model` flag is added in that case.
    /// (The CLI has no reasoning-effort flag, so `provider_effort` is intentionally NOT wired here.)
    model: String,
}

impl ClaudeCodeProvider {
    pub fn new() -> Self {
        Self {
            binary: DEFAULT_BINARY.to_string(),
            system_prompt: template::default_template(),
            model: String::new(),
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
        }
    }

    /// Set the model override (builder-style). Empty/blank ⇒ no `--model` flag (CLI default).
    pub fn with_model(mut self, model: String) -> Self {
        self.model = model;
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
            Err(e) => return Availability::Unavailable { reason: e.to_string() },
        };
        let mut cmd = Command::new(&bin);
        cmd.arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        harden_env(&mut cmd); // F2: env_clear + minimal PATH so no secrets leak to the child.
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
        let mut cmd = Command::new(&bin);
        cmd.arg("-p")
            .arg("--system-prompt")
            .arg(&system_prompt)
            .arg("--disallowedTools")
            .arg(DISALLOWED_TOOLS.join(" "))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true); // F6: a dropped future (cancel/panic) reaps the child.
        // Brain/AI model override: only add `--model` when the user picked a specific model;
        // an empty value lets the CLI use its own default.
        cmd.args(model_args(&self.model));
        harden_env(&mut cmd); // F2: env_clear + minimal PATH.
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
                return Err(AppError::Summarize(format!("failed waiting on claude: {e}")))
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
            // F6: do NOT echo claude's stderr at a PII-retaining level. stderr can contain prompt
            // fragments / transcript echoes; surface only the exit code, and log stderr length
            // (not content) at debug for diagnostics.
            tracing::debug!(
                target: "summarize",
                code = output.status.code().unwrap_or(-1),
                stderr_len = output.stderr.len(),
                "claude exited non-zero"
            );
            return Err(AppError::Summarize(format!(
                "claude exited with status {} (stderr suppressed: may contain transcript content)",
                output.status.code().unwrap_or(-1),
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
        let mut cmd = Command::new(&bin);
        cmd.arg("-p")
            .arg("--system-prompt")
            .arg(system)
            .arg("--disallowedTools")
            .arg(DISALLOWED_TOOLS.join(" "))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true); // F6
        // Brain/AI model override (mirrors `summarize`): add `--model` only when set.
        cmd.args(model_args(&self.model));
        harden_env(&mut cmd); // F2
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
                return Err(AppError::Summarize(format!("failed waiting on claude: {e}")))
            }
            Err(_elapsed) => {
                return Err(AppError::Summarize(format!(
                    "claude timed out after {}s",
                    CLAUDE_TIMEOUT.as_secs()
                )))
            }
        };
        if !output.status.success() {
            // F6: suppress stderr content (may carry prompt/transcript text); code only.
            tracing::debug!(
                target: "summarize",
                code = output.status.code().unwrap_or(-1),
                stderr_len = output.stderr.len(),
                "claude (complete) exited non-zero"
            );
            return Err(AppError::Summarize(format!(
                "claude exited with status {} (stderr suppressed: may contain prompt content)",
                output.status.code().unwrap_or(-1),
            )));
        }
        String::from_utf8(output.stdout)
            .map(|s| s.trim().to_string())
            .map_err(|e| AppError::Summarize(format!("claude produced non-UTF8 output: {e}")))
    }
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
    fn frontmatter_detection() {
        assert!(starts_with_frontmatter("---\ntitle: x\n---\n# X"));
        assert!(starts_with_frontmatter("---  \ntitle: x"));
        assert!(!starts_with_frontmatter("Here is your note:\n---"));
        assert!(!starts_with_frontmatter("----\ntitle: x"));
        assert!(!starts_with_frontmatter(""));
    }
}
