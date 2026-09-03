//! OpenAI Codex CLI provider.
//!
//! Codex is an agent CLI, not a plain text endpoint. Murmur therefore runs it as a deliberately
//! tool-free text transformer. CLI-enforced controls ignore user/project config and rules, disable
//! web/apps/plugins/multi-agent behavior, deny filesystem/network permissions, install an
//! explicitly empty MCP registry, and run a protocol-bound wildcard PreToolUse deny hook before
//! every native or MCP tool. The child gets a fresh `CODEX_HOME` containing at most a validated
//! symlink to `auth.json`; ambient config, hooks, plugins and connectors are structurally absent
//! even though Murmur's own immutable hook runs with hook-trust bypass. The JSONL parser is a
//! separate post-hoc tripwire: it rejects any tool event but is not treated as the preventer.
//! Pinned Codex 0.146.0 fixtures cover parser compatibility; a runner-owned loopback Responses
//! test additionally proves that the installed CLI honors the exact production hook and exposes
//! none of the disabled web/MCP/plugin/multi-agent capabilities without contacting OpenAI.
//! The process still rides the single provider egress seam in `summarize::make_provider_resolved`,
//! so consent, redaction, the content-free ledger and recording admission remain framework-owned.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use serde_json::Value;
use tokio::process::Command;

use crate::error::AppError;
use crate::summarize::claude_code::{
    harden_env, isolate_process_group, run_external_cli_child, run_external_cli_child_with_timeout,
    vet_binary,
};
use crate::summarize::provider::{Availability, SummarizeRequest, SummarizerProvider};
use crate::summarize::template;

const DEFAULT_BINARY: &str = "codex";
const DEFAULT_SEARCH_RELATIVE_DIRS: &[&str] = &[
    ".local/bin",
    ".bun/bin",
    ".deno/bin",
    ".volta/bin",
    ".npm-global/bin",
    ".asdf/shims",
    ".fnm/aliases/default/bin",
    ".nodenv/shims",
    ".n/bin",
    ".local/share/pnpm",
    "Library/pnpm",
];
const DEFAULT_SEARCH_SYSTEM_DIRS: &[&str] =
    &["/opt/homebrew/bin", "/opt/local/bin", "/usr/local/bin"];
const PROVIDER_LABEL: &str = "Codex CLI";
const EMPTY_CWD: &str = "/var/empty";
const SUPPORTED_CODEX_MAJOR: u64 = 0;
const SUPPORTED_CODEX_MINOR: u64 = 146;
#[cfg(not(test))]
const CODEX_VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
// Tests keep a SHORT budget so a wedged probe fails fast, but 2s encoded an assumption
// that a process spawn is quick — and that is false inside a sandbox. The boundary cases
// below spawn `sandbox-exec`, and running the suite under another Seatbelt profile (which
// the agent harness does for every check) nests one inside the other, where the spawn alone
// can exceed 2s. A correct implementation then failed the gate, and the failure looked
// exactly like a broken network boundary.
//
// 8s still fails fast against the 30s wedge these tests use, while leaving room for a spawn
// that is slow rather than stuck.
#[cfg(test)]
const CODEX_VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(8);
const RECOVERY_DIR_PREFIX: &str = "murmur-codex-recovery-";
const MAX_RECOVERY_DIRS: usize = 3;
const MAX_RECOVERY_AGE: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 60 * 60);
const TOOL_ATTEMPT_MARKER: &str = ".murmur-tool-attempted";
const CODEX_TOOL_ROUTER_LOG: &str = "codex_core::tools::router=error";
const TOOL_GUARD_COMMAND: &str = r#"umask 077 && /usr/bin/touch "$CODEX_HOME/.murmur-tool-attempted" && /usr/bin/printf '%s\n' '{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"Murmur Codex provider has no local tools."}}'"#;

/// Fixed, non-PII developer instructions. Meeting content is sent only on stdin, never in argv.
const DEVELOPER_CONFIG: &str = r#"developer_instructions="Act only as a tool-free text transformation engine for Murmur. Follow the task instructions and user payload supplied on stdin. Return only the requested content. Never call tools, inspect files, search the web, or access external connectors.""#;

/// Codex has no Claude-style tool allowlist. This immutable PreToolUse hook is the hard capability
/// boundary: every current or future native tool is denied before execution. It is copied from the
/// verifier-only Harness adapter; the installed-CLI loopback test below records the exact
/// production hook rejecting a synthetic command before execution.
fn tool_guard_config() -> String {
    let command = TOOL_GUARD_COMMAND
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    format!(
        r#"[{{matcher="*",hooks=[{{type="command",command="{command}",timeout=5,statusMessage="Blocking Codex tool access"}}]}}]"#
    )
}

const FILESYSTEM_PROFILE: &str = r#"permissions.murmur_provider.filesystem={":root"="deny",":tmpdir"="deny",":slash_tmp"="deny",":workspace_roots"={"."="deny"}}"#;
const NETWORK_PROFILE: &str = "permissions.murmur_provider.network.enabled=false";
const DEFAULT_PROFILE: &str = r#"default_permissions="murmur_provider""#;
const APPROVAL_POLICY: &str = r#"approval_policy="never""#;
const WEB_SEARCH_DISABLED: &str = r#"web_search="disabled""#;
const APPS_DISABLED: &str = "features.apps=false";
const PLUGINS_DISABLED: &str = "features.plugins=false";
const MULTI_AGENT_DISABLED: &str = "features.multi_agent=false";
const MCP_SERVERS_EMPTY: &str = "mcp_servers={}";
const NATIVE_TOOL_FEATURES: &[&str] = &[
    "shell_tool",
    "unified_exec",
    "code_mode_host",
    "browser_use",
    "browser_use_external",
    "browser_use_full_cdp_access",
    "computer_use",
    "image_generation",
    "request_permissions_tool",
    "tool_call_mcp_elicitation",
    "tool_suggest",
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct CodexBinaryIdentity {
    canonical_path: PathBuf,
    len: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    modified_seconds: i64,
    #[cfg(unix)]
    modified_nanoseconds: i64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
    #[cfg(not(unix))]
    modified_nanoseconds: u128,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct VerifiedCodexBinary {
    identity: CodexBinaryIdentity,
    boundary: VersionProbeNetworkBoundary,
    #[cfg(test)]
    version: String,
}

/// How many verified binaries the cache remembers at once.
///
/// Production holds exactly ONE: there is a single Codex executable, and a replaced one simply
/// misses on its new identity. The capacity exists because the cache is process-global and the test
/// suite drives many distinct fake binaries through it concurrently.
const VERIFIED_CODEX_BINARY_CACHE_CAP: usize = 32;

/// Verified Codex executables, keyed by the identity+boundary they were verified under.
///
/// This used to be a single `Option`, which stored whatever was verified LAST and threw away
/// everything else. With one real executable that is indistinguishable from a keyed cache, so
/// production never noticed. Under `cargo test --lib` — one process, many threads, each test
/// pointing at its own fake `codex` in its own temp dir — every store clobbered somebody else's
/// entry, so a caller that had just populated the cache could find a stranger's identity in it and
/// re-run a probe it had already paid for. That is how
/// `availability_runs_only_the_local_version_probe_and_finds_gui_install_paths` came to fail in a
/// full run and pass on its own: its second `availability_from` re-probed, writing a second `v` to
/// the execution marker.
///
/// Keying the entries removes the sharing rather than scheduling around it.
fn verified_codex_binary_cache() -> &'static Mutex<Vec<VerifiedCodexBinary>> {
    static CACHE: OnceLock<Mutex<Vec<VerifiedCodexBinary>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

/// Evaluator-only capability token binding model attribution to one vetted executable identity.
/// The absolute path alone is insufficient because its inode can be replaced during a long run.
#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct PinnedCodexRuntime {
    pub(crate) path: PathBuf,
    pub(crate) version: String,
    identity: CodexBinaryIdentity,
}

#[cfg(test)]
impl PinnedCodexRuntime {
    pub(crate) fn assert_unchanged(&self) -> crate::error::Result<()> {
        if codex_binary_identity(&self.path)? != self.identity {
            return Err(AppError::Unavailable(
                "pinned Codex runtime changed during the quality run".into(),
            ));
        }
        Ok(())
    }
}

struct CodexCliProvider {
    binary: String,
    model: String,
    effort: String,
    #[cfg(test)]
    runtime_probe: Option<CodexRuntimeProbe>,
    #[cfg(test)]
    pinned_runtime: Option<Arc<PinnedCodexRuntime>>,
}

#[cfg(test)]
struct CodexRuntimeProbe {
    provider_config: String,
    source_home: PathBuf,
    use_inner_network_sandbox: bool,
}

impl CodexCliProvider {
    fn new() -> Self {
        Self {
            binary: DEFAULT_BINARY.to_string(),
            model: String::new(),
            effort: String::new(),
            #[cfg(test)]
            runtime_probe: None,
            #[cfg(test)]
            pinned_runtime: None,
        }
    }

    fn with_model(mut self, model: String) -> Self {
        self.model = model;
        self
    }

    fn with_effort(mut self, effort: String) -> Self {
        self.effort = effort;
        self
    }

    #[cfg(test)]
    fn with_binary(binary: String) -> Self {
        Self {
            binary,
            model: String::new(),
            effort: String::new(),
            runtime_probe: None,
            pinned_runtime: None,
        }
    }

    #[cfg(test)]
    fn with_pinned_runtime(mut self, runtime: Arc<PinnedCodexRuntime>) -> Self {
        self.binary = runtime.path.to_string_lossy().into_owned();
        self.pinned_runtime = Some(runtime);
        self
    }

    #[cfg(test)]
    fn with_runtime_probe(
        mut self,
        provider_config: String,
        source_home: PathBuf,
        use_inner_network_sandbox: bool,
    ) -> Self {
        self.runtime_probe = Some(CodexRuntimeProbe {
            provider_config,
            source_home,
            use_inner_network_sandbox,
        });
        self
    }

    async fn run_text(&self, system: &str, user: &str) -> crate::error::Result<String> {
        let prompt = render_prompt(system, user);
        self.run_prompt(&prompt).await
    }

    async fn run_prompt(&self, prompt: &str) -> crate::error::Result<String> {
        ensure_empty_cwd()?;
        validate_model(&self.model)?;
        let bin = resolve_codex_binary(&self.binary)?;
        #[cfg(test)]
        self.assert_pinned_runtime_unchanged(&bin)?;
        verify_supported_codex_cli(&bin).await?;
        let mut runtime_home = self.prepare_runtime_home()?;
        let mut cmd = self.build_command(&bin, runtime_home.path());
        let child = cmd
            .spawn()
            .map_err(|error| AppError::Summarize(format!("failed to spawn `{bin}`: {error}")))?;
        let output_result = run_external_cli_child(child, prompt.as_bytes(), PROVIDER_LABEL).await;
        let tool_attempted = runtime_home.detect_tool_activity().map(|runtime_attempt| {
            runtime_attempt
                || output_result
                    .as_ref()
                    .is_ok_and(|output| stderr_reports_tool_activity(&output.stderr))
        });
        // OAuth refresh may atomically replace the isolated auth.json symlink. Synchronize that
        // replacement back before the disposable directory is removed, even when generation failed.
        let finalize_result = runtime_home.finalize();
        let output = match (output_result, finalize_result, tool_attempted) {
            (Ok(output), Ok(()), Ok(false)) => output,
            (Ok(_), Ok(()), Ok(true)) => {
                return Err(AppError::Summarize(
                    "Codex attempted disabled native activity; no content was accepted".into(),
                ));
            }
            (Ok(output), Err(sync_error), Ok(false)) => {
                // The content egress already succeeded. Keep the generated note instead of forcing
                // the user to resend the same meeting on retry; finalize retained an actionable,
                // private recovery directory for the credential rotation.
                tracing::error!(
                    target: "summarize",
                    error = %sync_error,
                    "Codex generation succeeded but auth synchronization requires recovery"
                );
                output
            }
            (Ok(_), Err(sync_error), Ok(true)) => {
                tracing::error!(
                    target: "summarize",
                    error = %sync_error,
                    "Codex auth synchronization also failed after disabled native activity"
                );
                return Err(AppError::Summarize(
                    "Codex attempted disabled native activity; no content was accepted".into(),
                ));
            }
            (_, _, Err(activity_error)) => return Err(activity_error),
            (Err(primary_error), Ok(()), Ok(_)) => return Err(primary_error),
            (Err(primary_error), Err(sync_error), Ok(_)) => {
                tracing::error!(
                    target: "summarize",
                    error = %sync_error,
                    "Codex auth synchronization also failed after the generation failure"
                );
                return Err(primary_error);
            }
        };

        #[cfg(test)]
        self.assert_pinned_runtime_unchanged(&bin)?;

        if !output.status.success() {
            let code = output.status.code().unwrap_or(-1);
            tracing::debug!(
                target: "summarize",
                code,
                stderr_len = output.stderr.len(),
                "codex CLI exited non-zero"
            );
            return Err(AppError::Summarize(codex_failure_message(
                code,
                &self.model,
            )));
        }

        let stdout = String::from_utf8(output.stdout).map_err(|error| {
            AppError::Summarize(format!("Codex produced non-UTF8 output: {error}"))
        })?;
        parse_codex_jsonl(&stdout)
    }

    fn prepare_runtime_home(&self) -> crate::error::Result<CodexRuntimeHome> {
        #[cfg(test)]
        if let Some(probe) = &self.runtime_probe {
            return CodexRuntimeHome::prepare_from(Some(&probe.source_home));
        }
        CodexRuntimeHome::prepare()
    }

    fn build_command(&self, bin: &str, runtime_home: &Path) -> Command {
        #[cfg(test)]
        if let Some(probe) = &self.runtime_probe {
            return build_local_runtime_probe_command(
                bin,
                &self.model,
                runtime_home,
                probe.provider_config.clone(),
                probe.use_inner_network_sandbox,
            );
        }
        build_codex_command_with_effort(bin, &self.model, &self.effort, runtime_home)
    }

    #[cfg(test)]
    fn assert_pinned_runtime_unchanged(&self, bin: &str) -> crate::error::Result<()> {
        let Some(runtime) = &self.pinned_runtime else {
            return Ok(());
        };
        if Path::new(bin) != runtime.path {
            return Err(AppError::Unavailable(
                "quality provider selected a different Codex runtime".into(),
            ));
        }
        runtime.assert_unchanged()
    }
}

impl Default for CodexCliProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl SummarizerProvider for CodexCliProvider {
    fn id(&self) -> &str {
        "codex_cli"
    }

    /// Readiness checks only explicit, inherited-data and well-known install locations without
    /// executing a login shell, vets the executable, requires a user-owned private `auth.json`,
    /// then runs the hardened local `codex --version` probe behind a deny-all-network boundary.
    /// It cannot contact OpenAI or load ambient Codex configuration, and it rejects an installed
    /// CLI outside the same version range generation accepts.
    async fn availability(&self) -> Availability {
        let _process_lease = match crate::perf::acquire_external_egress_lease(None) {
            Ok(lease) => lease,
            Err(error) => {
                return Availability::Unavailable {
                    reason: error.to_string(),
                }
            }
        };
        let codex_home = configured_codex_home();
        let user_home = dirs::home_dir();
        self.availability_from(codex_home.as_deref(), user_home.as_deref(), None)
            .await
    }

    async fn summarize(&self, req: &SummarizeRequest) -> crate::error::Result<String> {
        let prompt = render_summarize_stdin(req);
        let note = self.run_prompt(&prompt).await?;
        normalize_note_markdown(&note, req.template.trim().is_empty())
    }

    async fn complete(&self, system: &str, user: &str) -> crate::error::Result<String> {
        self.run_text(system, user).await
    }
}

impl CodexCliProvider {
    async fn availability_from(
        &self,
        codex_home: Option<&Path>,
        user_home: Option<&Path>,
        discovery_path: Option<&std::ffi::OsStr>,
    ) -> Availability {
        if let Err(error) = ensure_empty_cwd() {
            return Availability::Unavailable {
                reason: error.to_string(),
            };
        }
        let binary = match resolve_codex_binary_from(&self.binary, user_home, discovery_path) {
            Ok(binary) => binary,
            Err(error) => {
                return Availability::Unavailable {
                    reason: error.to_string(),
                };
            }
        };
        if let Err(error) = validate_executable(Path::new(&binary)) {
            return Availability::Unavailable {
                reason: error.to_string(),
            };
        }
        match validated_auth_path(codex_home) {
            Ok(Some(_)) => {
                if let Err(error) = verify_supported_codex_cli(&binary).await {
                    return Availability::Unavailable {
                        reason: error.to_string(),
                    };
                }
                #[cfg(not(test))]
                let auth_status = verify_codex_auth_status(&binary, codex_home).await;
                #[cfg(test)]
                let auth_status = verify_codex_auth_status_for_test(&binary, codex_home).await;
                match auth_status {
                    Ok(()) => Availability::Available,
                    Err(error) => Availability::Unavailable {
                        reason: error.to_string(),
                    },
                }
            }
            Ok(None) => Availability::Unavailable {
                reason: "Codex CLI is installed but not signed in — run `codex login` in Terminal"
                    .into(),
            },
            Err(error) => Availability::Unavailable {
                reason: error.to_string(),
            },
        }
    }
}

fn resolve_codex_binary(binary: &str) -> crate::error::Result<String> {
    let user_home = dirs::home_dir();
    resolve_codex_binary_from(binary, user_home.as_deref(), None)
}

/// Resolve the exact default CLI candidate production generation would execute. The quality
/// evaluator uses this seam so its runtime version/hash cannot drift to a different package-manager
/// install than [`CodexCliProvider::run_prompt`].
#[cfg(test)]
pub(crate) fn resolve_default_binary_path() -> crate::error::Result<PathBuf> {
    let resolved = resolve_codex_binary(DEFAULT_BINARY)?;
    fs::canonicalize(resolved)
        .map_err(|_| AppError::Unavailable("Codex executable identity could not be read".into()))
}

/// Resolve, vet and locally version-probe one immutable evaluator runtime token. The quality
/// runner passes this same token through every canonical provider wrapper and CloudReasoner build.
#[cfg(test)]
pub(crate) async fn resolve_pinned_default_runtime() -> crate::error::Result<PinnedCodexRuntime> {
    let path = resolve_default_binary_path()?;
    let path_string = path
        .to_str()
        .ok_or_else(|| AppError::Unavailable("Codex executable path is not UTF-8".into()))?;
    let verified = probe_supported_codex_version_production(path_string).await?;
    Ok(PinnedCodexRuntime {
        path: verified.identity.canonical_path.clone(),
        version: verified.version,
        identity: verified.identity,
    })
}

#[cfg(test)]
pub(crate) fn resolve_default_binary_path_from(
    user_home: Option<&Path>,
    discovery_path: Option<&std::ffi::OsStr>,
) -> crate::error::Result<PathBuf> {
    resolve_codex_binary_from(DEFAULT_BINARY, user_home, discovery_path).map(PathBuf::from)
}

fn resolve_codex_binary_from(
    binary: &str,
    user_home: Option<&Path>,
    discovery_path: Option<&std::ffi::OsStr>,
) -> crate::error::Result<String> {
    if binary.contains('/') {
        let path = Path::new(binary);
        vet_binary(path, PROVIDER_LABEL)?;
        validate_executable(path)?;
        return Ok(path.to_string_lossy().into_owned());
    }

    let mut search_dirs = Vec::new();
    // `discovery_path` is an injection seam for deterministic tests and callers that already own a
    // configuration-free PATH. Production deliberately passes None: starting a login shell here
    // would execute user startup files outside the cloud-egress boundary.
    if let Some(discovery_path) = discovery_path {
        search_dirs.extend(std::env::split_paths(discovery_path));
    }
    if let Some(home) = user_home {
        for relative in DEFAULT_SEARCH_RELATIVE_DIRS {
            search_dirs.push(home.join(relative));
        }
        if let Ok(node_versions) = fs::read_dir(home.join(".nvm/versions/node")) {
            let mut version_dirs = node_versions
                .filter_map(std::result::Result::ok)
                .map(|entry| entry.path().join("bin"))
                .collect::<Vec<_>>();
            version_dirs.sort();
            search_dirs.extend(version_dirs);
        }
    }
    search_dirs.extend(
        DEFAULT_SEARCH_SYSTEM_DIRS
            .iter()
            .copied()
            .map(PathBuf::from),
    );

    for directory in search_dirs {
        let candidate = directory.join(binary);
        if candidate.is_file()
            && vet_binary(&candidate, PROVIDER_LABEL).is_ok()
            && validate_executable(&candidate).is_ok()
        {
            return Ok(candidate.to_string_lossy().into_owned());
        }
    }

    Err(AppError::Unavailable(format!(
        "`{binary}` not found on a trusted filesystem path (or failed integrity checks)"
    )))
}

/// The only content-capable construction seam. The concrete provider remains private to this
/// module, so crate callers cannot bypass `summarize::make_provider_resolved` and its
/// consent/redaction/ledger wrapper.
pub(super) fn provider(model: String, effort: String) -> Arc<dyn SummarizerProvider> {
    Arc::new(
        CodexCliProvider::new()
            .with_model(model)
            .with_effort(effort),
    )
}

/// Evaluator-only raw transport constructor. Callers may use it only as the inner provider passed
/// to `make_provider_resolved`, which remains the owner of consent, redaction and ledger writes.
#[cfg(test)]
pub(crate) fn provider_with_pinned_runtime(
    model: String,
    effort: String,
    runtime: Arc<PinnedCodexRuntime>,
) -> Arc<dyn SummarizerProvider> {
    Arc::new(
        CodexCliProvider::new()
            .with_model(model)
            .with_effort(effort)
            .with_pinned_runtime(runtime),
    )
}

/// Availability-only surface for Settings. It returns no content-capable provider object.
pub(crate) async fn probe_availability() -> Availability {
    CodexCliProvider::new().availability().await
}

fn build_codex_command(bin: &str, model: &str, codex_home: &Path) -> Command {
    configure_codex_command(Command::new(bin), model, codex_home)
}

fn build_codex_command_with_effort(
    bin: &str,
    model: &str,
    effort: &str,
    codex_home: &Path,
) -> Command {
    // Preserve the exact pre-effort command path for an empty or rejected value. Besides keeping
    // existing installs byte-for-byte compatible, this makes the no-override construction remain
    // exercised by production rather than only by its command-shape tests.
    if effort_args(effort).is_empty() {
        return build_codex_command(bin, model, codex_home);
    }
    finish_codex_command_with_effort(
        configure_codex_command_prefix(Command::new(bin)),
        model,
        effort,
        codex_home,
    )
}

async fn verify_supported_codex_cli(bin: &str) -> crate::error::Result<()> {
    #[cfg(not(test))]
    {
        return verify_supported_codex_cli_production(bin).await;
    }
    #[cfg(test)]
    {
        verify_supported_codex_cli_with_boundary(
            bin,
            VersionProbeNetworkBoundary::InheritedTestSandbox,
        )
        .await
    }
}

#[cfg_attr(test, allow(dead_code))]
async fn verify_supported_codex_cli_production(bin: &str) -> crate::error::Result<()> {
    probe_supported_codex_version_production(bin)
        .await
        .map(|_| ())
}

async fn probe_supported_codex_version_production(
    bin: &str,
) -> crate::error::Result<VerifiedCodexBinary> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = bin;
        return Err(AppError::Unavailable(
            "Codex version readiness is unavailable on this platform because Murmur cannot enforce its local-only network boundary"
                .into(),
        ));
    }
    #[cfg(target_os = "macos")]
    {
        probe_supported_codex_version_with_boundary(bin, VersionProbeNetworkBoundary::MacosSeatbelt)
            .await
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VersionProbeNetworkBoundary {
    #[cfg(target_os = "macos")]
    MacosSeatbelt,
    #[cfg(test)]
    InheritedTestSandbox,
}

#[cfg(test)]
async fn verify_supported_codex_cli_with_boundary(
    bin: &str,
    boundary: VersionProbeNetworkBoundary,
) -> crate::error::Result<()> {
    probe_supported_codex_version_with_boundary(bin, boundary)
        .await
        .map(|_| ())
}

#[cfg(any(target_os = "macos", test))]
async fn probe_supported_codex_version_with_boundary(
    bin: &str,
    boundary: VersionProbeNetworkBoundary,
) -> crate::error::Result<VerifiedCodexBinary> {
    let identity = codex_binary_identity(Path::new(bin))?;
    if let Some(verified) = verified_codex_binary_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .find(|entry| entry.identity == identity && entry.boundary == boundary)
    {
        return Ok(verified.clone());
    }

    let mut command = codex_version_probe_command(bin, boundary);
    let child = command
        .spawn()
        .map_err(|_| AppError::Unavailable("Codex version probe could not start".into()))?;
    let output = run_external_cli_child_with_timeout(
        child,
        b"",
        "Codex CLI version probe",
        CODEX_VERSION_PROBE_TIMEOUT,
    )
    .await?;
    if !output.status.success() {
        return Err(AppError::Unavailable(
            "Codex version probe failed; install the supported Codex CLI release".into(),
        ));
    }
    let version = String::from_utf8(output.stdout)
        .map_err(|_| AppError::Unavailable("Codex version output was not UTF-8".into()))?;
    validate_supported_codex_version(&version)?;
    let post_probe_identity = codex_binary_identity(Path::new(bin))?;
    if post_probe_identity != identity {
        return Err(AppError::Unavailable(
            "Codex executable changed during the bounded version probe".into(),
        ));
    }
    let verified = VerifiedCodexBinary {
        identity,
        boundary,
        #[cfg(test)]
        version: version.trim().to_string(),
    };
    {
        let mut cache = verified_codex_binary_cache()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Replace this key's own entry rather than the whole cache, so a concurrent verification of
        // a DIFFERENT binary cannot evict one that is still in use.
        if let Some(slot) = cache
            .iter_mut()
            .find(|entry| entry.identity == verified.identity && entry.boundary == verified.boundary)
        {
            *slot = verified.clone();
        } else {
            if cache.len() >= VERIFIED_CODEX_BINARY_CACHE_CAP {
                // Oldest first. A cache that grew without bound would be the worse bug.
                cache.remove(0);
            }
            cache.push(verified.clone());
        }
    }
    Ok(verified)
}

#[cfg(any(target_os = "macos", test))]
fn codex_version_probe_command(bin: &str, boundary: VersionProbeNetworkBoundary) -> Command {
    #[cfg(target_os = "macos")]
    let mut command = match boundary {
        VersionProbeNetworkBoundary::MacosSeatbelt => {
            let mut command = Command::new("/usr/bin/sandbox-exec");
            command
                .arg("-p")
                .arg("(version 1)(allow default)(deny network*)")
                .arg(bin);
            command
        }
        #[cfg(test)]
        VersionProbeNetworkBoundary::InheritedTestSandbox => Command::new(bin),
    };
    #[cfg(all(not(target_os = "macos"), test))]
    let mut command = match boundary {
        VersionProbeNetworkBoundary::InheritedTestSandbox => Command::new(bin),
    };

    command
        .arg("--version")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .current_dir(EMPTY_CWD);
    isolate_process_group(&mut command);
    harden_env(&mut command, false);
    command
}

#[cfg_attr(test, allow(dead_code))]
async fn verify_codex_auth_status(
    bin: &str,
    source_home: Option<&Path>,
) -> crate::error::Result<()> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (bin, source_home);
        return Err(AppError::Unavailable(
            "Codex authentication readiness is unavailable on this platform because Murmur cannot enforce its local-only network boundary"
                .into(),
        ));
    }
    #[cfg(target_os = "macos")]
    {
        verify_codex_auth_status_with_boundary(
            bin,
            source_home,
            AuthStatusNetworkBoundary::MacosSeatbelt,
        )
        .await
    }
}

