//! SLACK connector — live, on-demand message search via the Slack Web API (brain2 connectors,
//! Phase 3 of docs/research/2026-07-05-connectors-live-vs-rag.md).
//!
//! ## Egress posture (NEW EGRESS CLASS INSTANCE — audited by the lock-security reviewer)
//! [`EgressClass::External`]. Exposed ONLY when `slack_enabled && slack_consented && a user token
//! is in the Keychain` — otherwise absent (fail-closed). The framework redacts the query first.
//!
//! ## Endpoint + token model
//! `GET https://slack.com/api/search.messages?query=…&count=8` with `Authorization: Bearer xoxp-…`.
//! `search.messages` requires a USER token (`xoxp-`, scope `search:read`) — a bot token cannot
//! search. The token is BYO: the user creates a single-workspace app, installs it, pastes the token.
//! QUIRK: Slack answers HTTP 200 with `{"ok":false,"error":"…"}` on failure — `parse_results`
//! checks `ok` and surfaces the error code (non-PII) as a Failed.
//!
//! ## No PII in logs
//! Connector id + hit count + HTTP status / Slack error CODE only — never queries, message text,
//! channel names, or the token.

use async_trait::async_trait;
use serde::Deserialize;

use super::{Connector, ConnectorError, ConnectorHit, ConnectorResult, EgressClass};
use crate::settings::AppConfig;

/// Keychain account holding the BYO Slack user token (`xoxp-…`). NEVER logged / NEVER FE-exposed.
pub const SLACK_TOKEN_ACCOUNT: &str = "slack_user_token";

const SOURCE_LABEL: &str = "Slack";

/// Cap a message snippet so one long Slack message can't blow the tool budget.
const SNIPPET_MAX: usize = 300;

pub struct SlackConnector {
    token: String,
}

impl SlackConnector {
    /// FAIL-CLOSED gate — see module doc. Keychain error degrades to `None`.
    pub fn from_config_if_available(config: &AppConfig) -> Option<Self> {
        if !config.slack_enabled || !config.slack_consented {
            return None;
        }
        let token = crate::secrets::get_secret(SLACK_TOKEN_ACCOUNT)
            .ok()
            .flatten()
            .filter(|k| !k.trim().is_empty())?;
        Some(Self { token })
    }

    /// Parse a `search.messages` body. `ok:false` → Failed carrying ONLY the Slack error CODE.
    pub(crate) fn parse_results(body: &str) -> ConnectorResult {
        let parsed: SlackSearchResponse = serde_json::from_str(body)
            .map_err(|e| ConnectorError::Failed(format!("slack response parse: {e}")))?;
        if !parsed.ok {
            return Err(ConnectorError::Failed(format!(
                "slack error: {}",
                parsed.error.unwrap_or_else(|| "unknown".into())
            )));
        }
        let matches = parsed.messages.map(|m| m.matches).unwrap_or_default();
        let hits = matches
            .into_iter()
            .filter_map(|m| {
                let mut text = m.text.unwrap_or_default().trim().to_string();
                if text.is_empty() {
                    return None;
                }
                if text.chars().count() > SNIPPET_MAX {
                    text = text.chars().take(SNIPPET_MAX).collect::<String>() + "…";
                }
                let channel = m
                    .channel
                    .and_then(|c| c.name)
                    .map(|n| format!("#{n}"))
                    .unwrap_or_else(|| "DM".to_string());
                let who = m.username.unwrap_or_default();
                let title = if who.is_empty() {
                    channel.clone()
                } else {
                    format!("{channel} · @{who}")
                };
                Some(ConnectorHit {
                    title,
                    snippet: text,
                    url: m.permalink.unwrap_or_default(),
                    source_label: SOURCE_LABEL.to_string(),
                })
            })
            .collect();
        Ok(hits)
    }
}

#[async_trait]
impl Connector for SlackConnector {
    fn id(&self) -> &str {
        "slack"
    }

    fn egress_class(&self) -> EgressClass {
        EgressClass::External
    }

    fn egress_attribution(&self) -> (&'static str, &'static str) {
        ("slack_search", "Slack (connector)")
    }

    async fn search(&self, redacted_query: &str) -> ConnectorResult {
        let q = redacted_query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let client = super::http_client();
        let resp = client
            .get("https://slack.com/api/search.messages")
            .query(&[("query", q), ("count", "8")])
            .bearer_auth(&self.token)
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| ConnectorError::Failed(format!("slack request: {}", e.without_url())))?;
        let status = resp.status();
        if !status.is_success() {
            tracing::warn!(target: "connector", provider = "slack", status = status.as_u16(), "slack search HTTP error");
            return Err(ConnectorError::Failed(format!("slack HTTP {status}")));
        }
        let body = resp
            .text()
            .await
            .map_err(|e| ConnectorError::Failed(format!("slack body: {}", e.without_url())))?;
        let hits = Self::parse_results(&body)?;
        tracing::info!(target: "connector", provider = "slack", hits = hits.len(), "slack search returned");
        Ok(hits)
    }
}

#[derive(Debug, Deserialize)]
struct SlackSearchResponse {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    messages: Option<SlackMessages>,
}

#[derive(Debug, Deserialize)]
struct SlackMessages {
    #[serde(default)]
    matches: Vec<SlackMatch>,
}

#[derive(Debug, Deserialize)]
struct SlackMatch {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    permalink: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    channel: Option<SlackChannel>,
}

#[derive(Debug, Deserialize)]
struct SlackChannel {
    #[serde(default)]
    name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slack_parser_maps_matches_to_hits() {
        let body = r#"{
            "ok": true,
            "messages": { "matches": [
                {"text":"We decided to ship Friday","permalink":"https://acme.slack.com/archives/C1/p1","username":"anna","channel":{"name":"eng"}},
                {"text":"","permalink":"https://x"},
                {"text":"No channel or user"}
            ]}
        }"#;
        let hits = SlackConnector::parse_results(body).unwrap();
        assert_eq!(hits.len(), 2, "empty-text match is skipped");
        assert_eq!(hits[0].title, "#eng · @anna");
        assert_eq!(hits[0].snippet, "We decided to ship Friday");
        assert_eq!(hits[0].url, "https://acme.slack.com/archives/C1/p1");
        assert_eq!(hits[0].source_label, "Slack");
        assert_eq!(hits[1].title, "DM");
    }

    #[test]
    fn slack_ok_false_is_failed_with_error_code_only() {
        let body = r#"{"ok": false, "error": "invalid_auth"}"#;
        match SlackConnector::parse_results(body) {
            Err(ConnectorError::Failed(m)) => assert!(m.contains("invalid_auth")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn slack_parser_truncates_long_messages() {
        let long = "x".repeat(1000);
        let body = format!(r#"{{"ok":true,"messages":{{"matches":[{{"text":"{long}"}}]}}}}"#);
        let hits = SlackConnector::parse_results(&body).unwrap();
        assert!(hits[0].snippet.chars().count() <= 301);
        assert!(hits[0].snippet.ends_with('…'));
    }

    #[test]
    fn slack_parser_rejects_malformed_json() {
        assert!(matches!(
            SlackConnector::parse_results("nope"),
            Err(ConnectorError::Failed(_))
        ));
    }

    #[test]
    fn from_config_fail_closed_when_disabled_or_unconsented() {
        let cfg = AppConfig::default();
        assert!(SlackConnector::from_config_if_available(&cfg).is_none());
        let cfg = AppConfig { slack_enabled: true, ..AppConfig::default() };
        assert!(SlackConnector::from_config_if_available(&cfg).is_none(), "unconsented");
    }
}
