//! JIRA connector — live, on-demand issue search via the Jira Cloud REST API (brain2 connectors,
//! Phase 2 of docs/research/2026-07-05-connectors-live-vs-rag.md).
//!
//! ## Egress posture (NEW EGRESS CLASS INSTANCE — audited by the lock-security reviewer)
//! [`EgressClass::External`]. Exposed to the brain ONLY when ALL of:
//! - `config.jira_enabled` (master toggle), AND
//! - `config.jira_consented` (one-time consent, preserve-only, flipped solely by `consent_to_jira`), AND
//! - `config.jira_base_url` + `config.jira_email` are non-empty, AND
//! - an API token is present in the Keychain (`jira_api_token`).
//! Otherwise [`JiraConnector::from_config_if_available`] returns `None` (fail-closed: the tool does
//! not exist for the session). The framework redacts the query BEFORE it reaches [`Connector::search`].
//!
//! ## Endpoint
//! `POST {base}/rest/api/3/search/jql` with `{"jql": "text ~ \"…\" ORDER BY updated DESC", …}` and
//! HTTP Basic auth (`email:api_token`). NOTE: the legacy `/rest/api/3/search` endpoint was REMOVED
//! (HTTP 410) — do not use it.
//!
//! ## No PII in logs
//! Logs carry connector id + hit count + HTTP status only — never the JQL, summaries, or the token.

use async_trait::async_trait;
use serde::Deserialize;

use super::{Connector, ConnectorError, ConnectorHit, ConnectorResult, EgressClass};
use crate::settings::AppConfig;

/// Keychain account holding the BYO Jira API token. NEVER logged / NEVER sent to the FE.
pub const JIRA_TOKEN_ACCOUNT: &str = "jira_api_token";

/// Loud attribution label on every hit.
const SOURCE_LABEL: &str = "Jira";

/// Escape a user query for embedding inside a JQL quoted string: backslashes then double quotes.
pub(crate) fn escape_jql(q: &str) -> String {
    q.replace('\\', "\\\\").replace('"', "\\\"")
}

pub struct JiraConnector {
    base_url: String,
    email: String,
    api_token: String,
}

impl JiraConnector {
    /// FAIL-CLOSED gate — see the module doc. A Keychain error degrades to `None`, never a crash.
    pub fn from_config_if_available(config: &AppConfig) -> Option<Self> {
        if !config.jira_enabled || !config.jira_consented {
            return None;
        }
        let base_url = config.jira_base_url.trim().trim_end_matches('/').to_string();
        let email = config.jira_email.trim().to_string();
        if base_url.is_empty() || email.is_empty() {
            return None;
        }
        let token = crate::secrets::get_secret(JIRA_TOKEN_ACCOUNT)
            .ok()
            .flatten()
            .filter(|k| !k.trim().is_empty())?;
        Some(Self {
            base_url,
            email,
            api_token: token,
        })
    }

    /// Parse a `/rest/api/3/search/jql` JSON body into [`ConnectorHit`]s. Pulled out so it is
    /// unit-testable with a fixture and NO network. Missing `issues` → empty (clean "no results").
    /// An issue without a key is skipped.
    pub(crate) fn parse_results(body: &str, base_url: &str) -> ConnectorResult {
        let parsed: JiraSearchResponse = serde_json::from_str(body)
            .map_err(|e| ConnectorError::Failed(format!("jira response parse: {e}")))?;
        let hits = parsed
            .issues
            .into_iter()
            .filter_map(|i| {
                let key = i.key.trim().to_string();
                if key.is_empty() {
                    return None;
                }
                let f = i.fields;
                let summary = f.summary.unwrap_or_default();
                let mut parts: Vec<String> = Vec::new();
                if let Some(s) = f.status.and_then(|s| s.name) {
                    parts.push(format!("Status: {s}"));
                }
                if let Some(a) = f.assignee.and_then(|a| a.display_name) {
                    parts.push(format!("Assignee: {a}"));
                }
                if let Some(d) = f.duedate {
                    parts.push(format!("Due: {d}"));
                }
                Some(ConnectorHit {
                    title: format!("{key} — {}", summary.trim()),
                    snippet: parts.join(" · "),
                    url: format!("{base_url}/browse/{key}"),
                    source_label: SOURCE_LABEL.to_string(),
                })
            })
            .collect();
        Ok(hits)
    }
}

#[async_trait]
impl Connector for JiraConnector {
    fn id(&self) -> &str {
        "jira"
    }

    fn egress_class(&self) -> EgressClass {
        EgressClass::External
    }

