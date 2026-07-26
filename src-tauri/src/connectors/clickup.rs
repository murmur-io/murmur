//! CLICKUP connector — live, on-demand READ-ONLY task search via the ClickUp REST API. Mirrors
//! `connectors::jira` / `connectors::slack` exactly (they are the proven pattern); every framework
//! discipline (consent gate, redaction firewall, content-free egress ledger) is INHERITED from
//! [`crate::connectors::ConnectorRegistry`], never re-implemented here.
//!
//! ## Egress posture (NEW EGRESS CLASS INSTANCE — audited by the lock-security reviewer)
//! [`EgressClass::External`]. Exposed to the brain ONLY when ALL of:
//! - `config.clickup_enabled` (master toggle), AND
//! - `config.clickup_consented` (one-time consent, preserve-only, flipped solely by `consent_to_clickup`), AND
//! - `config.clickup_team_id` is non-empty, AND
//! - a personal API token is present in the Keychain (`clickup_api_token`).
//!
//! Otherwise [`ClickUpConnector::from_config_if_available`] returns `None` (fail-closed: the tool
//! does not exist for the session). The framework redacts the query BEFORE it reaches
//! [`Connector::search`].
//!
//! ## Endpoint — ONE READ, never a write
//! `GET https://api.clickup.com/api/v2/team/{team_id}/task` ("get filtered team tasks"), most
//! recently-updated first, with the personal token in a bare `Authorization` header (ClickUp does
//! NOT use the `Bearer ` prefix). This connector exposes no create/update/delete path at all.
//!
//! ### Why the text match is applied LOCALLY (deliberate, documented)
//! ClickUp's v2 task API has no free-text search parameter — the filtered-team-tasks read is the
//! documented way to reach a workspace's tasks. So this connector fetches ONE page of the most
//! recently-updated tasks and matches the (already-redacted) query against them ON DEVICE
//! ([`matches_query`]). Two consequences, both good for the privacy posture: the query text itself
//! never has to be handed to ClickUp, and the match is deterministic + unit-testable against a canned
//! body. The trade-off is honest: recall is bounded by [`PAGE_LIMIT`] recently-updated tasks, so this
//! is a "what's on my board right now" lookup, not an exhaustive archive search.
//!
//! ## No PII in logs
//! Logs carry connector id + hit count + HTTP status only — never the query, task names, or the token.

use async_trait::async_trait;
use serde::Deserialize;

use super::{Connector, ConnectorError, ConnectorHit, ConnectorResult, EgressClass};
use crate::settings::AppConfig;

/// Keychain account holding the BYO ClickUp personal API token (`pk_…`). NEVER logged / NEVER
/// FE-exposed.
pub const CLICKUP_TOKEN_ACCOUNT: &str = "clickup_api_token";

/// LOUD attribution on every hit — a fixed label, never server-supplied text.
const SOURCE_LABEL: &str = "clickup · task";

/// Max hits returned to the brain — the same budget the sibling connectors use, so one connector can
/// never blow the agent-loop tool budget.
const MAX_HITS: usize = 8;

/// How many recently-updated tasks one read scans locally. ClickUp pages this endpoint at 100; we
/// take page 0 only, so a search never fans out into a multi-request crawl.
pub(crate) const PAGE_LIMIT: usize = 100;

/// Cap on the rendered task name so one absurdly long task can't dominate the tool result.
const TITLE_MAX: usize = 200;

pub struct ClickUpConnector {
    team_id: String,
    token: String,
}

impl ClickUpConnector {
    /// FAIL-CLOSED gate — see the module doc. A Keychain error degrades to `None`, never a crash.
    pub fn from_config_if_available(config: &AppConfig) -> Option<Self> {
        if !config.clickup_enabled || !config.clickup_consented {
            return None;
        }
        let team_id = config.clickup_team_id.trim().to_string();
        if team_id.is_empty() {
            return None;
        }
        let token = crate::secrets::get_secret(CLICKUP_TOKEN_ACCOUNT)
            .ok()
            .flatten()
            .filter(|k| !k.trim().is_empty())?;
        Some(Self { team_id, token })
    }