#[cfg(test)]
async fn verify_codex_auth_status_for_test(
    bin: &str,
    source_home: Option<&Path>,
) -> crate::error::Result<()> {
    verify_codex_auth_status_with_boundary(
        bin,
        source_home,
        AuthStatusNetworkBoundary::InheritedTestSandbox,
    )
    .await
}

#[derive(Clone, Copy)]
enum AuthStatusNetworkBoundary {
    #[cfg(target_os = "macos")]
    MacosSeatbelt,
    #[cfg(test)]
    InheritedTestSandbox,
}

#[cfg(any(target_os = "macos", test))]
async fn verify_codex_auth_status_with_boundary(
    bin: &str,
    source_home: Option<&Path>,
    boundary: AuthStatusNetworkBoundary,
) -> crate::error::Result<()> {
    let mut runtime_home = CodexRuntimeHome::prepare_from(source_home)?;
    let mut command = codex_auth_status_command(bin, runtime_home.path(), boundary);
    let child = command.spawn().map_err(|_| {
        AppError::Unavailable(
            "Codex authentication status could not be checked — run `codex login` in Terminal"
                .into(),
        )
    })?;
    let output_result = run_external_cli_child_with_timeout(
        child,
        b"",
        "Codex CLI authentication status",
        CODEX_VERSION_PROBE_TIMEOUT,
    )
    .await;
    let finalize_result = runtime_home.finalize();
    let output = match (output_result, finalize_result) {
        (Ok(output), Ok(())) => output,
        (Err(error), Ok(())) => return Err(error),
        (Ok(_), Err(error)) | (Err(_), Err(error)) => return Err(error),
    };
    if !output.status.success() {
        return Err(AppError::Unavailable(
            "Codex CLI is installed but not signed in — run `codex login` in Terminal".into(),
        ));
    }
    let stdout = String::from_utf8(output.stdout).map_err(|_| {
        AppError::Unavailable(
            "Codex authentication status was not recognized — run `codex login status` in Terminal"
                .into(),
        )
    })?;
    let status = stdout.trim();
    if status.lines().count() != 1 || !status.starts_with("Logged in using ") {
        return Err(AppError::Unavailable(
            "Codex authentication status was not recognized — run `codex login status` in Terminal"
                .into(),
        ));
    }
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn codex_auth_status_command(
    bin: &str,
    codex_home: &Path,
    boundary: AuthStatusNetworkBoundary,
) -> Command {
    #[cfg(target_os = "macos")]
    let mut command = match boundary {
        AuthStatusNetworkBoundary::MacosSeatbelt => {
            // Production readiness remains a local file-state check even if a future CLI release
            // changes `login status`. This branch is unconditional in production code.
            let mut command = Command::new("/usr/bin/sandbox-exec");
            command
                .arg("-p")
                .arg("(version 1)(allow default)(deny network*)")
                .arg(bin);
            command
        }
        #[cfg(test)]
        AuthStatusNetworkBoundary::InheritedTestSandbox => Command::new(bin),
    };
    #[cfg(all(not(target_os = "macos"), test))]
    let mut command = match boundary {
        AuthStatusNetworkBoundary::InheritedTestSandbox => Command::new(bin),
    };

    command
        .arg("login")
        .arg("status")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .current_dir(EMPTY_CWD);
    isolate_process_group(&mut command);
    harden_env(&mut command, false);
    command.env("CODEX_HOME", codex_home);
    command
}

fn codex_binary_identity(path: &Path) -> crate::error::Result<CodexBinaryIdentity> {
    let canonical_path = fs::canonicalize(path)
        .map_err(|_| AppError::Unavailable("Codex executable identity could not be read".into()))?;
    let metadata = fs::metadata(&canonical_path)
        .map_err(|_| AppError::Unavailable("Codex executable metadata could not be read".into()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(CodexBinaryIdentity {
            canonical_path,
            len: metadata.len(),
            device: metadata.dev(),
            inode: metadata.ino(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
            changed_seconds: metadata.ctime(),
            changed_nanoseconds: metadata.ctime_nsec(),
        })
    }
    #[cfg(not(unix))]
    {
        let modified_nanoseconds = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(std::time::SystemTime::UNIX_EPOCH).ok())
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        Ok(CodexBinaryIdentity {
            canonical_path,
            len: metadata.len(),
            modified_nanoseconds,
        })
    }
}

fn validate_supported_codex_version(version: &str) -> crate::error::Result<()> {
    let token = version.trim().strip_prefix("codex-cli ").ok_or_else(|| {
        AppError::Unavailable("Codex returned an unrecognized version string".into())
    })?;
    let mut parts = token.split('.');
    let major = parts.next().and_then(|part| part.parse::<u64>().ok());
    let minor = parts.next().and_then(|part| part.parse::<u64>().ok());
    let patch = parts.next().and_then(|part| part.parse::<u64>().ok());
    if major == Some(SUPPORTED_CODEX_MAJOR)
        && minor == Some(SUPPORTED_CODEX_MINOR)
        && patch.is_some()
        && parts.next().is_none()
    {
        return Ok(());
    }
    Err(AppError::Unavailable(format!(
        "Murmur requires Codex CLI {SUPPORTED_CODEX_MAJOR}.{SUPPORTED_CODEX_MINOR}.x for its verified tool-isolation contract; found `{token}`"
    )))
}

fn configure_codex_command(cmd: Command, model: &str, codex_home: &Path) -> Command {
    finish_codex_command(configure_codex_command_prefix(cmd), model, codex_home)
}

fn configure_codex_command_prefix(cmd: Command) -> Command {
    configure_codex_command_prefix_with_tool_guard(cmd, true)
}

fn configure_codex_command_prefix_with_tool_guard(
    mut cmd: Command,
    install_tool_guard: bool,
) -> Command {
    cmd.arg("exec")
        .arg("--ephemeral")
        .arg("--ignore-user-config")
        .arg("--strict-config")
        .arg("--config")
        .arg(FILESYSTEM_PROFILE)
        .arg("--config")
        .arg(NETWORK_PROFILE)
        .arg("--config")
        .arg(DEFAULT_PROFILE)
        .arg("--cd")
        .arg(EMPTY_CWD)
        .arg("--skip-git-repo-check")
        .arg("--ignore-rules");
    if install_tool_guard {
        cmd.arg("--dangerously-bypass-hook-trust")
            .arg("--enable")
            .arg("hooks")
            .arg("--config")
            .arg(format!("hooks.PreToolUse={}", tool_guard_config()));
    }
    for feature in NATIVE_TOOL_FEATURES {
        cmd.arg("--disable").arg(feature);
    }
    cmd.arg("--config")
        .arg(APPROVAL_POLICY)
        .arg("--config")
        .arg(WEB_SEARCH_DISABLED)
        .arg("--config")
        .arg(APPS_DISABLED)
        .arg("--config")
        .arg(PLUGINS_DISABLED)
        .arg("--config")
        .arg(MULTI_AGENT_DISABLED)
        .arg("--config")
        .arg(MCP_SERVERS_EMPTY)
        .arg("--config")
        .arg(DEVELOPER_CONFIG);
    cmd
}

/// The `--model <id>` argument pair for this arm, or `&[]` when the id is blank or unusable.
///
/// Same argv-injection guard as `claude_code::model_args`, and now the same SHAPE: the rule was
/// inline in `finish_codex_command`, which meant the two CLI arms could drift and neither could be
/// asked "what would you actually send?" without building a whole command. `pub(crate)` so the A6
/// ledger-versus-wire test can ask exactly that, rather than re-implementing the rule in the test
/// and proving only that two copies of it agree.
pub(crate) fn model_args(model: &str) -> Vec<String> {
    if !crate::summarize::provider::valid_model_id(model) {
        return Vec::new();
    }
    vec!["--model".to_string(), model.trim().to_string()]
}

/// The strict-config override for Codex reasoning effort. Unknown values are omitted rather than
/// forwarded: this field is persisted user input and must never become an arbitrary CLI config
/// expression. Empty means Codex's provider default, matching every pre-existing install.
pub(crate) fn effort_args(effort: &str) -> Vec<String> {
    let effort = effort.trim();
    if !matches!(effort, "low" | "medium" | "high") {
        return Vec::new();
    }
    vec![
        "--config".to_string(),
        format!("model_reasoning_effort=\"{effort}\""),
    ]
}

fn finish_codex_command(cmd: Command, model: &str, codex_home: &Path) -> Command {
    finish_codex_command_with_effort(cmd, model, "", codex_home)
}

fn finish_codex_command_with_effort(
    mut cmd: Command,
    model: &str,
    effort: &str,
    codex_home: &Path,
) -> Command {
    // The public JSONL stream intentionally omits internal unsupported-call events. Restrict the
    // child logger to the tool router's errors so Murmur can reject such an event without enabling
    // request/prompt tracing or recording content.
    cmd.env("RUST_LOG", CODEX_TOOL_ROUTER_LOG);
    cmd.args(model_args(model));
    cmd.args(effort_args(effort));
    cmd.arg("--json")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .current_dir(EMPTY_CWD);

    isolate_process_group(&mut cmd);
    // Empty-by-default environment, with only the same non-secret runtime basics as Claude.
    harden_env(&mut cmd, false);
    // The fresh home contains no config, rules, MCP registry, plugin, or hook source. It may contain
    // only a validated auth.json symlink, allowing normal ChatGPT/API auth without granting the
    // hook-trust bypass to anything from the user's real CODEX_HOME.
    cmd.env("CODEX_HOME", codex_home);
    cmd
}

fn stderr_reports_tool_activity(stderr: &[u8]) -> bool {
    stderr
        .windows(b"unsupported call:".len())
        .any(|window| window == b"unsupported call:")
        || stderr
            .windows(b"unsupported custom tool call:".len())
            .any(|window| window == b"unsupported custom tool call:")
}

#[cfg(test)]
fn build_schema_probe_command(
    bin: &str,
    model: &str,
    codex_home: &Path,
    use_inner_network_sandbox: bool,
) -> Command {
    let cmd = if use_inner_network_sandbox {
        let mut command = Command::new("/usr/bin/sandbox-exec");
        command
            .arg("-p")
            .arg("(version 1)(allow default)(deny network*)")
            .arg(bin);
        command
    } else {
        Command::new(bin)
    };
    configure_codex_command(cmd, model, codex_home)
}

#[cfg(test)]
fn build_local_runtime_probe_command(
    bin: &str,
    model: &str,
    codex_home: &Path,
    provider_config: String,
    use_inner_network_sandbox: bool,
) -> Command {
    let cmd = if use_inner_network_sandbox {
        let mut command = Command::new("/usr/bin/sandbox-exec");
        command
            .arg("-p")
            .arg(
                r#"(version 1)(allow default)(deny network-bind (require-not (local ip "localhost:*")))(deny network-inbound (require-not (local ip "localhost:*")))(deny network-outbound (require-not (remote ip "localhost:*")))"#,
            )
            .arg(bin);
        command
    } else {
        Command::new(bin)
    };
    let mut cmd = configure_codex_command_prefix(cmd);
    cmd.arg("--config")
        .arg(provider_config)
        .arg("--config")
        .arg(r#"model_provider="murmur_runtime_test""#)
        .arg("--config")
        .arg("analytics.enabled=false");
    finish_codex_command(cmd, model, codex_home)
}

#[cfg(test)]
fn build_local_permission_probe_command(
    bin: &str,
    model: &str,
    codex_home: &Path,
    provider_config: String,
    use_inner_network_sandbox: bool,
) -> Command {
    let cmd = if use_inner_network_sandbox {
        let mut command = Command::new("/usr/bin/sandbox-exec");
        command
            .arg("-p")
            .arg(
                r#"(version 1)(allow default)(deny network-bind (require-not (local ip "localhost:*")))(deny network-inbound (require-not (local ip "localhost:*")))(deny network-outbound (require-not (remote ip "localhost:*")))"#,
            )
            .arg(bin);
        command
    } else {
        Command::new(bin)
    };
    let mut cmd = configure_codex_command_prefix_with_tool_guard(cmd, false);
    cmd.arg("--config")
        .arg(provider_config)
        .arg("--config")
        .arg(r#"model_provider="murmur_runtime_test""#)
        .arg("--config")
        .arg("analytics.enabled=false");
    finish_codex_command(cmd, model, codex_home)
}

/// Per-call Codex home. The directory is mode 0700 and contains no executable configuration.
/// File-backed auth is exposed as a symlink to the user's validated private `auth.json`.
/// If Codex atomically replaces that link during refresh, [`Self::finalize`] copies the replacement
/// into the real auth directory through a mode-0600 staging file and an atomic rename.
struct CodexRuntimeHome {
    path: PathBuf,
    auth_destination: Option<PathBuf>,
    original_auth: Option<PathBuf>,
    finalized: bool,
    recovery_path: Option<PathBuf>,
}

impl CodexRuntimeHome {
    fn prepare() -> crate::error::Result<Self> {
        let source_home = configured_codex_home();
        Self::prepare_from(source_home.as_deref())
    }

    fn prepare_from(source_home: Option<&Path>) -> crate::error::Result<Self> {
        sweep_codex_recovery_dirs_in(&std::env::temp_dir());
        let path = std::env::temp_dir().join(format!("murmur-codex-{}", uuid::Uuid::new_v4()));
        create_private_directory(&path)?;

        let mut home = Self {
            path,
            auth_destination: None,
            original_auth: None,
            finalized: false,
            recovery_path: None,
        };
        if let Some(canonical) = validated_auth_path(source_home)? {
            create_auth_symlink(&canonical, &home.path.join("auth.json"))?;
            home.auth_destination = Some(canonical.clone());
            home.original_auth = Some(canonical);
        }
        Ok(home)
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn detect_tool_activity(&self) -> crate::error::Result<bool> {
        let marker = self.path.join(TOOL_ATTEMPT_MARKER);
        match fs::symlink_metadata(&marker) {
            Ok(_) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err(AppError::Summarize(
                "Codex tool-activity marker could not be inspected safely".into(),
            )),
        }
    }

    fn finalize(&mut self) -> crate::error::Result<()> {
        match self.finalize_inner() {
            Ok(()) => {
                self.finalized = true;
                Ok(())
            }
            Err(error) => {
                // Preserve only a validated refreshed credential. The disposable CODEX_HOME may
                // contain CLI session artifacts, so it is always deleted in Drop and is never
                // renamed wholesale into the bounded recovery pool.
                self.recovery_path = self.retain_refreshed_auth_only();
                self.finalized = true;
                let recovery = self
                    .recovery_path
                    .as_ref()
                    .map(|path| format!("; refreshed auth was retained at `{}`", path.display()))
                    .unwrap_or_else(|| {
                        "; no credential could be retained safely; runtime artifacts were removed"
                            .into()
                    });
                Err(AppError::Unavailable(format!(
                    "Codex auth synchronization failed{recovery} ({error})"
                )))
            }
        }
    }

    fn retain_refreshed_auth_only(&self) -> Option<PathBuf> {
        let runtime_auth = self.path.join("auth.json");
        let metadata = fs::symlink_metadata(&runtime_auth).ok()?;
        if !metadata.file_type().is_file() || validate_auth_metadata(&metadata).is_err() {
            return None;
        }
        let parent = self.path.parent()?;
        let recovery = parent.join(format!("{RECOVERY_DIR_PREFIX}{}", uuid::Uuid::new_v4()));
        if create_private_directory(&recovery).is_err() {
            return None;
        }
        if fs::rename(&runtime_auth, recovery.join("auth.json")).is_err() {
            let _ = fs::remove_dir_all(&recovery);
            return None;
        }
        Some(recovery)
    }

    fn finalize_inner(&self) -> crate::error::Result<()> {
        if self.finalized {
            return Ok(());
        }
        let runtime_auth = self.path.join("auth.json");
        let metadata = match fs::symlink_metadata(&runtime_auth) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => {
                return Err(AppError::Unavailable(
                    "Codex refreshed auth metadata could not be inspected safely".into(),
                ));
            }
        };

        if metadata.file_type().is_symlink() {
            let target = fs::canonicalize(&runtime_auth).map_err(|_| {
                AppError::Unavailable("Codex auth link could not be re-verified".into())
            })?;
            if self.original_auth.as_ref() == Some(&target) {
                return Ok(());
            }
            return Err(AppError::Unavailable(
                "Codex auth link changed during generation; auth-only recovery will be attempted"
                    .into(),
            ));
        }
        if !metadata.is_file() {
            return Err(AppError::Unavailable(
                "Codex produced an invalid refreshed auth entry; runtime artifacts will be removed"
                    .into(),
            ));
        }
        validate_auth_metadata(&metadata)?;
        let destination = self.auth_destination.as_ref().ok_or_else(|| {
            AppError::Unavailable(
                "Codex refreshed auth has no safe destination; auth-only recovery will be attempted"
                    .into(),
            )
        })?;
        sync_refreshed_auth(&runtime_auth, destination)
    }
}

fn sweep_codex_recovery_dirs_in(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    let mut recoveries = entries
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            if !name.to_string_lossy().starts_with(RECOVERY_DIR_PREFIX) {
                return None;
            }
            let metadata = fs::symlink_metadata(entry.path()).ok()?;
            if !metadata.file_type().is_dir() {
                return None;
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::{MetadataExt, PermissionsExt};
                if metadata.uid() != current_uid() || metadata.permissions().mode() & 0o077 != 0 {
                    return None;
                }
            }
            Some((
                metadata
                    .modified()
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                entry.path(),
            ))
        })
        .collect::<Vec<_>>();
    recoveries.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    let now = std::time::SystemTime::now();
    for (index, (modified, path)) in recoveries.into_iter().enumerate() {
        let too_old = now
            .duration_since(modified)
            .is_ok_and(|age| age > MAX_RECOVERY_AGE);
        if index >= MAX_RECOVERY_DIRS || too_old {
            let _ = fs::remove_dir_all(path);
        }
    }
}

impl Drop for CodexRuntimeHome {
    fn drop(&mut self) {
        if !self.finalized {
            if let Err(error) = self.finalize() {
                tracing::error!(
                    target: "summarize",
                    error = %error,
                    "Codex auth synchronization failed during runtime-home teardown"
                );
            }
        }
        // The UUID path was created by this instance with mode 0700. It is always disposable:
        // a separately-created recovery directory contains at most the validated auth.json.
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn configured_codex_home() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".codex")))
}

