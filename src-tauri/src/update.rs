//! GitHub-release-based update check (#112).
//!
//! Three Tauri commands:
//! - [`check_for_update`] — GETs the latest GitHub release for `murmur-io/murmur`, compares its
//!   tag against the compiled `CARGO_PKG_VERSION` by hand-rolled semver, and reports whether a
//!   newer version exists. Sends NO user content — a plain unauthenticated GET to GitHub.
//! - [`app_info`] — compile-time app identity (name/version/description/repository).
//! - [`open_release_page`] — opens the release page in the default browser, host-validated to the
//!   Murmur repo so it can never become an arbitrary-URL launcher.
//!
//! Rules: only [`AppError`] / `crate::error::Result`; no `unwrap`/`expect` in non-test code; no
//! PII in logs (the update check carries none, but we still keep the URL/body out of logs).

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::error::Result;

/// The canonical Murmur repository. The update check and the release-page launcher are both pinned
/// to this — `open_release_page` refuses any URL not under it.
const REPO_URL: &str = "https://github.com/murmur-io/murmur";
/// GitHub API endpoint for the newest published release of the Murmur repo.
/// The one host Murmur contacts on its own initiative; named for the egress ledger.
const UPDATE_CHECK_HOST: &str = "api.github.com";

const LATEST_RELEASE_API: &str = "https://api.github.com/repos/murmur-io/murmur/releases/latest";

// ── IPC DTOs (camelCase mirrors) ──

/// Result of an update check: the compiled version vs. the newest GitHub release.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    /// The running app version (`CARGO_PKG_VERSION`), e.g. `"0.6.3"`.
    pub current_version: String,
    /// The latest GitHub release tag with a leading `v`/`V` stripped, e.g. `"0.6.4"`.
    pub latest_version: String,
    /// `true` iff `latest_version` parses to a strictly-greater semver than `current_version`.
    pub update_available: bool,
    /// The release's `html_url` for the user to open.
    pub release_url: String,
    /// The release title (`name`), if any.
    pub release_name: Option<String>,
    /// The release body / changelog (`body`), if any.
    pub release_notes: Option<String>,
}

/// Compile-time app identity for the About screen.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub description: String,
    pub repository: String,
}

/// Minimal mirror of the GitHub `releases/latest` response — only the fields we use.
#[derive(Debug, Deserialize)]
struct GithubRelease {
    #[serde(default)]
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    body: Option<String>,
}

/// Build the reqwest client for the update check: TLS 1.2 floor + a short (~10s) overall timeout so
/// a stalled connection can't wedge the UI. Mirrors `summarize::anthropic::build_client`'s hardened
/// builder pattern; falls back to `Client::new()` if the builder somehow fails so the check is
/// never un-constructible.
fn build_client() -> reqwest::Client {
    reqwest::Client::builder()
        .min_tls_version(reqwest::tls::Version::TLS_1_2)
        .timeout(std::time::Duration::from_secs(10))
        .connect_timeout(std::time::Duration::from_secs(8))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Strip a single leading `v`/`V` from a release tag to get the bare version string.
fn strip_v(tag: &str) -> &str {
    tag.strip_prefix('v')
        .or_else(|| tag.strip_prefix('V'))
        .unwrap_or(tag)
}

/// Parse a version string into `(major, minor, patch)`. Splits on `.`; for each of the first three
/// components it takes the leading numeric run before any `-`/`+` (pre-release/build) suffix.
/// Returns `None` if the first three components are not all present-and-numeric — a malformed
/// version must never panic and must be treated by the caller as "no update".
fn parse_version(v: &str) -> Option<(u32, u32, u32)> {
    let mut parts = v.trim().split('.');
    let major = parse_component(parts.next()?)?;
    let minor = parse_component(parts.next()?)?;
    let patch = parse_component(parts.next()?)?;
    Some((major, minor, patch))
}

/// Parse one dotted component: take the leading numeric run before any non-digit (so `4-rc1` → 4,
/// `4+build` → 4). Empty / non-numeric-leading → `None`.
fn parse_component(s: &str) -> Option<u32> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<u32>().ok()
}

/// `true` iff `latest` is a strictly-greater semver than `current`. Defensive: if EITHER side fails
/// to parse, return `false` (never claim an update on a version we can't understand).
fn is_newer(current: &str, latest: &str) -> bool {
    match (parse_version(current), parse_version(latest)) {
        (Some(c), Some(l)) => l > c,
        _ => false,
    }
}