    /// Parse a filtered-team-tasks JSON body and keep the tasks matching `query`. Pulled out so it is
    /// unit-testable with a fixture and NO network. Missing `tasks` → empty (clean "no results"). A
    /// task without an id is skipped.
    pub(crate) fn parse_results(body: &str, query: &str) -> ConnectorResult {
        let parsed: ClickUpTasksResponse = serde_json::from_str(body)
            .map_err(|e| ConnectorError::Failed(format!("clickup response parse: {e}")))?;
        let terms = query_terms(query);
        let hits = parsed
            .tasks
            .into_iter()
            .take(PAGE_LIMIT)
            .filter_map(|t| {
                if t.id.trim().is_empty() {
                    return None;
                }
                if !matches_query(&t, &terms) {
                    return None;
                }
                let mut name = t.name.unwrap_or_default().trim().to_string();
                if name.is_empty() {
                    name = "(untitled task)".to_string();
                }
                if name.chars().count() > TITLE_MAX {
                    name = name.chars().take(TITLE_MAX).collect::<String>() + "…";
                }
                let mut parts: Vec<String> = Vec::new();
                if let Some(s) = t.status.as_ref().and_then(|s| s.status.as_deref()) {
                    parts.push(format!("Status: {s}"));
                }
                if let Some(l) = t.list.as_ref().and_then(|l| l.name.as_deref()) {
                    parts.push(format!("List: {l}"));
                }
                let assignees: Vec<&str> = t
                    .assignees
                    .iter()
                    .filter_map(|a| a.username.as_deref())
                    .filter(|u| !u.trim().is_empty())
                    .collect();
                if !assignees.is_empty() {
                    parts.push(format!("Assignee: {}", assignees.join(", ")));
                }
                if let Some(due) = epoch_ms_date(&t.due_date) {
                    parts.push(format!("Due: {due}"));
                }
                Some(ConnectorHit {
                    title: name,
                    snippet: parts.join(" · "),
                    url: t.url.unwrap_or_default(),
                    source_label: SOURCE_LABEL.to_string(),
                })
            })
            .take(MAX_HITS)
            .collect();
        Ok(hits)
    }
}

#[async_trait]
impl Connector for ClickUpConnector {
    fn id(&self) -> &str {
        "clickup"
    }

    fn egress_class(&self) -> EgressClass {
        EgressClass::External
    }

    fn egress_attribution(&self) -> (&'static str, &'static str) {
        ("clickup_search", "ClickUp (connector)")
    }

    async fn search(&self, redacted_query: &str) -> ConnectorResult {
        let q = redacted_query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let client = super::http_client();
        let resp = client
            .get(format!(
                "https://api.clickup.com/api/v2/team/{}/task",
                self.team_id
            ))
            .query(&[
                ("page", "0"),
                ("order_by", "updated"),
                ("reverse", "true"),
                ("subtasks", "true"),
                ("include_closed", "false"),
            ])
            // ClickUp personal tokens go in a BARE Authorization header (no "Bearer " prefix).
            .header("Authorization", &self.token)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| ConnectorError::Failed(format!("clickup request: {}", e.without_url())))?;
        let status = resp.status();
        if !status.is_success() {
            tracing::warn!(target: "connector", provider = "clickup", status = status.as_u16(), "clickup search HTTP error");
            return Err(ConnectorError::Failed(format!("clickup HTTP {status}")));
        }
        let text = resp
            .text()
            .await
            .map_err(|e| ConnectorError::Failed(format!("clickup body: {}", e.without_url())))?;
        let hits = Self::parse_results(&text, q)?;
        tracing::info!(target: "connector", provider = "clickup", hits = hits.len(), "clickup search returned");
        Ok(hits)
    }
}

/// Lowercased, de-punctuated query terms. An all-punctuation query yields no terms, which
/// [`matches_query`] treats as "match nothing" — never "match everything" (a fail-closed default
/// mirroring the `query_database` filter grammar).
fn query_terms(query: &str) -> Vec<String> {
    query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect()
}

/// AND over the query terms against the task's own text (name, body text, list, status). An empty
/// term list matches NOTHING.
fn matches_query(task: &ClickUpTask, terms: &[String]) -> bool {
    if terms.is_empty() {
        return false;
    }
    let mut hay = String::new();
    for s in [
        task.name.as_deref(),
        task.text_content.as_deref(),
        task.description.as_deref(),
        task.list.as_ref().and_then(|l| l.name.as_deref()),
        task.status.as_ref().and_then(|s| s.status.as_deref()),
    ]
    .into_iter()
    .flatten()
    {
        hay.push_str(&s.to_lowercase());
        hay.push(' ');
    }
    terms.iter().all(|t| hay.contains(t.as_str()))
}