fn validated_auth_path(source_home: Option<&Path>) -> crate::error::Result<Option<PathBuf>> {
    let Some(source_home) = source_home else {
        return Ok(None);
    };
    let auth_source = source_home.join("auth.json");
    match fs::canonicalize(&auth_source) {
        Ok(canonical) => {
            let metadata = fs::metadata(&canonical).map_err(|_| {
                AppError::Unavailable("Codex auth file metadata could not be read safely".into())
            })?;
            validate_auth_metadata(&metadata)?;
            Ok(Some(canonical))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(AppError::Unavailable(
            "Codex auth file metadata could not be read safely".into(),
        )),
    }
}

fn validate_executable(path: &Path) -> crate::error::Result<()> {
    let metadata = fs::metadata(path)
        .map_err(|_| AppError::Unavailable("Codex binary metadata could not be read".into()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(AppError::Unavailable(
                "Codex binary is installed but not executable".into(),
            ));
        }
    }
    Ok(())
}

fn create_private_directory(path: &Path) -> crate::error::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(|_| {
                AppError::Unavailable("Codex isolated auth directory could not be created".into())
            })?;
    }
    #[cfg(not(unix))]
    fs::create_dir(path).map_err(|_| {
        AppError::Unavailable("Codex isolated auth directory could not be created".into())
    })?;
    Ok(())
}

fn validate_auth_metadata(metadata: &fs::Metadata) -> crate::error::Result<()> {
    if !metadata.is_file() {
        return Err(AppError::Unavailable(
            "Codex auth source is not a regular file".into(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != current_uid() || metadata.permissions().mode() & 0o077 != 0 {
            return Err(AppError::Unavailable(
                "Codex auth file must be user-owned and private".into(),
            ));
        }
    }
    Ok(())
}

fn sync_refreshed_auth(source: &Path, destination: &Path) -> crate::error::Result<()> {
    use std::io::{Read, Write};

    let parent = destination.parent().ok_or_else(|| {
        AppError::Unavailable(
            "Codex auth destination has no safe parent; auth-only recovery will be attempted"
                .into(),
        )
    })?;
    let staging = parent.join(format!(".murmur-codex-auth-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> crate::error::Result<()> {
        let mut input = fs::File::open(source).map_err(|_| {
            AppError::Unavailable(
                "Codex refreshed auth could not be opened; auth-only recovery will be attempted"
                    .into(),
            )
        })?;
        #[cfg(unix)]
        let output = {
            use std::os::unix::fs::OpenOptionsExt;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&staging)
        };
        #[cfg(not(unix))]
        let output = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging);
        let mut output = output.map_err(|_| {
            AppError::Unavailable(
                "Codex refreshed auth staging file could not be created; auth-only recovery will be attempted"
                    .into(),
            )
        })?;
        let mut buffer = [0_u8; 8192];
        let mut total = 0_u64;
        loop {
            let read = input.read(&mut buffer).map_err(|_| {
                AppError::Unavailable(
                    "Codex refreshed auth could not be copied; auth-only recovery will be attempted"
                        .into(),
                )
            })?;
            if read == 0 {
                break;
            }
            output.write_all(&buffer[..read]).map_err(|_| {
                AppError::Unavailable(
                    "Codex refreshed auth could not be copied; auth-only recovery will be attempted"
                        .into(),
                )
            })?;
            total = total.saturating_add(read as u64);
        }
        if total == 0 {
            return Err(AppError::Unavailable(
                "Codex produced an empty refreshed auth file; auth-only recovery will be attempted"
                    .into(),
            ));
        }
        output.sync_all().map_err(|_| {
            AppError::Unavailable(
                "Codex refreshed auth could not be flushed; auth-only recovery will be attempted"
                    .into(),
            )
        })?;
        drop(output);
        fs::rename(&staging, destination).map_err(|_| {
            AppError::Unavailable(
                "Codex refreshed auth could not be installed atomically; auth-only recovery will be attempted"
                    .into(),
            )
        })?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&staging);
    }
    result
}

#[cfg(unix)]
fn create_auth_symlink(source: &Path, target: &Path) -> crate::error::Result<()> {
    std::os::unix::fs::symlink(source, target)
        .map_err(|_| AppError::Unavailable("Codex isolated auth link could not be created".into()))
}

#[cfg(not(unix))]
fn create_auth_symlink(_source: &Path, _target: &Path) -> crate::error::Result<()> {
    Err(AppError::Unavailable(
        "Codex file-backed authentication is supported only on macOS".into(),
    ))
}

#[cfg(unix)]
fn current_uid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: getuid(2) takes no pointers and always succeeds.
    unsafe { getuid() }
}

fn ensure_empty_cwd() -> crate::error::Result<()> {
    if Path::new(EMPTY_CWD).is_dir() {
        Ok(())
    } else {
        Err(AppError::Unavailable(format!(
            "Codex isolation directory `{EMPTY_CWD}` is unavailable"
        )))
    }
}

fn validate_model(model: &str) -> crate::error::Result<()> {
    if model.trim_start().starts_with('-') {
        return Err(AppError::InvalidArg(
            "Codex model id must not begin with `-`".into(),
        ));
    }
    Ok(())
}

pub(crate) fn render_prompt(system: &str, user: &str) -> String {
    let system = system.replace("</task>", "<\\/task>");
    let user = user.replace("</payload>", "<\\/payload>");
    format!(
        "TASK INSTRUCTIONS\n<task>\n{system}\n</task>\n\nUSER PAYLOAD\n<payload>\n{user}\n</payload>\n\nReturn only the requested final content."
    )
}

pub(crate) fn render_summarize_stdin(req: &SummarizeRequest) -> String {
    let system = if req.template.trim().is_empty() {
        template::default_template()
    } else {
        req.template.clone()
    };
    let user = template::render_user_content(req);
    render_prompt(&system, &user)
}

fn normalize_note_markdown(text: &str, require_frontmatter: bool) -> crate::error::Result<String> {
    let text = text.trim_start_matches('\u{feff}').trim();
    let mut lines = Vec::new();
    let mut offset = 0;
    for chunk in text.split_inclusive('\n') {
        let line = chunk.strip_suffix('\n').unwrap_or(chunk);
        let line = line.strip_suffix('\r').unwrap_or(line);
        lines.push((offset, line));
        offset += chunk.len();
    }

    let Some((frontmatter_index, frontmatter_end_index)) =
        lines.iter().enumerate().find_map(|(start, (_, line))| {
            if *line != "---" {
                return None;
            }
            let end = lines[start + 1..]
                .iter()
                .position(|(_, candidate)| *candidate == "---")
                .map(|relative| start + relative + 1)?;
            looks_like_yaml_frontmatter(&lines[start + 1..end]).then_some((start, end))
        })
    else {
        if require_frontmatter {
            return Err(AppError::Summarize(
                "Codex output did not contain a complete YAML front-matter block".into(),
            ));
        }
        return Ok(normalize_custom_markdown_without_frontmatter(text, &lines));
    };

    let wrapper_fence_consumed = lines[..frontmatter_index]
        .iter()
        .rev()
        .find(|(_, line)| !line.trim().is_empty())
        .is_some_and(|(_, line)| is_markdown_wrapper_fence(line));
    let frontmatter_offset = lines[frontmatter_index].0;
    let mut note_end = text.len();

    if wrapper_fence_consumed {
        if let Some((closing_index, (closing_offset, _))) = lines
            .iter()
            .enumerate()
            .rev()
            .find(|(_, (_, line))| !line.trim().is_empty())
            .filter(|(_, (_, line))| line.trim() == "```")
        {
            // Removing a wrapper is safe only when every fence inside the note is balanced. This
            // keeps the final fence of a truncated wrapper's legitimate code block intact.
            let internal_fences = lines[frontmatter_end_index + 1..closing_index]
                .iter()
                .filter(|(_, line)| line.trim().starts_with("```"))
                .count();
            if internal_fences % 2 == 0 {
                note_end = *closing_offset;
            }
        }
    }

    Ok(text[frontmatter_offset..note_end].trim().to_string())
}