/// GET the latest GitHub release for `murmur-io/murmur` and report whether it is newer than the
/// running build. Sends no user content. Network / non-2xx / rate-limit → `AppError::Unavailable`
/// with a non-PII message (no tokens, no response body dumped).
/// Headless core of [`check_for_update`], so the consent gate and the ledger row are testable
/// without a Tauri `State`.
///
/// `manual` is the whole distinction. An automatic launch-time check is something the app decides
/// to do to the user, so it is governed by `update_check_enabled` and refuses BEFORE a request is
/// built — the gate is here rather than in the frontend precisely so it cannot be routed around. A
/// manual check is the user pressing a button that says it asks GitHub; pressing it IS the consent,
/// and the flag does not apply.
pub(crate) async fn check_for_update_inner(
    state: &crate::state::AppState,
    manual: bool,
) -> Result<UpdateInfo> {
    check_for_update_against(state, manual, LATEST_RELEASE_API).await
}

/// As [`check_for_update_inner`], with the endpoint injected.
///
/// The seam exists for ONE test: that the ledger row is written even when the request FAILS. That
/// property cannot be tested against the real endpoint, because the real endpoint answers — review
/// moved the `ledger_row` call to after the request and the test stayed green, since GitHub is
/// reachable from CI. Pointing this at an unroutable host makes the failure deterministic, so the
/// ordering is actually pinned rather than merely asserted in a comment.
pub(crate) async fn check_for_update_against(
    state: &crate::state::AppState,
    manual: bool,
    api_url: &str,
) -> Result<UpdateInfo> {
    let current_version = env!("CARGO_PKG_VERSION").to_string();

    if !manual {
        // Fail CLOSED on a poisoned lock: a broken config must not turn a silent network call back
        // on. The user cannot see this decision, so the safe default is not to make the request.
        let enabled = state
            .config
            .lock()
            .map(|c| c.update_check_enabled)
            .unwrap_or(false);
        if !enabled {
            return Err(AppError::Unavailable(
                "automatic update checks are turned off".into(),
            ));
        }
    }

    // Logged BEFORE the request, not after. A ledger that only records calls which SUCCEEDED is a
    // ledger that hides exactly the ones worth auditing — a request that reached GitHub and then
    // timed out still left the machine. No content is sent, so the byte count is zero.
    crate::share::ledger_row(&state.db, UPDATE_CHECK_HOST, "update_check", 0);

    let client = build_client();
    // GitHub's API 403s without a User-Agent; also request the recommended media type.
    let user_agent = format!("Murmur/{current_version}");
    let resp = client
        .get(api_url)
        .header(reqwest::header::USER_AGENT, user_agent)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| {
            tracing::warn!(target: "update", is_timeout = e.is_timeout(), "update check network error");
            AppError::Unavailable("Could not reach GitHub to check for updates.".into())
        })?;

    let status = resp.status();
    if !status.is_success() {
        tracing::warn!(target: "update", status = status.as_u16(), "update check non-2xx");
        return Err(AppError::Unavailable(format!(
            "GitHub update check failed (HTTP {}).",
            status.as_u16()
        )));
    }

    let release: GithubRelease = resp.json().await.map_err(|_| {
        tracing::warn!(target: "update", "update check response parse error");
        AppError::Unavailable("Could not parse the GitHub release response.".into())
    })?;

    let latest_version = strip_v(&release.tag_name).to_string();
    let update_available = is_newer(&current_version, &latest_version);
    tracing::info!(target: "update", update_available, "update check complete");

    Ok(UpdateInfo {
        current_version,
        latest_version,
        update_available,
        release_url: release.html_url,
        release_name: release.name,
        release_notes: release.body,
    })
}

/// Compile-time app identity for the About screen. Infallible.
#[tauri::command]
pub fn app_info() -> AppInfo {
    let description = {
        let d = env!("CARGO_PKG_DESCRIPTION");
        if d.trim().is_empty() {
            "Local-first meeting notes for macOS.".to_string()
        } else {
            d.to_string()
        }
    };
    AppInfo {
        name: "Murmur".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        description,
        repository: REPO_URL.to_string(),
    }
}