    async fn search(&self, redacted_query: &str) -> ConnectorResult {
        let q = redacted_query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let jql = format!("text ~ \"{}\" ORDER BY updated DESC", escape_jql(q));
        let body = serde_json::json!({
            "jql": jql,
            "maxResults": 8,
            "fields": ["summary", "status", "assignee", "duedate"],
        });
        let client = reqwest::Client::new();
        let resp = client
            .post(format!("{}/rest/api/3/search/jql", self.base_url))
            .basic_auth(&self.email, Some(&self.api_token))
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ConnectorError::Failed(format!("jira request: {}", e.without_url())))?;
        let status = resp.status();
        if !status.is_success() {
            tracing::warn!(target: "connector", provider = "jira", status = status.as_u16(), "jira search HTTP error");
            return Err(ConnectorError::Failed(format!("jira HTTP {status}")));
        }
        let text = resp
            .text()
            .await
            .map_err(|e| ConnectorError::Failed(format!("jira body: {}", e.without_url())))?;
        let hits = Self::parse_results(&text, &self.base_url)?;
        tracing::info!(target: "connector", provider = "jira", hits = hits.len(), "jira search returned");
        Ok(hits)
    }
}

/// Only the fields we consume; `#[serde(default)]` everywhere so a missing field never fails.
#[derive(Debug, Deserialize)]
struct JiraSearchResponse {
    #[serde(default)]
    issues: Vec<JiraIssue>,
}

#[derive(Debug, Deserialize)]
struct JiraIssue {
    #[serde(default)]
    key: String,
    #[serde(default)]
    fields: JiraFields,
}

#[derive(Debug, Deserialize, Default)]
struct JiraFields {
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    status: Option<JiraStatus>,
    #[serde(default)]
    assignee: Option<JiraAssignee>,
    #[serde(default)]
    duedate: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JiraStatus {
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct JiraAssignee {
    #[serde(default, rename = "displayName")]
    display_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jira_parser_maps_json_to_hits() {
        let body = r#"{
            "issues": [
                {"key":"PROJ-123","fields":{"summary":"Fix login flow","status":{"name":"In Progress"},"assignee":{"displayName":"Anna"},"duedate":"2026-07-10"}},
                {"key":"PROJ-9","fields":{"summary":"Spike","status":{"name":"Done"}}}
            ],
            "nextPageToken": "abc"
        }"#;
        let hits = JiraConnector::parse_results(body, "https://acme.atlassian.net").unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "PROJ-123 — Fix login flow");
        assert_eq!(hits[0].snippet, "Status: In Progress · Assignee: Anna · Due: 2026-07-10");
        assert_eq!(hits[0].url, "https://acme.atlassian.net/browse/PROJ-123");
        assert_eq!(hits[0].source_label, "Jira");
        assert_eq!(hits[1].snippet, "Status: Done");
    }

    #[test]
    fn jira_parser_tolerates_missing_fields_and_empty() {
        assert!(JiraConnector::parse_results(r#"{}"#, "https://x").unwrap().is_empty());
        assert!(JiraConnector::parse_results(r#"{"issues":[]}"#, "https://x").unwrap().is_empty());
        // Missing key → skipped; missing fields → title still renders.
        let body = r#"{"issues":[{"key":"","fields":{}},{"key":"A-1","fields":{}}]}"#;
        let hits = JiraConnector::parse_results(body, "https://x").unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "A-1 — ");
    }

    #[test]
    fn jira_parser_rejects_malformed_json() {
        assert!(matches!(
            JiraConnector::parse_results("not json", "https://x"),
            Err(ConnectorError::Failed(_))
        ));
    }

    #[test]
    fn jql_escaping_neutralizes_quotes_and_backslashes() {
        assert_eq!(escape_jql(r#"login "bug" \ test"#), r#"login \"bug\" \\ test"#);
    }

    #[test]
    fn from_config_fail_closed_when_disabled_or_unconsented_or_unconfigured() {
        // Default config: everything off → None, and no Keychain read is even attempted for
        // the disabled cases (enable/consent are checked first).
        let cfg = AppConfig::default();
        assert!(JiraConnector::from_config_if_available(&cfg).is_none());
        let cfg = AppConfig { jira_enabled: true, ..AppConfig::default() };
        assert!(JiraConnector::from_config_if_available(&cfg).is_none(), "unconsented");
        let cfg = AppConfig {
            jira_enabled: true,
            jira_consented: true,
            jira_base_url: String::new(),
            jira_email: "a@b.c".into(),
            ..AppConfig::default()
        };
        assert!(JiraConnector::from_config_if_available(&cfg).is_none(), "no base url");
    }
}