fn normalize_custom_markdown_without_frontmatter(text: &str, lines: &[(usize, &str)]) -> String {
    let Some((opening_index, _)) = lines
        .iter()
        .enumerate()
        .find(|(_, (_, line))| !line.trim().is_empty())
        .filter(|(_, (_, line))| is_markdown_wrapper_fence(line))
    else {
        return text.to_string();
    };
    let Some((closing_index, (closing_offset, _))) = lines
        .iter()
        .enumerate()
        .rev()
        .find(|(_, (_, line))| !line.trim().is_empty())
        .filter(|(index, (_, line))| *index > opening_index && line.trim() == "```")
    else {
        return text.to_string();
    };
    let internal_fences = lines[opening_index + 1..closing_index]
        .iter()
        .filter(|(_, line)| line.trim().starts_with("```"))
        .count();
    if internal_fences % 2 != 0 {
        return text.to_string();
    }
    let body_start = lines
        .get(opening_index + 1)
        .map(|(offset, _)| *offset)
        .unwrap_or(text.len());
    text[body_start..*closing_offset].trim().to_string()
}

fn looks_like_yaml_frontmatter(lines: &[(usize, &str)]) -> bool {
    let mut saw_mapping = false;
    for (_, line) in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if line.chars().next().is_some_and(char::is_whitespace) || trimmed.starts_with("- ") {
            continue;
        }
        let Some((key, _)) = trimmed.split_once(':') else {
            return false;
        };
        if key.trim().is_empty()
            || key.chars().any(|character| {
                character.is_control() || matches!(character, '[' | ']' | '{' | '}')
            })
        {
            return false;
        }
        saw_mapping = true;
    }
    saw_mapping
}

fn is_markdown_wrapper_fence(line: &str) -> bool {
    matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "```" | "```md" | "```markdown"
    )
}

fn protocol_incompatibility(detail: &str) -> AppError {
    AppError::Summarize(format!(
        "Codex CLI output protocol is incompatible with Murmur's verified 0.146.x contract ({detail}); install Codex CLI 0.146.x or update Murmur. No content was accepted"
    ))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodexJsonlState {
    AwaitingThread,
    AwaitingTurn,
    InTurn,
    Completed,
}

fn parse_codex_jsonl(stdout: &str) -> crate::error::Result<String> {
    let mut final_message: Option<String> = None;
    let mut state = CodexJsonlState::AwaitingThread;

    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        if state == CodexJsonlState::Completed {
            return Err(protocol_incompatibility("event after terminal turn event"));
        }
        let event: Value = serde_json::from_str(line).map_err(|_| {
            AppError::Summarize("Codex returned malformed JSONL; no content was accepted".into())
        })?;
        let event_type = event.get("type").and_then(Value::as_str).ok_or_else(|| {
            AppError::Summarize(
                "Codex returned an event without a type; no content was accepted".into(),
            )
        })?;
        match event_type {
            "thread.started" => {
                if state != CodexJsonlState::AwaitingThread {
                    return Err(protocol_incompatibility(
                        "duplicate or out-of-order thread start",
                    ));
                }
                state = CodexJsonlState::AwaitingTurn;
            }
            "turn.started" => {
                if state != CodexJsonlState::AwaitingTurn {
                    return Err(protocol_incompatibility(
                        "duplicate or out-of-order turn start",
                    ));
                }
                state = CodexJsonlState::InTurn;
            }
            "turn.completed" => {
                if state != CodexJsonlState::InTurn {
                    return Err(protocol_incompatibility(
                        "turn completed outside an active turn",
                    ));
                }
                if final_message.is_none() {
                    return Err(AppError::Summarize(
                        "Codex completed without a final text response".into(),
                    ));
                }
                state = CodexJsonlState::Completed;
            }
            "turn.failed" | "error" => {
                return Err(AppError::Summarize(
                    "Codex reported a failed turn; stderr and event details were suppressed".into(),
                ));
            }
            item_event @ ("item.started" | "item.updated" | "item.completed") => {
                let item = event
                    .get("item")
                    .and_then(Value::as_object)
                    .ok_or_else(|| {
                        AppError::Summarize(
                            "Codex returned a malformed item event; no content was accepted".into(),
                        )
                    })?;
                let item_type = item.get("type").and_then(Value::as_str).ok_or_else(|| {
                    AppError::Summarize(
                        "Codex returned an untyped item event; no content was accepted".into(),
                    )
                })?;
                // Codex emits this process-local trust advisory independently of model output.
                // Admit only its exact completed error shape before the active turn, including
                // before thread.started; every other pre-thread/pre-turn item still fails closed.
                let pre_turn_hook_notice = matches!(
                    state,
                    CodexJsonlState::AwaitingThread | CodexJsonlState::AwaitingTurn
                ) && item_event == "item.completed"
                    && item_type == "error"
                    && item
                        .get("message")
                        .and_then(Value::as_str)
                        .is_some_and(is_hook_trust_notice);
                if pre_turn_hook_notice {
                    continue;
                }
                if state != CodexJsonlState::InTurn {
                    return Err(protocol_incompatibility(
                        "item event outside an active turn",
                    ));
                }
                match item_type {
                    "reasoning" => {}
                    "agent_message" => {
                        if item_event == "item.completed" {
                            let text =
                                item.get("text").and_then(Value::as_str).ok_or_else(|| {
                                    AppError::Summarize(
                                        "Codex returned an agent message without text".into(),
                                    )
                                })?;
                            if final_message.replace(text.to_string()).is_some() {
                                return Err(AppError::Summarize(
                                    "Codex returned multiple final messages; no content was accepted"
                                        .into(),
                                ));
                            }
                        }
                    }
                    "error" if item_event == "item.completed" => {
                        let message = item.get("message").and_then(Value::as_str).unwrap_or("");
                        if !is_hook_trust_notice(message) {
                            return Err(AppError::Summarize(
                                "Codex reported an error item; details were suppressed".into(),
                            ));
                        }
                    }
                    "command_execution" | "file_change" | "mcp_tool_call" | "web_search"
                    | "image_generation" => {
                        return Err(AppError::Summarize(
                            "Codex attempted disabled native activity; no content was accepted"
                                .into(),
                        ));
                    }
                    _ => {
                        return Err(protocol_incompatibility("unsupported item type"));
                    }
                }
            }
            _ => {
                return Err(protocol_incompatibility("unsupported event type"));
            }
        }
    }

    if state != CodexJsonlState::Completed {
        return Err(AppError::Summarize(
            "Codex output ended before turn completion; no content was accepted".into(),
        ));
    }
    let message = final_message.ok_or_else(|| {
        AppError::Summarize("Codex completed without a final text response".into())
    })?;
    if message.trim().is_empty() {
        return Err(AppError::Summarize(
            "Codex completed with an empty final text response".into(),
        ));
    }
    Ok(message.trim().to_string())
}

fn is_hook_trust_notice(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    message.starts_with("`--dangerously-bypass-hook-trust` is enabled.")
        && lower.contains("enabled hooks may run without review")
        && lower.ends_with("for this invocation.")
}