/// ClickUp renders epoch MILLISECONDS, sometimes as a JSON string and sometimes as a number →
/// `YYYY-MM-DD` (UTC). `None` when absent/unparseable.
fn epoch_ms_date(v: &serde_json::Value) -> Option<String> {
    let ms = match v {
        serde_json::Value::String(s) => s.trim().parse::<i64>().ok()?,
        serde_json::Value::Number(n) => n.as_i64()?,
        _ => return None,
    };
    chrono::DateTime::from_timestamp_millis(ms).map(|d| d.format("%Y-%m-%d").to_string())
}

/// Only the fields we consume; `#[serde(default)]` everywhere so a missing field never fails.
#[derive(Debug, Deserialize)]
struct ClickUpTasksResponse {
    #[serde(default)]
    tasks: Vec<ClickUpTask>,
}

#[derive(Debug, Deserialize)]
struct ClickUpTask {
    #[serde(default)]
    id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    text_content: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    status: Option<ClickUpStatus>,
    #[serde(default)]
    list: Option<ClickUpList>,
    #[serde(default)]
    assignees: Vec<ClickUpAssignee>,
    /// Epoch ms, string OR number depending on the field — kept raw so neither shape fails the parse.
    #[serde(default)]
    due_date: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct ClickUpStatus {
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClickUpList {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClickUpAssignee {
    #[serde(default)]
    username: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A canned ClickUp "get filtered team tasks" body: two matching tasks, one non-matching, one
    /// id-less.
    const FIXTURE: &str = r#"{
        "tasks": [
            {
                "id": "abc1",
                "name": "Fix the login flow",
                "text_content": "Users get logged out after refresh.",
                "url": "https://app.clickup.com/t/abc1",
                "status": {"status": "in progress"},
                "list": {"name": "Sprint 12"},
                "assignees": [{"username": "Anna"}],
                "due_date": "1782864000000"
            },
            {
                "id": "abc2",
                "name": "Login spike",
                "url": "https://app.clickup.com/t/abc2",
                "status": {"status": "done"},
                "assignees": [],
                "due_date": null
            },
            {
                "id": "abc3",
                "name": "Rewrite the invoicing docs",
                "url": "https://app.clickup.com/t/abc3",
                "status": {"status": "open"}
            },
            { "id": "", "name": "login ghost" }
        ],
        "last_page": true
    }"#;

    #[test]
    fn clickup_parser_maps_json_to_hits() {
        let hits = ClickUpConnector::parse_results(FIXTURE, "login").unwrap();
        assert_eq!(
            hits.len(),
            2,
            "only the matching, id-carrying tasks survive: {hits:?}"
        );

        assert_eq!(hits[0].title, "Fix the login flow");
        assert_eq!(
            hits[0].snippet,
            "Status: in progress · List: Sprint 12 · Assignee: Anna · Due: 2026-07-01"
        );
        assert_eq!(hits[0].url, "https://app.clickup.com/t/abc1");
        assert_eq!(hits[0].source_label, "clickup · task");

        assert_eq!(hits[1].title, "Login spike");
        assert_eq!(hits[1].snippet, "Status: done", "no list/assignee/due");
    }

    /// The local match is AND-over-terms and case-insensitive, and it reads the task BODY too — so a
    /// term that appears only in `text_content` still matches.
    #[test]
    fn clickup_match_is_and_over_terms_case_insensitive_and_reads_the_body() {
        let hits = ClickUpConnector::parse_results(FIXTURE, "Logged OUT").unwrap();
        assert_eq!(hits.len(), 1, "body-only terms match: {hits:?}");
        assert_eq!(hits[0].title, "Fix the login flow");

        // AND: "login" matches two tasks, "login invoicing" matches none.
        assert!(ClickUpConnector::parse_results(FIXTURE, "login invoicing")
            .unwrap()
            .is_empty());
    }

    /// FAIL-CLOSED matching: a query with no usable terms matches NOTHING (never every task).
    #[test]
    fn clickup_empty_or_punctuation_query_matches_nothing() {
        assert!(ClickUpConnector::parse_results(FIXTURE, "   ")
            .unwrap()
            .is_empty());
        assert!(ClickUpConnector::parse_results(FIXTURE, "--- ???")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn clickup_parser_tolerates_missing_fields_and_empty() {
        assert!(ClickUpConnector::parse_results("{}", "x")
            .unwrap()
            .is_empty());
        assert!(ClickUpConnector::parse_results(r#"{"tasks":[]}"#, "x")
            .unwrap()
            .is_empty());
        // No name → placeholder title; no url → empty string; numeric due_date parses too.
        let body = r#"{"tasks":[{"id":"t1","text_content":"widget","due_date":1782864000000}]}"#;
        let hits = ClickUpConnector::parse_results(body, "widget").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "(untitled task)");
        assert_eq!(hits[0].snippet, "Due: 2026-07-01");
        assert_eq!(hits[0].url, "");
    }

    #[test]
    fn clickup_parser_caps_the_hit_count() {
        let tasks: Vec<String> = (0..50)
            .map(|i| format!(r#"{{"id":"t{i}","name":"login task {i}"}}"#))
            .collect();
        let body = format!(r#"{{"tasks":[{}]}}"#, tasks.join(","));
        let hits = ClickUpConnector::parse_results(&body, "login").unwrap();
        assert_eq!(hits.len(), MAX_HITS);
    }

    #[test]
    fn clickup_parser_truncates_absurd_names() {
        let long = "login".to_string() + &"x".repeat(1000);
        let body = format!(r#"{{"tasks":[{{"id":"t1","name":"{long}"}}]}}"#);
        let hits = ClickUpConnector::parse_results(&body, "login").unwrap();
        assert!(hits[0].title.chars().count() <= TITLE_MAX + 1);
        assert!(hits[0].title.ends_with('…'));
    }

    #[test]
    fn clickup_parser_rejects_malformed_json() {
        assert!(matches!(
            ClickUpConnector::parse_results("nope", "x"),
            Err(ConnectorError::Failed(_))
        ));
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    /// The connector declares the EXTERNAL egress class and its OWN truthful ledger attribution —
    /// a ClickUp egress must never be recorded as a web search (mirrors the Jira/Slack invariant).
    #[test]
    fn clickup_declares_external_egress_and_truthful_attribution() {
        let c = ClickUpConnector {
            team_id: "9001".to_string(),
            token: "pk_t".to_string(),
        };
        assert_eq!(c.id(), "clickup");
        assert_eq!(c.egress_class(), EgressClass::External);
        assert_eq!(
            c.egress_attribution(),
            ("clickup_search", "ClickUp (connector)")
        );
        assert_ne!(c.egress_attribution().0, "web_search");
    }

    /// An empty (post-redaction) query short-circuits to zero hits WITHOUT any network call — the
    /// test would hang/fail if a request were attempted against the fake token.
    #[test]
    fn clickup_empty_query_egresses_nothing() {
        let c = ClickUpConnector {
            team_id: "9001".to_string(),
            token: "pk_t".to_string(),
        };
        assert!(block_on(c.search("   ")).unwrap().is_empty());
    }

    #[test]
    fn from_config_fail_closed_when_disabled_unconsented_or_unconfigured() {
        // Default config: everything off → None, and no Keychain read is even attempted (enable /
        // consent are checked first).
        let cfg = AppConfig::default();
        assert!(ClickUpConnector::from_config_if_available(&cfg).is_none());
        let cfg = AppConfig {
            clickup_enabled: true,
            ..AppConfig::default()
        };
        assert!(
            ClickUpConnector::from_config_if_available(&cfg).is_none(),
            "unconsented"
        );
        let cfg = AppConfig {
            clickup_enabled: true,
            clickup_consented: true,
            clickup_team_id: String::new(),
            ..AppConfig::default()
        };
        assert!(
            ClickUpConnector::from_config_if_available(&cfg).is_none(),
            "no team id"
        );
        let cfg = AppConfig {
            clickup_enabled: false,
            clickup_consented: true,
            clickup_team_id: "9001".into(),
            ..AppConfig::default()
        };
        assert!(
            ClickUpConnector::from_config_if_available(&cfg).is_none(),
            "consented but disabled"
        );
    }
}
