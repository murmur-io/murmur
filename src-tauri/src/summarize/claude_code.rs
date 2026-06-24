use std::process::Stdio;

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
        match Command::new(&self.binary)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
        {
            Ok(status) if status.success() => Availability::Available,
            Ok(status) => Availability::Unavailable {
                reason: format!(
                    "`{} --version` exited with status {}",
                    self.binary,
                    status.code().unwrap_or(-1)
                ),
            },
            Err(e) => Availability::Unavailable {
                reason: format!("`{}` not found in PATH ({e})", self.binary),
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

        let mut child = Command::new(&self.binary)
            .arg("-p")
            .arg("--system-prompt")
            .arg(&system_prompt)
            .arg("--disallowedTools")
            .arg(DISALLOWED_TOOLS.join(" "))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                AppError::Summarize(format!("failed to spawn `{}`: {e}", self.binary))
            })?;

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