/// Open a release page in the default browser via macOS `open`. SECURITY: only URLs under the
/// Murmur repo are allowed — this must never become an arbitrary-URL launcher. Does not block on
/// the spawned child.
#[tauri::command]
pub fn open_release_page(url: String) -> Result<()> {
    // Require a path boundary after the repo root: the bare repo URL, or something strictly
    // BELOW it (`<repo>/releases/...`). A plain `starts_with(REPO_URL)` would also accept a
    // sibling repo whose name merely begins with "murmur" (e.g. `.../murmur-phish`) — pin the
    // trailing `/` so only the real repo and its sub-paths pass.
    let allowed = url == REPO_URL || url.starts_with(&format!("{REPO_URL}/"));
    if !allowed {
        return Err(AppError::InvalidArg(
            "refused to open a URL outside the Murmur repository".into(),
        ));
    }
    std::process::Command::new("open")
        .arg(&url)
        .spawn()
        .map_err(|e| AppError::Unavailable(format!("could not open the browser: {e}")))?;
    tracing::info!(target: "update", "opened release page");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_basics() {
        assert_eq!(parse_version("0.6.3"), Some((0, 6, 3)));
        assert_eq!(parse_version("0.10.0"), Some((0, 10, 0)));
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
    }

    #[test]
    fn parse_version_with_suffix() {
        assert_eq!(parse_version("0.6.4-rc1"), Some((0, 6, 4)));
        assert_eq!(parse_version("0.6.4+build.7"), Some((0, 6, 4)));
    }

    #[test]
    fn parse_version_malformed_is_none() {
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("0.6"), None);
        assert_eq!(parse_version("v0.6.3"), None); // strip_v runs before parse
        assert_eq!(parse_version("abc"), None);
        assert_eq!(parse_version("0.x.3"), None);
    }

    #[test]
    fn strip_v_variants() {
        assert_eq!(strip_v("v0.6.4"), "0.6.4");
        assert_eq!(strip_v("V0.6.4"), "0.6.4");
        assert_eq!(strip_v("0.6.4"), "0.6.4");
        assert_eq!(strip_v("vv0.6.4"), "v0.6.4"); // only one leading v stripped
    }

    #[test]
    fn newer_true_when_latest_greater() {
        assert!(is_newer("0.6.3", "0.6.4"));
        assert!(is_newer("0.9.9", "0.10.0"));
        assert!(is_newer("0.6.3", "1.0.0"));
    }

    #[test]
    fn not_newer_when_equal_or_older() {
        assert!(!is_newer("0.6.3", "0.6.3"));
        assert!(!is_newer("0.6.3", "0.6.2"));
        assert!(!is_newer("0.10.0", "0.9.9"));
        assert!(!is_newer("1.0.0", "0.9.9"));
    }

    #[test]
    fn newer_defensive_on_malformed() {
        // Malformed latest → never claim an update.
        assert!(!is_newer("0.6.3", "not-a-version"));
        assert!(!is_newer("0.6.3", ""));
        assert!(!is_newer("0.6.3", "0.6"));
        // Malformed current → also no update (can't reason about it).
        assert!(!is_newer("garbage", "0.6.4"));
    }

    #[test]
    fn app_info_is_populated() {
        let info = app_info();
        assert_eq!(info.name, "Murmur");
        assert!(!info.version.is_empty());
        assert!(!info.description.trim().is_empty());
        assert_eq!(info.repository, REPO_URL);
    }

    #[test]
    fn open_release_page_rejects_foreign_host() {
        // A URL outside the repo must be refused with InvalidArg — no process spawned.
        let err = open_release_page("https://evil.example.com/x".into()).unwrap_err();
        assert!(matches!(err, AppError::InvalidArg(_)));
        let err = open_release_page("https://github.com/other/repo".into()).unwrap_err();
        assert!(matches!(err, AppError::InvalidArg(_)));
    }

    #[test]
    fn open_release_page_rejects_prefix_boundary_siblings() {
        // A sibling repo whose name merely BEGINS with the repo URL (no path boundary) must be
        // refused — the fix from a bare `starts_with`. These all stay on github.com but are NOT
        // the Murmur repo.
        for u in [
            "https://github.com/murmur-io/murmur-phish",
            "https://github.com/murmur-io/murmurEVIL/releases",
            "https://github.com/murmur-io/murmur.evil.com",
            "https://github.com/murmur-io/murmur@evil.com",
        ] {
            let err = open_release_page(u.into()).unwrap_err();
            assert!(matches!(err, AppError::InvalidArg(_)), "should reject {u}");
        }
    }
}