fn codex_failure_message(code: i32, model: &str) -> String {
    let model = model.trim();
    if model.is_empty() {
        format!(
            "Codex exited with status {code} — run `codex login status` in Terminal and confirm a normal `codex exec` request works. stderr suppressed because it may contain meeting content"
        )
    } else {
        format!(
            "Codex exited with status {code} — selected model `{model}` may not be available for this Codex account. Choose another Codex model in Settings or verify it with `codex exec -m {model}`. stderr suppressed because it may contain meeting content"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    fn args(cmd: &Command) -> Vec<String> {
        cmd.as_std()
            .get_args()
            .map(OsStr::to_string_lossy)
            .map(|arg| arg.into_owned())
            .collect()
    }

    fn emit_runner_marker(marker: &str) {
        // A child that inherits fd 2 bypasses libtest's per-test capture, so the bounded Harness
        // stderr evidence always contains this marker even when the full stdout listing is clipped.
        let status = std::process::Command::new("/bin/sh")
            .args([
                "-c",
                "exec /usr/bin/printf '%s\n' \"$1\" >&2",
                "murmur-runner-marker",
                marker,
            ])
            .status()
            .expect("runner marker command must start");
        assert!(status.success(), "runner marker command must succeed");
    }

    fn installed_codex_for_runtime_proof(skip_marker: &str) -> Option<String> {
        // Harness deliberately replaces HOME so checks cannot read ambient user config. Its
        // runner-bound RUSTUP_HOME still identifies the real account home, and the sandbox grants
        // read/execute only to the trusted tool directories beneath it. Resolve the binary from
        // that home so the runtime proof exercises the same default candidate as the desktop app,
        // while CODEX_HOME and every auth/config write remain isolated by the individual test.
        let resolved = if std::env::var("MURMUR_HARNESS").as_deref() == Ok("1") {
            std::env::var_os("RUSTUP_HOME")
                .map(PathBuf::from)
                .filter(|path| path.file_name() == Some(OsStr::new(".rustup")))
                .and_then(|path| path.parent().map(Path::to_path_buf))
                .ok_or_else(|| {
                    AppError::Unavailable(
                        "Harness did not bind the real user home for the Codex runtime proof"
                            .into(),
                    )
                })
                .and_then(|home| resolve_codex_binary_from(DEFAULT_BINARY, Some(&home), None))
        } else {
            resolve_codex_binary(DEFAULT_BINARY)
        };
        match resolved {
            Ok(binary) => Some(binary),
            Err(error) if std::env::var("MURMUR_HARNESS").as_deref() == Ok("1") => {
                panic!(
                    "Harness requires the installed Codex CLI runtime proofs to execute: {error}"
                )
            }
            Err(_) => {
                emit_runner_marker(skip_marker);
                None
            }
        }
    }

    struct LocalResponsesServer {
        base_url: String,
        requests: Arc<Mutex<Vec<Value>>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    const INTEGRATED_PRIVATE_NAME: &str = "MurmurAlicePrivacySentinel";
    const INTEGRATED_PRIVATE_EMAIL: &str = "murmur-alice-privacy@example.test";

    struct IntegratedNameRedactor;

    impl crate::summarize::redact::NameRedactor for IntegratedNameRedactor {
        fn redact_names(&self, text: &str) -> (String, Vec<(String, String)>) {
            let redacted = text.replace(INTEGRATED_PRIVATE_NAME, "⟪NAME_1⟫");
            let pairs = if redacted != text {
                vec![("⟪NAME_1⟫".to_string(), INTEGRATED_PRIVATE_NAME.to_string())]
            } else {
                Vec::new()
            };
            (redacted, pairs)
        }
    }

    #[derive(Default)]
    struct IntegratedEgressSink {
        entries: Mutex<Vec<crate::summarize::egress_log::EgressEntry>>,
    }

    impl crate::summarize::egress_log::EgressSink for IntegratedEgressSink {
        fn record(&self, entry: crate::summarize::egress_log::EgressEntry) {
            self.entries.lock().unwrap().push(entry);
        }
    }

    impl LocalResponsesServer {
        fn start(blocked_command: String) -> Self {
            Self::start_command(blocked_command, "MURMUR_RUNTIME_HOOK_BLOCKED")
        }

        fn start_command(blocked_command: String, final_text: &str) -> Self {
            let call_arguments = serde_json::to_string(&serde_json::json!({
                "command": blocked_command,
            }))
            .unwrap();
            Self::start_with_responses(vec![
                local_sse(vec![
                    serde_json::json!({
                        "type": "response.created",
                        "response": {"id": "murmur-runtime-response-1"},
                    }),
                    serde_json::json!({
                        "type": "response.output_item.done",
                        "item": {
                            "type": "function_call",
                            "call_id": "murmur-runtime-shell-call",
                            "name": "shell_command",
                            "arguments": call_arguments,
                        },
                    }),
                    local_completed_event("murmur-runtime-response-1"),
                ]),
                local_sse(vec![
                    serde_json::json!({
                        "type": "response.created",
                        "response": {"id": "murmur-runtime-response-2"},
                    }),
                    serde_json::json!({
                        "type": "response.output_item.done",
                        "item": {
                            "type": "message",
                            "role": "assistant",
                            "id": "murmur-runtime-message",
                            "content": [{
                                "type": "output_text",
                                "text": final_text,
                            }],
                        },
                    }),
                    local_completed_event("murmur-runtime-response-2"),
                ]),
            ])
        }

        fn start_note(note: String) -> Self {
            Self::start_with_responses(vec![local_sse(vec![
                serde_json::json!({
                    "type": "response.created",
                    "response": {"id": "murmur-note-response"},
                }),
                serde_json::json!({
                    "type": "response.output_item.done",
                    "item": {
                        "type": "message",
                        "role": "assistant",
                        "id": "murmur-note-message",
                        "content": [{
                            "type": "output_text",
                            "text": note,
                        }],
                    },
                }),
                local_completed_event("murmur-note-response"),
            ])])
        }

        fn start_with_responses(responses: Vec<String>) -> Self {
            let listener =
                TcpListener::bind(("127.0.0.1", 0)).expect("local Responses server must bind");
            listener
                .set_nonblocking(true)
                .expect("local Responses listener must become nonblocking");
            let address = listener
                .local_addr()
                .expect("local Responses server address");
            let requests = Arc::new(Mutex::new(Vec::new()));
            let captured = Arc::clone(&requests);
            let thread = std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(30);
                let mut response_index = 0;
                while response_index < responses.len() && Instant::now() < deadline {
                    match listener.accept() {
                        Ok((mut stream, _)) => {
                            stream
                                .set_nonblocking(false)
                                .expect("accepted local Responses stream must become blocking");
                            let (path, request) =
                                read_local_http_json(&mut stream).expect("valid local API request");
                            if !path.ends_with("/responses") {
                                write_local_http_response(
                                    &mut stream,
                                    404,
                                    "application/json",
                                    "{}",
                                )
                                .expect("local 404 response");
                                continue;
                            }
                            captured.lock().unwrap().push(request);
                            write_local_http_response(
                                &mut stream,
                                200,
                                "text/event-stream",
                                &responses[response_index],
                            )
                            .expect("local SSE response");
                            response_index += 1;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(error) => panic!("local Responses server failed: {error}"),
                    }
                }
                assert_eq!(
                    response_index,
                    responses.len(),
                    "real Codex CLI did not complete every local Responses call"
                );
            });

            Self {
                base_url: format!("http://{address}/v1"),
                requests,
                thread: Some(thread),
            }
        }

        fn finish(mut self) -> Vec<Value> {
            self.thread
                .take()
                .expect("local Responses thread")
                .join()
                .expect("local Responses server must finish");
            Arc::try_unwrap(self.requests)
                .expect("all local Responses references must be released")
                .into_inner()
                .unwrap()
        }
    }

    fn local_completed_event(id: &str) -> Value {
        serde_json::json!({
            "type": "response.completed",
            "response": {
                "id": id,
                "usage": {
                    "input_tokens": 0,
                    "input_tokens_details": null,
                    "output_tokens": 0,
                    "output_tokens_details": null,
                    "total_tokens": 0,
                },
            },
        })
    }

    fn local_provider_config(base_url: &str) -> String {
        format!(
            r#"model_providers.murmur_runtime_test={{name="Murmur Runtime Test",base_url="{base_url}",wire_api="responses",experimental_bearer_token="synthetic-test-token",requires_openai_auth=false,supports_websockets=false,request_max_retries=0,stream_max_retries=0}}"#
        )
    }

    fn local_sse(events: Vec<Value>) -> String {
        use std::fmt::Write as _;

        let mut body = String::new();
        for event in events {
            let event_type = event["type"].as_str().expect("typed SSE event");
            writeln!(&mut body, "event: {event_type}").unwrap();
            writeln!(&mut body, "data: {event}\n").unwrap();
        }
        body
    }

    fn read_local_http_json(stream: &mut TcpStream) -> std::io::Result<(String, Value)> {
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 8192];
        let header_end = loop {
            let read = stream.read(&mut chunk)?;
            if read == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "local API request ended before headers",
                ));
            }
            bytes.extend_from_slice(&chunk[..read]);
            if bytes.len() > 2 * 1024 * 1024 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "local API request exceeded the test bound",
                ));
            }
            if let Some(offset) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break offset + 4;
            }
        };
        let headers = std::str::from_utf8(&bytes[..header_end]).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "local API headers were not UTF-8",
            )
        })?;
        let path = headers
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "local API request line was malformed",
                )
            })?
            .to_string();
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "local API request had no content length",
                )
            })?;
        while bytes.len() < header_end + content_length {
            let read = stream.read(&mut chunk)?;
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..read]);
        }
        if bytes.len() < header_end + content_length {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "local API request body was truncated",
            ));
        }
        let body = serde_json::from_slice(&bytes[header_end..header_end + content_length])
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "local API request body was not JSON",
                )
            })?;
        Ok((path, body))
    }

    fn write_local_http_response(
        stream: &mut TcpStream,
        status: u16,
        content_type: &str,
        body: &str,
    ) -> std::io::Result<()> {
        let reason = if status == 200 { "OK" } else { "Not Found" };
        let headers = format!(
            "HTTP/1.1 {status} {reason}\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
            body.len()
        );
        stream.write_all(headers.as_bytes())?;
        stream.write_all(body.as_bytes())?;
        stream.flush()
    }

    #[derive(Clone, Copy)]
    enum SchemaNetworkBoundary {
        InnerDenyAll,
        InheritedNonLoopbackDenied,
    }

    impl SchemaNetworkBoundary {
        fn uses_inner_sandbox(self) -> bool {
            matches!(self, Self::InnerDenyAll)
        }

        #[cfg(target_os = "macos")]
        fn auth_status_boundary(self) -> AuthStatusNetworkBoundary {
            match self {
                Self::InnerDenyAll => AuthStatusNetworkBoundary::MacosSeatbelt,
                Self::InheritedNonLoopbackDenied => AuthStatusNetworkBoundary::InheritedTestSandbox,
            }
        }

        fn marker(self) -> &'static str {
            match self {
                Self::InnerDenyAll => "inner_deny_all",
                Self::InheritedNonLoopbackDenied => "inherited_nonloopback_denied",
            }
        }

        fn runtime_marker(self) -> &'static str {
            match self {
                Self::InnerDenyAll => "inner_loopback_only",
                Self::InheritedNonLoopbackDenied => "inherited_nonloopback_denied",
            }
        }
    }

    fn schema_test_network_boundary() -> SchemaNetworkBoundary {
        let nesting_probe = std::process::Command::new("/usr/bin/sandbox-exec")
            .args([
                "-p",
                "(version 1)(allow default)",
                "/usr/bin/printf",
                "MURMUR_NESTED_SANDBOX_OK",
            ])
            .output()
            .expect("Seatbelt nesting probe must start");

        if nesting_probe.status.success() {
            assert_eq!(
                nesting_probe.stdout, b"MURMUR_NESTED_SANDBOX_OK",
                "successful Seatbelt nesting probe returned unexpected output"
            );
            return SchemaNetworkBoundary::InnerDenyAll;
        }

        let stderr = String::from_utf8_lossy(&nesting_probe.stderr);
        assert!(
            stderr.contains("sandbox_apply: Operation not permitted"),
            "inner Seatbelt failed without evidence of an inherited sandbox: {stderr}"
        );

        // RFC 5737 TEST-NET-1 is permanently reserved for documentation and cannot host a real
        // service. EPERM here proves the inherited runner profile blocks non-loopback egress
        // before this test permits the synthetic Codex process to run without a nested profile.
        let test_net = std::net::SocketAddr::from(([192, 0, 2, 1], 9));
        let network_error =
            std::net::TcpStream::connect_timeout(&test_net, std::time::Duration::from_millis(100))
                .expect_err("inherited sandbox unexpectedly allowed TEST-NET-1 egress");
        assert_eq!(
            network_error.kind(),
            std::io::ErrorKind::PermissionDenied,
            "nested Seatbelt is unavailable, but non-loopback egress was not denied"
        );

        SchemaNetworkBoundary::InheritedNonLoopbackDenied
    }

    #[test]
    fn command_is_ephemeral_tool_free_and_model_aware() {
        let cmd = build_codex_command(
            "codex",
            "gpt-5.6-terra",
            Path::new("/tmp/murmur-codex-test-home"),
        );
        let args = args(&cmd);
        for required in [
            "exec",
            "--ephemeral",
            "--ignore-user-config",
            "--strict-config",
            "--skip-git-repo-check",
            "--ignore-rules",
            "--dangerously-bypass-hook-trust",
            "--json",
            "gpt-5.6-terra",
        ] {
            assert!(args.iter().any(|arg| arg == required), "missing {required}");
        }
        for feature in NATIVE_TOOL_FEATURES {
            assert!(
                args.windows(2)
                    .any(|pair| pair[0] == "--disable" && pair[1] == *feature),
                "native tool feature must be disabled: {feature}"
            );
        }
        assert!(args.iter().any(|arg| arg == EMPTY_CWD));
        let expected_hook = format!("hooks.PreToolUse={}", tool_guard_config());
        assert!(
            args.iter().any(|arg| arg == &expected_hook),
            "production command must install the hook derived from TOOL_GUARD_COMMAND"
        );
        for closed in [
            APPROVAL_POLICY,
            WEB_SEARCH_DISABLED,
            APPS_DISABLED,
            PLUGINS_DISABLED,
            MULTI_AGENT_DISABLED,
            MCP_SERVERS_EMPTY,
            NETWORK_PROFILE,
            FILESYSTEM_PROFILE,
        ] {
            assert!(args.iter().any(|arg| arg == closed), "missing {closed}");
        }
        assert_eq!(cmd.as_std().get_current_dir(), Some(Path::new(EMPTY_CWD)));
    }

    #[test]
    fn empty_model_omits_model_flag() {
        let args = args(&build_codex_command(
            "codex",
            "  ",
            Path::new("/tmp/murmur-codex-test-home"),
        ));
        assert!(!args.iter().any(|arg| arg == "--model"));
    }

    /// A6 on the OTHER CLI arm, asserted on real argv. `--model` takes the next argv entry, so a
    /// value beginning with `-` would be read by `codex exec` as a flag.
    #[test]
    fn a_hostile_model_id_never_reaches_codex_argv() {
        let home = Path::new("/tmp/murmur-codex-hostile-home");
        for hostile in [
            "--sandbox danger-full-access",
            "-m",
            "codex --dangerously-bypass-approvals-and-sandbox",
            "../../etc/passwd",
        ] {
            let args = args(&build_codex_command("codex", hostile, home));
            assert!(
                !args.iter().any(|arg| arg == "--model"),
                "{hostile:?} must not produce a --model flag; got {args:?}"
            );
            assert!(
                !args.iter().any(|arg| arg.contains(hostile.trim())),
                "no fragment of {hostile:?} may survive into argv"
            );
        }
        // A legitimate unlisted id still reaches the CLI — the point of a hint catalog.
        let args = args(&build_codex_command("codex", "gpt-5.7-nova", home));
        assert!(args.iter().any(|arg| arg == "--model"));
        assert!(args.iter().any(|arg| arg == "gpt-5.7-nova"));
    }

    #[test]
    fn immutable_tool_guard_command_emits_a_deny_decision() {
        let codex_home = crate::storage::db::unique_temp_path("murmur-codex-tool-marker", "dir");
        create_private_directory(&codex_home).unwrap();
        let output = std::process::Command::new("/bin/sh")
            .args(["-c", TOOL_GUARD_COMMAND])
            .env("CODEX_HOME", &codex_home)
            .output()
            .expect("tool guard command must execute");
        assert!(output.status.success());
        let decision: Value = serde_json::from_slice(&output.stdout).unwrap();
        let hook = &decision["hookSpecificOutput"];
        assert_eq!(hook["hookEventName"], "PreToolUse");
        assert_eq!(hook["permissionDecision"], "deny");
        assert_eq!(
            hook["permissionDecisionReason"],
            "Murmur Codex provider has no local tools."
        );
        assert!(
            codex_home.join(TOOL_ATTEMPT_MARKER).is_file(),
            "every hook denial must leave a private provider-visible tripwire"
        );
        let _ = fs::remove_dir_all(codex_home);
        emit_runner_marker(
            "MURMUR_CODEX_TOOL_GUARD_EXECUTED production_config_derived=true decision=deny tripwire=true",
        );
    }

    #[test]
    fn tool_router_error_tripwire_is_exact_and_content_free() {
        assert!(stderr_reports_tool_activity(
            b"ERROR codex_core::tools::router: unsupported call: shell_command"
        ));
        assert!(stderr_reports_tool_activity(
            b"ERROR codex_core::tools::router: unsupported custom tool call: future_tool"
        ));
        assert!(!stderr_reports_tool_activity(
            b"Codex completed a tool-free text response"
        ));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn version_probe_cache_is_scoped_to_the_network_boundary() {
        use std::os::unix::fs::PermissionsExt;

        let root = crate::storage::db::unique_temp_path("murmur-codex-boundary-cache", "dir");
        fs::create_dir_all(&root).unwrap();
        let binary = root.join("codex");
        let counter = root.join("runs");
        fs::write(
            &binary,
            format!(
                "#!/bin/sh\n/usr/bin/printf x >> \"{}\"\n/usr/bin/printf 'codex-cli 0.146.0\\n'\n",
                counter.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let binary = binary.to_str().unwrap();

        let network_boundary = schema_test_network_boundary();
        probe_supported_codex_version_with_boundary(
            binary,
            VersionProbeNetworkBoundary::InheritedTestSandbox,
        )
        .await
        .unwrap();
        let production_result = probe_supported_codex_version_with_boundary(
            binary,
            VersionProbeNetworkBoundary::MacosSeatbelt,
        )
        .await;
        match network_boundary {
            SchemaNetworkBoundary::InnerDenyAll => {
                production_result.unwrap();
                assert_eq!(fs::read(&counter).unwrap(), b"xx");
            }
            SchemaNetworkBoundary::InheritedNonLoopbackDenied => {
                assert!(
                    production_result.is_err(),
                    "a cache entry from the inherited test boundary must not satisfy the production boundary"
                );
                assert_eq!(
                    fs::read(&counter).unwrap(),
                    b"x",
                    "nested Seatbelt rejection must happen before the production child executes"
                );
            }
        }
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn installed_default_runtime_pin_uses_the_production_bounded_probe_when_available() {
        let Some(binary) =
            installed_codex_for_runtime_proof("MURMUR_CODEX_RUNTIME_PIN_SKIPPED cli=absent")
        else {
            return;
        };
        let expected = fs::canonicalize(binary).unwrap();
        let network_boundary = schema_test_network_boundary();
        let result = resolve_pinned_default_runtime().await;
        match network_boundary {
            SchemaNetworkBoundary::InnerDenyAll => {
                let runtime = result.expect(
                    "installed default Codex must pass the production bounded version probe",
                );
                assert_eq!(runtime.path, expected);
                validate_supported_codex_version(&runtime.version).unwrap();
                runtime.assert_unchanged().unwrap();
                emit_runner_marker(
                    "MURMUR_CODEX_RUNTIME_PIN_EXECUTED supported_0_146_x=true bounded=true identity=true network_boundary=inner_deny_all",
                );
            }
            SchemaNetworkBoundary::InheritedNonLoopbackDenied => {
                assert!(
                    result.is_err(),
                    "production runtime pin must fail closed when deny-all Seatbelt cannot be nested"
                );
                emit_runner_marker(
                    "MURMUR_CODEX_RUNTIME_PIN_EXECUTED child_started=false bounded=false identity=false fail_closed=true network_boundary=inherited_outer_plus_inner_rejected",
                );
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn installed_codex_cli_accepts_the_exact_production_schema_when_available() {
        let Some(bin) = installed_codex_for_runtime_proof("MURMUR_CODEX_SCHEMA_SKIPPED cli=absent")
        else {
            return;
        };
        let version_output = std::process::Command::new(&bin)
            .arg("--version")
            .output()
            .expect("installed Codex version probe must start");
        assert!(version_output.status.success());
        let version = String::from_utf8(version_output.stdout)
            .expect("Codex version must be UTF-8")
            .trim()
            .to_string();
        assert!(
            version.starts_with("codex-cli "),
            "unexpected installed Codex version output"
        );
        validate_supported_codex_version(&version)
            .expect("runner Codex must be inside the production-supported isolation range");
        let source_home = crate::storage::db::unique_temp_path("murmur-codex-schema-source", "dir");
        create_private_directory(&source_home).unwrap();
        let mut runtime = CodexRuntimeHome::prepare_from(Some(&source_home)).unwrap();
        // A regular checkout adds its own deny-all-network wrapper. Harness checks already inherit
        // a hash-bound Seatbelt profile, and Seatbelt cannot nest. Detect that state from the
        // kernel-enforced behavior itself and prove non-loopback egress returns EPERM before using
        // the inherited boundary.
        let network_boundary = schema_test_network_boundary();
        let mut command = build_schema_probe_command(
            &bin,
            "",
            runtime.path(),
            network_boundary.uses_inner_sandbox(),
        );
        // Do not add or remove any environment entry here: this is the exact production child
        // environment produced by `harden_env(false)` plus the isolated CODEX_HOME.
        let child = command
            .spawn()
            .expect("installed Codex schema probe failed to spawn");
        let output = run_external_cli_child(
            child,
            b"Return only the word MURMUR_SCHEMA_PROBE.",
            "Codex CLI schema probe",
        )
        .await
        .expect("installed Codex schema probe process failed");
        runtime.finalize().unwrap();
        let stdout = String::from_utf8(output.stdout).expect("Codex JSONL must be UTF-8");
        assert!(
            stdout.lines().any(|line| {
                serde_json::from_str::<Value>(line)
                    .ok()
                    .and_then(|event| event.get("type").and_then(Value::as_str).map(str::to_owned))
                    .as_deref()
                    == Some("thread.started")
            }),
            "the real Codex process must start a thread under the exact production argv"
        );
        let diagnostics =
            format!("{stdout}\n{}", String::from_utf8_lossy(&output.stderr)).to_ascii_lowercase();
        assert!(
            !diagnostics.trim().is_empty(),
            "the installed CLI must execute far enough to emit a diagnostic"
        );
        for schema_error in [
            "unexpected argument",
            "unrecognized option",
            "unknown configuration",
            "unknown config key",
            "error loading configuration",
            "failed to parse config",
            "invalid configuration",
        ] {
            assert!(
                !diagnostics.contains(schema_error),
                "Codex rejected Murmur's production isolation schema: {diagnostics}"
            );
        }
        emit_runner_marker(&format!(
            "MURMUR_CODEX_SCHEMA_EXECUTED version={version} strict_profile_accepted=true production_env=true thread_started=true network_boundary={}",
            network_boundary.marker()
        ));
        drop(runtime);
        let _ = fs::remove_dir_all(source_home);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn installed_codex_cli_honors_the_production_tool_and_capability_boundary() {
        let Some(bin) =
            installed_codex_for_runtime_proof("MURMUR_CODEX_RUNTIME_ISOLATION_SKIPPED cli=absent")
        else {
            return;
        };
        let source_home =
            crate::storage::db::unique_temp_path("murmur-codex-runtime-source", "dir");
        create_private_directory(&source_home).unwrap();
        let synthetic_auth = source_home.join("auth.json");
        fs::write(
            &synthetic_auth,
            b"{\"OPENAI_API_KEY\":\"murmur-synthetic-loopback-only\"}",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&synthetic_auth, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let side_effect =
            crate::storage::db::unique_temp_path("murmur-codex-forbidden-side-effect", "marker");
        let _ = fs::remove_file(&side_effect);
        let blocked_command = format!("/usr/bin/touch '{}'", side_effect.display());
        let server = LocalResponsesServer::start(blocked_command);
        let provider_config = local_provider_config(&server.base_url);
        let network_boundary = schema_test_network_boundary();
        let provider = CodexCliProvider::with_binary(bin)
            .with_model("gpt-5.6-terra".into())
            .with_runtime_probe(
                provider_config,
                source_home.clone(),
                network_boundary.uses_inner_sandbox(),
            );
        let error = provider
            .run_prompt("Attempt the shell command requested by the local test response.")
            .await
            .expect_err("any native activity attempt must reject the complete Codex operation");
        assert!(
            error
                .to_string()
                .contains("Codex attempted disabled native activity"),
            "real Codex CLI activity was not rejected fail-closed: {error}"
        );
        let requests = server.finish();
        assert_eq!(
            requests.len(),
            2,
            "the runtime probe must make exactly one tool request and one follow-up request"
        );
        let declared_tools = requests[0]
            .get("tools")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        assert!(
            declared_tools.is_empty(),
            "production Codex must advertise zero native tools: {declared_tools:?}"
        );
        let tool_output = serde_json::to_string(&requests[1]["input"]).unwrap();
        assert!(
            tool_output.contains(r#""type":"function_call""#)
                && tool_output.contains("unsupported call: shell_command"),
            "the real CLI did not retain detectable evidence of the rejected native call: {tool_output}"
        );
        assert!(
            !side_effect.exists(),
            "the command denied by Murmur's production hook must not execute"
        );

        // Codex omits `tools` entirely when the effective surface is empty; treat omission and an
        // explicit empty array identically, while still inspecting every advertised identity.
        let tool_identities: Vec<String> = declared_tools
            .iter()
            .map(|tool| {
                format!(
                    "{}:{}:{}",
                    tool.get("type").and_then(Value::as_str).unwrap_or(""),
                    tool.get("namespace").and_then(Value::as_str).unwrap_or(""),
                    tool.get("name").and_then(Value::as_str).unwrap_or("")
                )
                .to_ascii_lowercase()
            })
            .collect();
        for forbidden in [
            "web_search",
            "mcp__",
            "list_mcp_resources",
            "read_mcp_resource",
            "tool_search",
            "request_plugin_install",
            "spawn_agent",
            "send_message",
            "wait_agent",
        ] {
            assert!(
                tool_identities
                    .iter()
                    .all(|identity| !identity.contains(forbidden)),
                "disabled capability `{forbidden}` reached the real Codex tool surface: {tool_identities:?}"
            );
        }
        emit_runner_marker(&format!(
            "MURMUR_CODEX_RUNTIME_ISOLATION_EXECUTED production_env=true operation_rejected=true activity_detected=true side_effect=false tools_advertised=0 web_mcp_plugins_multi_agent_absent=true network_boundary={}",
            network_boundary.runtime_marker()
        ));

        let _ = fs::remove_file(side_effect);
        let _ = fs::remove_dir_all(source_home);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn installed_codex_cli_enforces_the_production_permission_profile_without_the_hook() {
        let Some(bin) =
            installed_codex_for_runtime_proof("MURMUR_CODEX_PERMISSION_PROFILE_SKIPPED cli=absent")
        else {
            return;
        };
        let source_home =
            crate::storage::db::unique_temp_path("murmur-codex-permission-source", "dir");
        create_private_directory(&source_home).unwrap();
        let side_effect =
            crate::storage::db::unique_temp_path("murmur-codex-permission-side-effect", "marker");
        let _ = fs::remove_file(&side_effect);
        let network_listener =
            TcpListener::bind(("127.0.0.1", 0)).expect("permission probe listener must bind");
        network_listener
            .set_nonblocking(true)
            .expect("permission probe listener must be nonblocking");
        let network_address = network_listener.local_addr().unwrap();
        let blocked_command = format!(
            "/usr/bin/touch '{}'; /usr/bin/nc -z -w 1 {} {}",
            side_effect.display(),
            network_address.ip(),
            network_address.port()
        );
        let server = LocalResponsesServer::start_command(
            blocked_command,
            "MURMUR_RUNTIME_PERMISSION_PROFILE_DENIED",
        );
        let provider_config = local_provider_config(&server.base_url);
        let network_boundary = schema_test_network_boundary();
        let mut runtime = CodexRuntimeHome::prepare_from(Some(&source_home)).unwrap();
        let mut command = build_local_permission_probe_command(
            &bin,
            "gpt-5.6-terra",
            runtime.path(),
            provider_config,
            network_boundary.uses_inner_sandbox(),
        );
        let child = command
            .spawn()
            .expect("installed Codex permission probe failed to spawn");
        let output = run_external_cli_child(
            child,
            b"Attempt the shell command requested by the local test response.",
            "Codex CLI permission profile probe",
        )
        .await
        .expect("installed Codex permission probe failed");
        runtime.finalize().unwrap();
        let requests = server.finish();
        let diagnostics = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.status.success(),
            "real Codex CLI did not complete the permission probe: {diagnostics}"
        );
        assert_eq!(requests.len(), 2);
        let tool_output = serde_json::to_string(&requests[1]["input"]).unwrap();
        assert!(
            tool_output.to_ascii_lowercase().contains("denied")
                || tool_output
                    .to_ascii_lowercase()
                    .contains("operation not permitted"),
            "the real CLI did not report its production permission denial: {tool_output}"
        );
        assert!(
            !side_effect.exists(),
            "the production filesystem profile must deny the synthetic write without relying on the hook"
        );
        assert!(
            matches!(
                network_listener.accept(),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
            ),
            "the production network profile must deny the synthetic loopback connection without relying on the hook"
        );
        emit_runner_marker(&format!(
            "MURMUR_CODEX_PERMISSION_PROFILE_EXECUTED hook_installed=false filesystem_side_effect=false loopback_tool_connection=false network_boundary={}",
            network_boundary.runtime_marker()
        ));

        drop(runtime);
        let _ = fs::remove_file(side_effect);
        let _ = fs::remove_dir_all(source_home);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn installed_codex_cli_summarize_round_trips_a_normalized_note() {
        let Some(bin) =
            installed_codex_for_runtime_proof("MURMUR_CODEX_NOTE_ROUNDTRIP_SKIPPED cli=absent")
        else {
            return;
        };
        let expected = concat!(
            "---\n",
            "title: Runtime proof\n",
            "date: 2026-07-29\n",
            "---\n",
            "# Runtime proof\n\n",
            "- Codex note pipeline completed."
        );
        let server = LocalResponsesServer::start_note(format!("```markdown\n{expected}\n```"));
        let source_home = crate::storage::db::unique_temp_path("murmur-codex-note-source", "dir");
        create_private_directory(&source_home).unwrap();
        let synthetic_auth = source_home.join("auth.json");
        fs::write(
            &synthetic_auth,
            b"{\"OPENAI_API_KEY\":\"murmur-synthetic-loopback-only\"}",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&synthetic_auth, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let network_boundary = schema_test_network_boundary();
        let provider = CodexCliProvider::with_binary(bin)
            .with_model("gpt-5.6-terra".into())
            .with_runtime_probe(
                local_provider_config(&server.base_url),
                source_home.clone(),
                network_boundary.uses_inner_sandbox(),
            );
        let request = SummarizeRequest {
            transcript: "MURMUR_SYNTHETIC_TRANSCRIPT_FOR_NOTE_ROUNDTRIP".into(),
            meta: crate::summarize::provider::MeetingMeta {
                date_iso: "2026-07-29".into(),
                title_hint: Some("Runtime proof".into()),
                duration_s: 60,
                language: Some("en".into()),
            },
            template: "Return an Obsidian note with YAML front matter.".into(),
            vault_titles: vec![],
            related_context: None,
            user_notes: None,
            live_bullets: None,
            glossary: None,
        };

        let note = provider
            .summarize(&request)
            .await
            .expect("real Codex JSONL must pass through provider parsing and note normalization");
        let requests = server.finish();
        assert_eq!(note, expected);
        assert_eq!(requests.len(), 1);
        assert_eq!(
            fs::read(&synthetic_auth).unwrap(),
            b"{\"OPENAI_API_KEY\":\"murmur-synthetic-loopback-only\"}",
            "the real CLI round-trip must finalize without losing or corrupting isolated auth"
        );
        assert!(
            serde_json::to_string(&requests[0]["input"])
                .unwrap()
                .contains("MURMUR_SYNTHETIC_TRANSCRIPT_FOR_NOTE_ROUNDTRIP"),
            "the real CLI request must contain the synthetic stdin-rendered meeting payload"
        );
        emit_runner_marker(&format!(
            "MURMUR_CODEX_NOTE_ROUNDTRIP_EXECUTED production_provider=true isolated_auth_present=true auth_sync_finalized=true jsonl_parsed=true markdown_normalized=true network_boundary={}",
            network_boundary.runtime_marker()
        ));
        let _ = fs::remove_dir_all(source_home);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn installed_codex_cli_runs_through_the_production_redaction_and_ledger_seam() {
        let Some(bin) =
            installed_codex_for_runtime_proof("MURMUR_CODEX_EGRESS_SEAM_SKIPPED cli=absent")
        else {
            return;
        };
        let server =
            LocalResponsesServer::start_note("Summary for ⟪NAME_1⟫ at ⟪EMAIL_1⟫.".to_string());
        let source_home = crate::storage::db::unique_temp_path("murmur-codex-egress-source", "dir");
        create_private_directory(&source_home).unwrap();
        let synthetic_auth = source_home.join("auth.json");
        fs::write(
            &synthetic_auth,
            b"{\"OPENAI_API_KEY\":\"murmur-synthetic-loopback-only\"}",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&synthetic_auth, fs::Permissions::from_mode(0o600)).unwrap();
        }

        let network_boundary = schema_test_network_boundary();
        let raw_codex: Arc<dyn SummarizerProvider> = Arc::new(
            CodexCliProvider::with_binary(bin)
                .with_model("gpt-5.6-terra".into())
                .with_runtime_probe(
                    local_provider_config(&server.base_url),
                    source_home.clone(),
                    network_boundary.uses_inner_sandbox(),
                ),
        );
        let sink = Arc::new(IntegratedEgressSink::default());
        let config = crate::settings::AppConfig {
            cloud_egress_consented: true,
            ..crate::settings::AppConfig::default()
        };
        let target = crate::summarize::roles::RoleTarget {
            connection: crate::summarize::PROVIDER_CODEX_CLI.to_string(),
            model: "gpt-5.6-terra".to_string(),
            effort: String::new(),
        };
        let provider = super::super::make_provider_resolved(
            &target,
            &config,
            &Arc::new(tokio::sync::Semaphore::new(1)),
            None,
            Some(super::super::ProviderTestOverrides {
                codex_inner: raw_codex,
                names: Arc::new(IntegratedNameRedactor),
                sink: sink.clone(),
            }),
        )
        .expect("production resolved-provider seam must construct Codex with consent");

        let output = provider
            .complete(
                &format!("Summarize for {INTEGRATED_PRIVATE_NAME}."),
                &format!("{INTEGRATED_PRIVATE_NAME} can be reached at {INTEGRATED_PRIVATE_EMAIL}."),
            )
            .await
            .expect("redacted payload must complete through the real installed Codex CLI");
        let requests = server.finish();
        assert_eq!(
            output,
            format!("Summary for {INTEGRATED_PRIVATE_NAME} at {INTEGRATED_PRIVATE_EMAIL}."),
            "the production wrapper must restore both name and regex placeholders"
        );
        assert_eq!(requests.len(), 1);
        let dispatched = serde_json::to_string(&requests[0]["input"]).unwrap();
        assert!(!dispatched.contains(INTEGRATED_PRIVATE_NAME));
        assert!(!dispatched.contains(INTEGRATED_PRIVATE_EMAIL));
        assert!(dispatched.contains("⟪NAME_1⟫"));
        assert!(dispatched.contains("⟪EMAIL_1⟫"));
        let declared_tools = requests[0]
            .get("tools")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        assert!(
            declared_tools.is_empty(),
            "integrated production request must advertise zero tools"
        );

        let entries = sink.entries.lock().unwrap();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.provider_id, crate::summarize::PROVIDER_CODEX_CLI);
        assert_eq!(
            entry.destination,
            crate::summarize::CODEX_EGRESS_DESTINATION
        );
        assert_eq!(entry.model_requested, "gpt-5.6-terra");
        assert_eq!(entry.call_kind, "complete");
        assert_eq!(entry.redactions.name, 1);
        assert_eq!(entry.redactions.email, 1);
        assert_eq!(entry.system_bytes, 0);
        assert!(entry.user_bytes > 0);
        let ledger_debug = format!("{entry:?}");
        assert!(!ledger_debug.contains(INTEGRATED_PRIVATE_NAME));
        assert!(!ledger_debug.contains(INTEGRATED_PRIVATE_EMAIL));
        drop(entries);

        emit_runner_marker(&format!(
            "MURMUR_CODEX_EGRESS_SEAM_EXECUTED production_factory=true real_cli=true consent=true name_redacted=true email_redacted=true restored=true ledger_content_free=true tools_advertised=0 network_boundary={}",
            network_boundary.runtime_marker()
        ));
        let _ = fs::remove_dir_all(source_home);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn installed_codex_cli_complete_round_trips_redaction_tokens() {
        let Some(bin) =
            installed_codex_for_runtime_proof("MURMUR_CODEX_COMPLETE_SKIPPED cli=absent")
        else {
            return;
        };
        let server =
            LocalResponsesServer::start_note("Summary for ⟪NAME_1⟫ at ⟪EMAIL_1⟫.".to_string());
        let source_home =
            crate::storage::db::unique_temp_path("murmur-codex-complete-source", "dir");
        create_private_directory(&source_home).unwrap();
        let synthetic_auth = source_home.join("auth.json");
        fs::write(
            &synthetic_auth,
            b"{\"OPENAI_API_KEY\":\"murmur-synthetic-loopback-only\"}",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&synthetic_auth, fs::Permissions::from_mode(0o600)).unwrap();
        }
        let network_boundary = schema_test_network_boundary();
        let provider = CodexCliProvider::with_binary(bin)
            .with_model("gpt-5.6-terra".into())
            .with_runtime_probe(
                local_provider_config(&server.base_url),
                source_home.clone(),
                network_boundary.uses_inner_sandbox(),
            );
        let output = provider
            .complete(
                "Summarize for ⟪NAME_1⟫.",
                "⟪NAME_1⟫ can be reached at ⟪EMAIL_1⟫.",
            )
            .await
            .expect("real Codex completion must preserve redaction tokens");
        let requests = server.finish();
        assert_eq!(output, "Summary for ⟪NAME_1⟫ at ⟪EMAIL_1⟫.");
        assert_eq!(requests.len(), 1);
        let _ = fs::remove_dir_all(source_home);
    }

    #[test]
    fn generation_uses_only_the_isolated_codex_home() {
        let isolated_home = Path::new("/tmp/murmur-codex-test-home");
        let generation = build_codex_command("codex", "", isolated_home);

        let generation_env: std::collections::HashMap<_, _> =
            generation.as_std().get_envs().collect();
        assert_eq!(
            generation_env
                .get(OsStr::new("CODEX_HOME"))
                .and_then(|value| *value),
            Some(isolated_home.as_os_str())
        );
        assert_eq!(
            generation.as_std().get_current_dir(),
            Some(Path::new(EMPTY_CWD))
        );
    }

    #[test]
    fn meeting_content_is_stdin_only_never_argv_or_environment() {
        const SENTINEL: &str = "MURMUR_PRIVATE_MEETING_SENTINEL_7F42";
        let request = SummarizeRequest {
            transcript: SENTINEL.into(),
            meta: crate::summarize::provider::MeetingMeta {
                date_iso: "2026-07-29".into(),
                title_hint: Some(SENTINEL.into()),
                duration_s: 42,
                language: Some("pl".into()),
            },
            template: format!("Summarize {SENTINEL}"),
            vault_titles: vec![SENTINEL.into()],
            related_context: Some(SENTINEL.into()),
            user_notes: Some(SENTINEL.into()),
            live_bullets: Some(SENTINEL.into()),
            glossary: Some(SENTINEL.into()),
        };
        let stdin = render_summarize_stdin(&request);
        assert!(
            stdin.contains(SENTINEL),
            "the synthetic meeting payload must reach the stdin renderer"
        );

        let command = build_codex_command(
            "codex",
            "gpt-5.6-terra",
            Path::new("/tmp/murmur-codex-test-home"),
        );
        let argv = command
            .as_std()
            .get_args()
            .map(OsStr::to_string_lossy)
            .collect::<Vec<_>>()
            .join("\n");
        let environment = command
            .as_std()
            .get_envs()
            .filter_map(|(name, value)| {
                value.map(|value| format!("{}={}", name.to_string_lossy(), value.to_string_lossy()))
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            !argv.contains(SENTINEL),
            "meeting content must never enter Codex argv"
        );
        assert!(
            !environment.contains(SENTINEL),
            "meeting content must never enter the Codex environment"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn production_version_probe_command_unconditionally_installs_deny_network() {
        let command = codex_version_probe_command(
            "/opt/homebrew/bin/codex",
            VersionProbeNetworkBoundary::MacosSeatbelt,
        );
        assert_eq!(
            command.as_std().get_program(),
            OsStr::new("/usr/bin/sandbox-exec")
        );
        let args = args(&command);
        assert!(args
            .iter()
            .any(|arg| arg == "(version 1)(allow default)(deny network*)"));
        assert!(args.iter().any(|arg| arg == "/opt/homebrew/bin/codex"));
        assert!(args.iter().any(|arg| arg == "--version"));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn production_version_probe_denies_loopback_and_nonloopback_or_fails_before_exec() {
        use std::os::unix::fs::PermissionsExt;

        let root =
            crate::storage::db::unique_temp_path("murmur-codex-version-network-boundary", "dir");
        fs::create_dir_all(&root).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").expect("loopback proof listener");
        let loopback_port = listener.local_addr().unwrap().port();
        let attempted_loopback = root.join("loopback-attempted");
        let reached_loopback = root.join("loopback-reached");
        let attempted_nonloopback = root.join("nonloopback-attempted");
        let reached_nonloopback = root.join("nonloopback-reached");
        let fake_codex = root.join("codex");
        fs::write(
            &fake_codex,
            format!(
                "#!/bin/sh\n\
                 /usr/bin/printf attempted > '{}'\n\
                 if /usr/bin/nc -z -w 1 127.0.0.1 {} >/dev/null 2>&1; then /usr/bin/printf reached > '{}'; exit 93; fi\n\
                 /usr/bin/printf attempted > '{}'\n\
                 if /usr/bin/nc -z -w 1 192.0.2.1 9 >/dev/null 2>&1; then /usr/bin/printf reached > '{}'; exit 94; fi\n\
                 /usr/bin/printf 'codex-cli 0.146.0\\n'\n",
                attempted_loopback.display(),
                loopback_port,
                reached_loopback.display(),
                attempted_nonloopback.display(),
                reached_nonloopback.display(),
            ),
        )
        .unwrap();
        fs::set_permissions(&fake_codex, fs::Permissions::from_mode(0o700)).unwrap();

        let network_boundary = schema_test_network_boundary();
        let result = verify_supported_codex_cli_with_boundary(
            &fake_codex.to_string_lossy(),
            VersionProbeNetworkBoundary::MacosSeatbelt,
        )
        .await;
        match network_boundary {
            SchemaNetworkBoundary::InnerDenyAll => {
                result.expect(
                    "the supported version must remain readable behind the deny-all-network boundary",
                );
                assert!(attempted_loopback.exists());
                assert!(!reached_loopback.exists());
                assert!(attempted_nonloopback.exists());
                assert!(!reached_nonloopback.exists());
                emit_runner_marker(
                    "MURMUR_CODEX_VERSION_NETWORK_BOUNDARY_EXECUTED production_command_constructed=true attempted_loopback=true loopback_reached=false attempted_nonloopback=true nonloopback_reached=false supported_version_accepted=true process_group_reaped=true network_boundary=inner_deny_all",
                );
            }
            SchemaNetworkBoundary::InheritedNonLoopbackDenied => {
                assert!(
                    result.is_err(),
                    "when Seatbelt nesting is unavailable, the production probe must fail closed"
                );
                assert!(!attempted_loopback.exists());
                assert!(!reached_loopback.exists());
                assert!(!attempted_nonloopback.exists());
                assert!(!reached_nonloopback.exists());
                emit_runner_marker(
                    "MURMUR_CODEX_VERSION_NETWORK_BOUNDARY_EXECUTED production_command_constructed=true child_started=false loopback_reached=false nonloopback_reached=false supported_version_accepted=false fail_closed=true process_group_reaped=true network_boundary=inherited_outer_plus_inner_rejected",
                );
            }
        }

        drop(listener);
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn production_auth_status_command_unconditionally_installs_deny_network() {
        let command = codex_auth_status_command(
            "/opt/homebrew/bin/codex",
            Path::new("/tmp/murmur-codex-auth-status-home"),
            AuthStatusNetworkBoundary::MacosSeatbelt,
        );
        assert_eq!(
            command.as_std().get_program(),
            OsStr::new("/usr/bin/sandbox-exec")
        );
        let args = args(&command);
        assert!(args
            .iter()
            .any(|arg| arg == "(version 1)(allow default)(deny network*)"));
        assert!(args.iter().any(|arg| arg == "/opt/homebrew/bin/codex"));
        assert!(args.windows(2).any(|pair| pair == ["login", "status"]));
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn auth_status_readiness_denies_nonloopback_before_accepting_local_state() {
        use std::os::unix::fs::PermissionsExt;

        let root =
            crate::storage::db::unique_temp_path("murmur-codex-auth-network-boundary", "dir");
        let auth_home = root.join("auth");
        fs::create_dir_all(&auth_home).unwrap();
        let attempted = root.join("network-attempted");
        let reached = root.join("network-reached");
        let fake_codex = root.join("codex");
        fs::write(
            &fake_codex,
            format!(
                "#!/bin/sh\nif [ \"$#\" -eq 2 ] && [ \"$1\" = \"login\" ] && [ \"$2\" = \"status\" ]; then\n  /usr/bin/printf attempted > '{}'\n  if /usr/bin/nc -z -w 1 192.0.2.1 9 >/dev/null 2>&1; then /usr/bin/printf reached > '{}'; exit 93; fi\n  /usr/bin/grep -q '\"account\":\"valid\"' \"$CODEX_HOME/auth.json\" || exit 1\n  /usr/bin/printf 'Logged in using ChatGPT\\n'\n  exit 0\nfi\nexit 91\n",
                attempted.display(),
                reached.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&fake_codex, fs::Permissions::from_mode(0o700)).unwrap();
        let auth = auth_home.join("auth.json");
        fs::write(&auth, br#"{"account":"valid"}"#).unwrap();
        fs::set_permissions(&auth, fs::Permissions::from_mode(0o600)).unwrap();

        let network_boundary = schema_test_network_boundary();
        verify_codex_auth_status_with_boundary(
            &fake_codex.to_string_lossy(),
            Some(&auth_home),
            network_boundary.auth_status_boundary(),
        )
        .await
        .expect("local authenticated state must remain readable behind the deny-network boundary");
        assert!(
            attempted.exists(),
            "the fixture must attempt non-loopback access"
        );
        assert!(
            !reached.exists(),
            "the authentication readiness command must never reach a non-loopback endpoint"
        );
        assert_eq!(fs::read(&auth).unwrap(), br#"{"account":"valid"}"#);
        emit_runner_marker(&format!(
            "MURMUR_CODEX_AUTH_STATUS_NETWORK_BOUNDARY_EXECUTED production_command_constructed=true attempted_nonloopback=true reached=false local_status_accepted=true process_group_reaped=true network_boundary={}",
            network_boundary.marker()
        ));
        let _ = fs::remove_dir_all(root);
    }

    /// Two different executables must each keep their OWN verified entry.
    ///
    /// The cache used to be a single `Option`, so a store for binary B threw away the entry for
    /// binary A. With one real Codex that is invisible, but `cargo test --lib` runs the whole suite
    /// in ONE process with many threads, each test pointing at its own fake `codex` — so tests
    /// silently evicted each other and a caller re-ran a probe it had already paid for.
    ///
    /// This test is deliberately DIRECT rather than a full-suite reproduction. The original symptom
    /// (`availability_runs_only_the_local_version_probe_…` finding `vava` instead of `vaa`) needs a
    /// specific interleaving and does not reproduce on demand, so it is a coin flip as an oracle.
    /// Driving the eviction by hand is deterministic: on the old single-slot cache the final probe
    /// below is a MISS and the assertion fails.
    #[tokio::test]
    async fn a_verified_binary_survives_another_binary_being_verified() {
        use std::os::unix::fs::PermissionsExt;

        // A fake `codex` that appends one byte per --version call, so "did it probe again?" is
        // answered by the file rather than by timing.
        fn fake_codex(tag: &str) -> (PathBuf, PathBuf) {
            let root = crate::storage::db::unique_temp_path(
                &format!("murmur-codex-cache-{tag}"),
                "dir",
            );
            fs::create_dir_all(&root).unwrap();
            let marker = root.join("probes");
            let bin = root.join("codex");
            fs::write(
                &bin,
                format!(
                    "#!/bin/sh\n/usr/bin/printf v >> '{}'\n/usr/bin/printf 'codex-cli 0.146.7\\n'\nexit 0\n",
                    marker.display()
                ),
            )
            .unwrap();
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o700)).unwrap();
            (bin, marker)
        }

        let (bin_a, marker_a) = fake_codex("a");
        let (bin_b, _marker_b) = fake_codex("b");
        let boundary = VersionProbeNetworkBoundary::InheritedTestSandbox;
        let probes = |m: &PathBuf| fs::read(m).map(|b| b.len()).unwrap_or(0);

        probe_supported_codex_version_with_boundary(bin_a.to_str().unwrap(), boundary)
            .await
            .expect("A verifies");
        let after_first_a = probes(&marker_a);
        assert_eq!(after_first_a, 1, "the first verification must actually probe");

        probe_supported_codex_version_with_boundary(bin_b.to_str().unwrap(), boundary)
            .await
            .expect("B verifies");

        probe_supported_codex_version_with_boundary(bin_a.to_str().unwrap(), boundary)
            .await
            .expect("A verifies again");
        assert_eq!(
            probes(&marker_a),
            after_first_a,
            "verifying a DIFFERENT binary must not evict A's entry — A re-probed, so the cache is \
             still a single shared slot"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn availability_runs_only_the_local_version_probe_and_finds_gui_install_paths() {
        use std::os::unix::fs::PermissionsExt;

        let root = crate::storage::db::unique_temp_path("murmur-codex-readiness-local-only", "dir");
        let bin_dir = root.join(".nvm/versions/node/v22.0.0/bin");
        let auth_home = root.join("auth");
        fs::create_dir_all(&bin_dir).unwrap();
        fs::create_dir_all(&auth_home).unwrap();

        let execution_marker = root.join("codex-was-executed");
        let fake_codex = bin_dir.join("codex");
        fs::write(
            &fake_codex,
            format!(
                "#!/bin/sh\nif [ \"$#\" -eq 1 ] && [ \"$1\" = \"--version\" ]; then /usr/bin/printf v >> '{}'; /usr/bin/printf 'codex-cli 0.146.7\\n'; exit 0; fi\nif [ \"$#\" -eq 2 ] && [ \"$1\" = \"login\" ] && [ \"$2\" = \"status\" ]; then /usr/bin/printf a >> '{}'; /usr/bin/grep -q '\"account\":\"valid\"' \"$CODEX_HOME/auth.json\" || exit 1; /usr/bin/printf 'Logged in using ChatGPT\\n'; exit 0; fi\nexit 91\n",
                execution_marker.display(),
                execution_marker.display()
            ),
        )
        .unwrap();
        fs::set_permissions(&fake_codex, fs::Permissions::from_mode(0o700)).unwrap();

        let auth = auth_home.join("auth.json");
        fs::write(&auth, b"{\"account\":\"valid\"}").unwrap();
        fs::set_permissions(&auth, fs::Permissions::from_mode(0o600)).unwrap();

        let provider = CodexCliProvider::new();
        let availability = provider
            .availability_from(Some(&auth_home), Some(&root), None)
            .await;
        assert_eq!(availability, Availability::Available);
        let cached_availability = provider
            .availability_from(Some(&auth_home), Some(&root), None)
            .await;
        assert_eq!(cached_availability, Availability::Available);
        assert!(
            execution_marker.exists(),
            "availability must run the local version probe before reporting Available"
        );
        assert_eq!(
            fs::read(&execution_marker).unwrap(),
            b"vaa",
            "the version probe may be cached, but local authentication state must be revalidated"
        );

        fs::write(
            &fake_codex,
            "#!/bin/sh\n/usr/bin/printf 'codex-cli 0.147.0\\n'\n",
        )
        .unwrap();
        fs::set_permissions(&fake_codex, fs::Permissions::from_mode(0o700)).unwrap();
        let incompatible = provider
            .availability_from(Some(&auth_home), Some(&root), None)
            .await;
        assert!(
            matches!(
                incompatible,
                Availability::Unavailable { reason }
                    if reason.contains("requires Codex CLI 0.146.x")
            ),
            "Settings readiness must reject the same unsupported version as generation"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn availability_rejects_malformed_and_signed_out_auth_state() {
        use std::os::unix::fs::PermissionsExt;

        let root = crate::storage::db::unique_temp_path("murmur-codex-auth-state", "dir");
        let auth_home = root.join("auth");
        fs::create_dir_all(&auth_home).unwrap();
        let fake_codex = root.join("codex");
        fs::write(
            &fake_codex,
            "#!/bin/sh\nif [ \"$#\" -eq 1 ] && [ \"$1\" = \"--version\" ]; then /usr/bin/printf 'codex-cli 0.146.0\\n'; exit 0; fi\nif [ \"$#\" -eq 2 ] && [ \"$1\" = \"login\" ] && [ \"$2\" = \"status\" ]; then /usr/bin/grep -q '\"account\":\"valid\"' \"$CODEX_HOME/auth.json\" || exit 1; /usr/bin/printf 'Logged in using ChatGPT\\n'; exit 0; fi\nexit 91\n",
        )
        .unwrap();
        fs::set_permissions(&fake_codex, fs::Permissions::from_mode(0o700)).unwrap();
        let auth = auth_home.join("auth.json");
        fs::write(&auth, b"not-json").unwrap();
        fs::set_permissions(&auth, fs::Permissions::from_mode(0o600)).unwrap();
        let provider = CodexCliProvider::with_binary(fake_codex.to_string_lossy().into_owned());

        for invalid in [b"not-json".as_slice(), br#"{"account":"signed-out"}"#] {
            fs::write(&auth, invalid).unwrap();
            let availability = provider
                .availability_from(Some(&auth_home), Some(&root), None)
                .await;
            assert!(
                matches!(
                    availability,
                    Availability::Unavailable { reason }
                        if reason.contains("not signed in")
                ),
                "a private file is not proof of an authenticated Codex session"
            );
        }

        fs::write(&auth, br#"{"account":"valid"}"#).unwrap();
        assert_eq!(
            provider
                .availability_from(Some(&auth_home), Some(&root), None)
                .await,
            Availability::Available
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn availability_version_probe_has_a_short_deadline_and_reaps_its_group() {
        use std::os::unix::fs::PermissionsExt;

        let root = crate::storage::db::unique_temp_path("murmur-codex-readiness-timeout", "dir");
        let auth_home = root.join("auth");
        fs::create_dir_all(&auth_home).unwrap();
        let fake_codex = root.join("codex");
        fs::write(
            &fake_codex,
            "#!/bin/sh\n/bin/sleep 30\n/usr/bin/printf 'codex-cli 0.146.0\\n'\n",
        )
        .unwrap();
        fs::set_permissions(&fake_codex, fs::Permissions::from_mode(0o700)).unwrap();
        let auth = auth_home.join("auth.json");
        fs::write(&auth, b"{\"synthetic\":\"credential\"}").unwrap();
        fs::set_permissions(&auth, fs::Permissions::from_mode(0o600)).unwrap();
        let provider = CodexCliProvider::with_binary(fake_codex.to_string_lossy().into_owned());

        // Derived, not restated: spelling the budget into the expected text meant tuning the
        // constant broke this assertion for a reason that had nothing to do with the
        // behaviour it covers.
        let expected_timeout_phrase =
            format!("timed out after {}s", CODEX_VERSION_PROBE_TIMEOUT.as_secs());
        let started = Instant::now();
        let availability = provider
            .availability_from(Some(&auth_home), Some(&root), None)
            .await;
        let elapsed = started.elapsed();

        assert!(
            matches!(
                availability,
                Availability::Unavailable { ref reason }
                    if reason.contains(&expected_timeout_phrase)
            ),
            "a wedged version probe must fail as a short readiness check"
        );
        assert!(
            elapsed < CODEX_VERSION_PROBE_TIMEOUT + Duration::from_secs(2),
            "readiness must not inherit the generation timeout: {elapsed:?}"
        );
        assert!(
            !crate::summarize::claude_code::has_unproven_process_group(),
            "the timed-out version probe must prove its process group dead"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn injected_configuration_free_path_precedes_package_manager_fallbacks() {
        use std::os::unix::fs::PermissionsExt;

        let root = crate::storage::db::unique_temp_path("murmur-codex-path-precedence", "dir");
        let shell_bin = root.join("shell-bin");
        let fallback_bin = root.join(".bun/bin");
        fs::create_dir_all(&shell_bin).unwrap();
        fs::create_dir_all(&fallback_bin).unwrap();
        for binary in [shell_bin.join("codex"), fallback_bin.join("codex")] {
            fs::write(&binary, b"synthetic executable").unwrap();
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        }

        let resolved =
            resolve_codex_binary_from("codex", Some(&root), Some(shell_bin.as_os_str())).unwrap();
        assert_eq!(Path::new(&resolved), shell_bin.join("codex"));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn default_resolver_sorts_nvm_candidates_deterministically() {
        use std::os::unix::fs::PermissionsExt;

        let root = crate::storage::db::unique_temp_path("murmur-codex-nvm-order", "dir");
        let v20 = root.join(".nvm/versions/node/v20/bin/codex");
        let v18 = root.join(".nvm/versions/node/v18/bin/codex");
        fs::create_dir_all(v20.parent().unwrap()).unwrap();
        fs::create_dir_all(v18.parent().unwrap()).unwrap();
        for binary in [&v20, &v18] {
            fs::write(binary, b"#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(binary, fs::Permissions::from_mode(0o700)).unwrap();
        }

        assert_eq!(
            resolve_codex_binary_from(DEFAULT_BINARY, Some(&root), None).unwrap(),
            v18.to_string_lossy()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pinned_provider_rejects_runtime_identity_drift_before_generation() {
        use std::os::unix::fs::PermissionsExt;

        let root = crate::storage::db::unique_temp_path("murmur-codex-runtime-pin", "dir");
        fs::create_dir_all(&root).unwrap();
        let binary = root.join("codex");
        fs::write(&binary, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let canonical = fs::canonicalize(&binary).unwrap();
        let runtime = Arc::new(PinnedCodexRuntime {
            path: canonical.clone(),
            version: "codex-cli 0.146.0".into(),
            identity: codex_binary_identity(&canonical).unwrap(),
        });
        let provider = CodexCliProvider::new().with_pinned_runtime(Arc::clone(&runtime));
        provider
            .assert_pinned_runtime_unchanged(canonical.to_str().unwrap())
            .unwrap();

        fs::write(&binary, b"#!/bin/sh\n/usr/bin/printf drift\nexit 0\n").unwrap();
        assert!(provider
            .assert_pinned_runtime_unchanged(canonical.to_str().unwrap())
            .is_err());
        assert!(runtime.assert_unchanged().is_err());
        let error = provider
            .complete("system sentinel", "user sentinel")
            .await
            .unwrap_err();
        assert!(matches!(error, AppError::Unavailable(message)
            if message == "pinned Codex runtime changed during the quality run"));
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn runtime_home_exposes_only_validated_auth_not_ambient_config() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let source = crate::storage::db::unique_temp_path("murmur-codex-source-home", "dir");
        fs::create_dir_all(&source).unwrap();
        let auth = source.join("auth.json");
        fs::write(&auth, b"{\"synthetic\":\"credential\"}").unwrap();
        fs::set_permissions(&auth, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(
            source.join("config.toml"),
            b"[hooks]\n# must never enter the isolated runtime home\n",
        )
        .unwrap();
        let external_hook = crate::storage::db::unique_temp_path("murmur-codex-foreign-hook", "sh");
        fs::write(&external_hook, b"#!/bin/sh\nexit 99\n").unwrap();
        symlink(&external_hook, source.join("foreign-hook")).unwrap();

        let runtime = CodexRuntimeHome::prepare_from(Some(&source)).unwrap();
        let entries: Vec<_> = fs::read_dir(runtime.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("auth.json")]);
        assert_eq!(
            fs::canonicalize(runtime.path().join("auth.json")).unwrap(),
            fs::canonicalize(&auth).unwrap()
        );
        assert!(!runtime.path().join("config.toml").exists());
        assert!(!runtime.path().join("foreign-hook").exists());

        drop(runtime);
        let _ = fs::remove_file(external_hook);
        let _ = fs::remove_dir_all(source);
    }

    #[cfg(unix)]
    #[test]
    fn local_auth_readiness_requires_a_private_user_owned_file() {
        use std::os::unix::fs::PermissionsExt;

        let source = crate::storage::db::unique_temp_path("murmur-codex-auth-readiness", "dir");
        fs::create_dir_all(&source).unwrap();
        assert_eq!(validated_auth_path(Some(&source)).unwrap(), None);

        let auth = source.join("auth.json");
        fs::write(&auth, b"{\"synthetic\":\"credential\"}").unwrap();
        fs::set_permissions(&auth, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            validated_auth_path(Some(&source)).unwrap(),
            Some(fs::canonicalize(&auth).unwrap())
        );

        fs::set_permissions(&auth, fs::Permissions::from_mode(0o644)).unwrap();
        let error = validated_auth_path(Some(&source)).unwrap_err().to_string();
        assert!(error.contains("user-owned and private"));
        assert!(!error.contains(auth.to_string_lossy().as_ref()));

        let binary = source.join("codex");
        fs::write(&binary, b"synthetic binary").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(validate_executable(&binary).is_err());
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        validate_executable(&binary).unwrap();
        let _ = fs::remove_dir_all(source);
    }

    #[cfg(unix)]
    #[test]
    fn refreshed_auth_replacement_is_atomically_preserved() {
        use std::os::unix::fs::PermissionsExt;

        let source = crate::storage::db::unique_temp_path("murmur-codex-refresh-source", "dir");
        fs::create_dir_all(&source).unwrap();
        let auth = source.join("auth.json");
        fs::write(&auth, b"{\"token\":\"old-synthetic\"}").unwrap();
        fs::set_permissions(&auth, fs::Permissions::from_mode(0o600)).unwrap();

        let mut runtime = CodexRuntimeHome::prepare_from(Some(&source)).unwrap();
        let runtime_auth = runtime.path().join("auth.json");
        fs::remove_file(&runtime_auth).unwrap();
        fs::write(&runtime_auth, b"{\"token\":\"rotated-synthetic\"}").unwrap();
        fs::set_permissions(&runtime_auth, fs::Permissions::from_mode(0o600)).unwrap();
        runtime.finalize().unwrap();
        assert_eq!(
            fs::read(&auth).unwrap(),
            b"{\"token\":\"rotated-synthetic\"}",
            "a Codex atomic auth replacement must survive runtime-home cleanup"
        );

        drop(runtime);
        let _ = fs::remove_dir_all(source);
    }

    #[cfg(unix)]
    #[test]
    fn failed_auth_sync_retains_only_auth_and_removes_runtime_artifacts() {
        use std::os::unix::fs::PermissionsExt;

        let source = crate::storage::db::unique_temp_path("murmur-codex-recovery-source", "dir");
        fs::create_dir_all(&source).unwrap();
        let auth = source.join("auth.json");
        fs::write(&auth, b"{\"token\":\"old-synthetic\"}").unwrap();
        fs::set_permissions(&auth, fs::Permissions::from_mode(0o600)).unwrap();

        let mut runtime = CodexRuntimeHome::prepare_from(Some(&source)).unwrap();
        let runtime_path = runtime.path().to_path_buf();
        let runtime_auth = runtime.path().join("auth.json");
        fs::remove_file(&runtime_auth).unwrap();
        fs::write(&runtime_auth, b"{\"token\":\"rotated-synthetic\"}").unwrap();
        fs::set_permissions(&runtime_auth, fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(
            runtime.path().join("session-artifact.jsonl"),
            b"MURMUR_MEETING_DERIVED_SENTINEL",
        )
        .unwrap();
        // Force the atomic destination staging write to fail after the CLI produced a valid,
        // rotated auth file.
        fs::set_permissions(&source, fs::Permissions::from_mode(0o500)).unwrap();
        let error = runtime.finalize().unwrap_err().to_string();
        let recovery = runtime
            .recovery_path
            .clone()
            .expect("a valid rotated auth file must get its own recovery directory");
        assert!(recovery
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(RECOVERY_DIR_PREFIX));
        assert!(recovery.is_dir());
        assert!(
            error.contains(recovery.to_string_lossy().as_ref()),
            "the recovery error must name the retained directory"
        );
        let entries = fs::read_dir(&recovery)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, vec![std::ffi::OsString::from("auth.json")]);
        assert_eq!(
            fs::read(recovery.join("auth.json")).unwrap(),
            b"{\"token\":\"rotated-synthetic\"}"
        );

        drop(runtime);
        assert!(
            !runtime_path.exists(),
            "the disposable CODEX_HOME must always be removed after finalize failure"
        );
        assert!(
            !recovery.join("session-artifact.jsonl").exists(),
            "meeting-derived runtime artifacts must never enter auth recovery"
        );
        fs::set_permissions(&source, fs::Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir_all(&recovery).unwrap();
        let _ = fs::remove_dir_all(source);
    }

    #[cfg(unix)]
    #[test]
    fn recovery_sweep_caps_private_directories_without_following_symlinks() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let root = crate::storage::db::unique_temp_path("murmur-codex-recovery-sweep", "dir");
        let external =
            crate::storage::db::unique_temp_path("murmur-codex-recovery-external", "dir");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&external).unwrap();
        fs::write(external.join("must-survive"), b"sentinel").unwrap();
        for index in 0..(MAX_RECOVERY_DIRS + 2) {
            let recovery = root.join(format!("{RECOVERY_DIR_PREFIX}{index}"));
            fs::create_dir(&recovery).unwrap();
            fs::set_permissions(&recovery, fs::Permissions::from_mode(0o700)).unwrap();
        }
        symlink(
            &external,
            root.join(format!("{RECOVERY_DIR_PREFIX}external-link")),
        )
        .unwrap();

        sweep_codex_recovery_dirs_in(&root);

        let retained_directories = fs::read_dir(&root)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .filter(|entry| {
                fs::symlink_metadata(entry.path())
                    .is_ok_and(|metadata| metadata.file_type().is_dir())
            })
            .count();
        assert_eq!(retained_directories, MAX_RECOVERY_DIRS);
        assert!(
            external.join("must-survive").is_file(),
            "the sweep must never follow a recovery-shaped symlink"
        );

        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(external);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn isolation_cwd_exists_on_supported_macos_hosts() {
        ensure_empty_cwd().expect("/var/empty must exist on the supported macOS runtime");
    }

    #[test]
    fn model_values_cannot_be_parsed_as_options() {
        assert!(validate_model("-dangerous").is_err());
        assert!(validate_model(" gpt-5.6-sol").is_ok());
    }

    #[test]
    fn hook_trust_bypass_is_gated_to_the_verified_codex_minor() {
        assert!(validate_supported_codex_version("codex-cli 0.146.0").is_ok());
        assert!(validate_supported_codex_version("codex-cli 0.146.99").is_ok());
        for unsupported in [
            "codex-cli 0.145.9",
            "codex-cli 0.147.0",
            "codex-cli 1.146.0",
            "codex 0.146.0",
            "codex-cli unknown",
            "codex-cli 0.146",
            "codex-cli 0.146.0.1",
            "codex-cli 0.146.0-beta.1",
            "codex-cli 0.146.-1",
        ] {
            assert!(
                validate_supported_codex_version(unsupported).is_err(),
                "{unsupported} must fail closed before hook-trust bypass"
            );
        }
    }

    #[test]
    fn parser_accepts_only_reasoning_and_final_agent_text() {
        let jsonl = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"t\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"reasoning\",\"text\":\"hidden\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"hello\"}}\n",
            "{\"type\":\"turn.completed\",\"usage\":{}}\n"
        );
        assert_eq!(parse_codex_jsonl(jsonl).unwrap(), "hello");

        let unknown_envelope = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"t\"}\n",
            "{\"type\":\"turn.telemetry\",\"tokens\":12}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"hello\"}}\n",
            "{\"type\":\"turn.completed\",\"usage\":{}}\n"
        );
        let error = parse_codex_jsonl(unknown_envelope).unwrap_err().to_string();
        assert!(error.contains("unsupported event type"));
        assert!(!error.contains("hello"));
    }

    #[test]
    fn parser_fails_closed_on_native_tool_activity() {
        let jsonl = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"t\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.started\",\"item\":{\"type\":\"command_execution\",\"command\":\"ls\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"leaked\"}}\n",
            "{\"type\":\"turn.completed\"}\n"
        );
        let error = parse_codex_jsonl(jsonl).unwrap_err().to_string();
        assert!(error.contains("disabled native activity"));
        assert!(!error.contains("ls"));
        assert!(!error.contains("leaked"));

        let mcp_jsonl = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"t\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.started\",\"item\":{\"type\":\"mcp_tool_call\",\"server\":\"gmail\",\"arguments\":\"private\"}}\n",
            "{\"type\":\"turn.completed\"}\n"
        );
        let error = parse_codex_jsonl(mcp_jsonl).unwrap_err().to_string();
        assert!(error.contains("disabled native activity"));
        assert!(!error.contains("gmail"));
        assert!(!error.contains("private"));

        let unknown_top_level_tool = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"t\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"tool.started\",\"tool\":\"future_connector\",\"payload\":\"private\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"leaked\"}}\n",
            "{\"type\":\"turn.completed\"}\n"
        );
        let error = parse_codex_jsonl(unknown_top_level_tool)
            .unwrap_err()
            .to_string();
        assert!(error.contains("unsupported event type"));
        assert!(!error.contains("future_connector"));
        assert!(!error.contains("private"));
        assert!(!error.contains("leaked"));
    }

    #[test]
    fn parser_rejects_multiple_final_messages_instead_of_truncating() {
        let jsonl = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"t\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"first\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"second\"}}\n",
            "{\"type\":\"turn.completed\"}\n"
        );
        let error = parse_codex_jsonl(jsonl).unwrap_err().to_string();
        assert!(error.contains("multiple final messages"));
        assert!(!error.contains("first"));
        assert!(!error.contains("second"));
    }

    #[test]
    fn parser_accepts_only_the_anchored_hook_trust_advisory() {
        let jsonl = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"t\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"error\",\"message\":\"`--dangerously-bypass-hook-trust` is enabled. Enabled hooks may run without review for this invocation.\"}}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"hello\"}}\n",
            "{\"type\":\"turn.completed\"}\n"
        );
        assert_eq!(parse_codex_jsonl(jsonl).unwrap(), "hello");
    }

    #[test]
    fn parser_accepts_exact_hook_notice_before_thread_start_only() {
        const HOOK_NOTICE: &str = "`--dangerously-bypass-hook-trust` is enabled. Enabled hooks may run without review for this invocation.";
        let jsonl = format!(
            concat!(
                "{{\"type\":\"item.completed\",\"item\":{{\"id\":\"pre-thread\",\"type\":\"error\",\"message\":{notice}}}}}\n",
                "{{\"type\":\"thread.started\",\"thread_id\":\"t\"}}\n",
                "{{\"type\":\"item.completed\",\"item\":{{\"id\":\"pre-turn\",\"type\":\"error\",\"message\":{notice}}}}}\n",
                "{{\"type\":\"turn.started\"}}\n",
                "{{\"type\":\"item.completed\",\"item\":{{\"type\":\"agent_message\",\"text\":\"hello\"}}}}\n",
                "{{\"type\":\"turn.completed\"}}\n"
            ),
            notice = serde_json::to_string(HOOK_NOTICE).unwrap()
        );
        assert_eq!(parse_codex_jsonl(&jsonl).unwrap(), "hello");

        for forbidden in [
            jsonl.replace(HOOK_NOTICE, "arbitrary pre-thread error"),
            jsonl.replace("\"type\":\"error\"", "\"type\":\"command_execution\""),
            jsonl.replace("\"type\":\"item.completed\"", "\"type\":\"item.started\""),
        ] {
            assert!(
                parse_codex_jsonl(&forbidden).is_err(),
                "only the exact completed hook advisory may precede thread.started"
            );
        }
    }

    #[test]
    fn parser_reports_genuine_error_items_as_errors_not_tool_activity() {
        let jsonl = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"t\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"error\",\"message\":\"account unavailable\"}}\n",
            "{\"type\":\"turn.completed\"}\n"
        );
        let error = parse_codex_jsonl(jsonl).unwrap_err().to_string();
        assert!(error.contains("error item"));
        assert!(!error.contains("native activity"));
        assert!(!error.contains("account unavailable"));

        let quoted_flag = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"t\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"error\",\"message\":\"Hook failed while parsing --dangerously-bypass-hook-trust and requires review\"}}\n",
            "{\"type\":\"turn.completed\"}\n"
        );
        assert!(parse_codex_jsonl(quoted_flag).is_err());
    }

    #[test]
    fn pinned_codex_0_146_tool_denial_fixture_matches_the_parser_contract() {
        // Historical synthetic captures from Codex CLI 0.146.0. These pin only the JSONL parser
        // contract; runtime isolation is proven separately by command/home construction tests.
        let jsonl = include_str!("fixtures/codex-cli-0.146.0-tool-denial.jsonl");
        let stderr = include_str!("fixtures/codex-cli-0.146.0-tool-denial.stderr");
        let output = parse_codex_jsonl(jsonl).unwrap();
        assert!(output.contains("blocked before execution"));
        assert!(stderr.contains("Command blocked by PreToolUse hook"));
        for forbidden in [
            "\"type\":\"command_execution\"",
            "\"type\":\"web_search\"",
            "\"type\":\"mcp_tool_call\"",
            "MURMUR_TOOL_SHOULD_NOT_RUN\n",
        ] {
            assert!(!jsonl.contains(forbidden));
        }

        // A second real-CLI fixture uses the same production config with an explicit web-search
        // request. Codex reports that no web-search capability exists and emits no tool event.
        let web_jsonl = include_str!("fixtures/codex-cli-0.146.0-web-disabled.jsonl");
        let web_output = parse_codex_jsonl(web_jsonl).unwrap();
        assert!(web_output.contains("don’t have a web search tool available"));
        assert!(!web_jsonl.contains("\"type\":\"web_search\""));
        assert!(!web_jsonl.contains("MURMUR_WEB_SHOULD_NOT_RUN"));
    }

    #[test]
    fn parser_rejects_malformed_or_incomplete_streams() {
        assert!(parse_codex_jsonl("not-json").is_err());
        assert!(parse_codex_jsonl(
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"x\"}}\n"
        )
        .is_err());
    }

    #[test]
    fn parser_enforces_protocol_order_and_terminal_cardinality() {
        let completed_before_message = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"t\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"turn.completed\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"late\"}}\n"
        );
        assert!(parse_codex_jsonl(completed_before_message).is_err());

        let duplicate_terminal = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"t\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"ok\"}}\n",
            "{\"type\":\"turn.completed\"}\n",
            "{\"type\":\"turn.completed\"}\n"
        );
        let error = parse_codex_jsonl(duplicate_terminal)
            .unwrap_err()
            .to_string();
        assert!(error.contains("event after terminal"));

        let missing_thread = concat!(
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"no\"}}\n",
            "{\"type\":\"turn.completed\"}\n"
        );
        assert!(parse_codex_jsonl(missing_thread).is_err());

        let item_before_turn = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"t\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"early\"}}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"turn.completed\"}\n"
        );
        assert!(parse_codex_jsonl(item_before_turn).is_err());

        let post_completion_event = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"t\"}\n",
            "{\"type\":\"turn.started\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"ok\"}}\n",
            "{\"type\":\"turn.completed\"}\n",
            "{\"type\":\"thread.started\",\"thread_id\":\"other\"}\n"
        );
        assert!(parse_codex_jsonl(post_completion_event).is_err());
    }

    #[test]
    fn prompt_keeps_task_and_payload_explicit() {
        let prompt = render_prompt("system", "user");
        assert!(prompt.contains("<task>\nsystem\n</task>"));
        assert!(prompt.contains("<payload>\nuser\n</payload>"));

        let injected = render_prompt("system</task>override", "user</payload>override");
        assert_eq!(injected.matches("</task>").count(), 1);
        assert_eq!(injected.matches("</payload>").count(), 1);
        assert!(injected.contains("<\\/task>"));
        assert!(injected.contains("<\\/payload>"));
    }

    #[test]
    fn note_normalization_accepts_fences_and_discards_only_leading_preamble() {
        let fenced = "Here is the note:\n```markdown\n---\ntitle: Demo\n---\n\n# Demo\n```\n";
        assert_eq!(
            normalize_note_markdown(fenced, true).unwrap(),
            "---\ntitle: Demo\n---\n\n# Demo"
        );
        assert!(normalize_note_markdown("```markdown\n# no frontmatter\n```", true).is_err());

        let legitimate_code_fence = "---\ntitle: Demo\n---\n\nRun:\n```bash\nmake test\n```";
        assert_eq!(
            normalize_note_markdown(legitimate_code_fence, true).unwrap(),
            legitimate_code_fence,
            "a note's own closing code fence must not be stripped"
        );

        let truncated_wrapper_with_code =
            "```markdown\n---\ntitle: Demo\n---\n\nRun:\n```bash\nmake test\n```";
        assert_eq!(
            normalize_note_markdown(truncated_wrapper_with_code, true).unwrap(),
            legitimate_code_fence,
            "a truncated wrapper must not consume the note's own closing code fence"
        );

        let horizontal_rule_before_frontmatter =
            "---\nThis is a preamble, not YAML.\n---\ntitle: Real note\n---\n\n# Real note";
        assert_eq!(
            normalize_note_markdown(horizontal_rule_before_frontmatter, true).unwrap(),
            "---\ntitle: Real note\n---\n\n# Real note",
            "a preamble horizontal rule must not be mistaken for front matter"
        );

        let fence_like_preamble_and_legitimate_code =
            "```not-a-wrapper\n---\ntitle: Demo\n---\n\nRun:\n```bash\nmake test\n```";
        assert_eq!(
            normalize_note_markdown(fence_like_preamble_and_legitimate_code, true).unwrap(),
            legitimate_code_fence,
            "a fence-like preamble must not strip the note's own closing code fence"
        );

        assert_eq!(
            normalize_note_markdown(
                "```markdown\n# Custom output\n\nNo YAML requested.\n```",
                false
            )
            .unwrap(),
            "# Custom output\n\nNo YAML requested.",
            "a custom template may intentionally return Markdown without front matter"
        );
    }

    #[test]
    fn provider_defaults_to_codex_binary_and_no_model_override() {
        let provider = CodexCliProvider::with_binary("custom-codex".into());
        assert_eq!(provider.binary, "custom-codex");
        assert!(provider.model.is_empty());
        assert!(provider.effort.is_empty());
        let provider = provider
            .with_model("gpt-5.6-sol".into())
            .with_effort("high".into());
        assert_eq!(provider.model, "gpt-5.6-sol");
        assert_eq!(provider.effort, "high");
    }

    #[test]
    fn effort_is_allowlisted_and_reaches_the_isolated_command() {
        assert_eq!(
            effort_args(" high "),
            vec![
                "--config".to_string(),
                "model_reasoning_effort=\"high\"".to_string()
            ]
        );
        assert!(effort_args("").is_empty());
        assert!(effort_args("xhigh").is_empty());
        assert!(effort_args("high --sandbox danger-full-access").is_empty());

        let command = build_codex_command_with_effort(
            "codex",
            "gpt-5.6-sol",
            "high",
            Path::new("/tmp/murmur-codex-test"),
        );
        let argv = args(&command);
        let effort_pos = argv
            .windows(2)
            .position(|pair| pair == ["--config", "model_reasoning_effort=\"high\""])
            .expect("effort must be a strict-config pair");
        let model_pos = argv
            .windows(2)
            .position(|pair| pair == ["--model", "gpt-5.6-sol"])
            .expect("model must be forwarded");
        assert!(effort_pos > model_pos);
    }
}
