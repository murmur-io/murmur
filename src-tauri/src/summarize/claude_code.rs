use std::path::Path;
use std::process::Stdio;
use std::sync::OnceLock;

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

use crate::error::AppError;
use crate::summarize::provider::*;
use crate::summarize::template;

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

/// Resolve a bare binary name to an absolute path found in [`shell_path`]; pass through any
/// value containing `/` unchanged. Falls back to the bare name if not located.
fn resolve_binary(binary: &str) -> String {
    if binary.contains('/') {
        return binary.to_string();
    }
    for dir in shell_path().split(':').filter(|d| !d.is_empty()) {
        let candidate = Path::new(dir).join(binary);
        if candidate.is_file() {
            return candidate.to_string_lossy().into_owned();
        }
    }
    binary.to_string()
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
}

impl ClaudeCodeProvider {
    pub fn new() -> Self {
        Self {
            binary: DEFAULT_BINARY.to_string(),
            system_prompt: template::default_template(),
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
        }
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
    /// Non-failing: any spawn/exec error is reported as `Unavailable`.
    async fn availability(&self) -> Availability {
        let bin = resolve_binary(&self.binary);
        match Command::new(&bin)
            .arg("--version")
            .env("PATH", shell_path())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
        {
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

        let bin = resolve_binary(&self.binary);
        let mut child = Command::new(&bin)
            .arg("-p")
            .arg("--system-prompt")
            .arg(&system_prompt)
            .arg("--disallowedTools")
            .arg(DISALLOWED_TOOLS.join(" "))
            .env("PATH", shell_path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
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

        let output = child
            .wait_with_output()
            .await
            .map_err(|e| AppError::Summarize(format!("failed waiting on claude: {e}")))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(AppError::Summarize(format!(
                "claude exited with status {}: {}",
                output.status.code().unwrap_or(-1),
                stderr.trim()
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
        let bin = resolve_binary(&self.binary);
        let mut child = Command::new(&bin)
            .arg("-p")
            .arg("--system-prompt")
            .arg(system)
            .arg("--disallowedTools")
            .arg(DISALLOWED_TOOLS.join(" "))
            .env("PATH", shell_path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
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
        let output = child
            .wait_with_output()
            .await
            .map_err(|e| AppError::Summarize(format!("failed waiting on claude: {e}")))?;
        if !output.status.success() {
            return Err(AppError::Summarize(format!(
                "claude exited with status {}: {}",
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        String::from_utf8(output.stdout)
            .map(|s| s.trim().to_string())
            .map_err(|e| AppError::Summarize(format!("claude produced non-UTF8 output: {e}")))
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
    fn frontmatter_detection() {
        assert!(starts_with_frontmatter("---\ntitle: x\n---\n# X"));
        assert!(starts_with_frontmatter("---  \ntitle: x"));
        assert!(!starts_with_frontmatter("Here is your note:\n---"));
        assert!(!starts_with_frontmatter("----\ntitle: x"));
        assert!(!starts_with_frontmatter(""));
    }
}
