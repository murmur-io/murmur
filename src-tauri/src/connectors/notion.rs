//! NOTION connector — live, on-demand READ-ONLY page/database search via the Notion REST API.
//! Mirrors `connectors::jira` / `connectors::slack` exactly (they are the proven pattern); every
//! framework discipline (consent gate, redaction firewall, content-free egress ledger) is INHERITED
//! from [`crate::connectors::ConnectorRegistry`], never re-implemented here.
//!
//! ## Egress posture (NEW EGRESS CLASS INSTANCE — audited by the lock-security reviewer)
//! [`EgressClass::External`]. Exposed to the brain ONLY when ALL of:
//! - `config.notion_enabled` (master toggle), AND
//! - `config.notion_consented` (one-time consent, preserve-only, flipped solely by `consent_to_notion`), AND
//! - an integration token is present in the Keychain (`notion_api_token`).
//!
//! Otherwise [`NotionConnector::from_config_if_available`] returns `None` (fail-closed: the tool does
//! not exist for the session). The framework redacts the query BEFORE it reaches [`Connector::search`].
//!
//! ## Endpoint — ONE READ, never a write
//! `POST https://api.notion.com/v1/search` with `Authorization: Bearer <internal integration token>`
//! and `Notion-Version: 2022-06-28`. The POST verb is Notion's shape for a SEARCH QUERY — it creates
//! and mutates NOTHING. This connector exposes no create/update/delete path at all: the only Notion
//! call it can ever make is this single search read.
//!
//! ## No PII in logs
//! Logs carry connector id + hit count + HTTP status only — never the query, page titles, or the token.

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::Deserialize;

use super::{Connector, ConnectorError, ConnectorHit, ConnectorResult, EgressClass};
use crate::settings::AppConfig;

/// Keychain account holding the BYO Notion internal-integration token. NEVER logged / NEVER FE-exposed.
pub const NOTION_TOKEN_ACCOUNT: &str = "notion_api_token";

/// The pinned Notion API version. Notion REQUIRES this header on every request; pinning it means a
/// future default version bump can never silently change the response shape this parser expects.
const NOTION_VERSION: &str = "2022-06-28";

/// Max results asked of Notion (and therefore max hits) — the same budget the sibling connectors use
/// so one connector can never blow the agent-loop tool budget.
const PAGE_SIZE: usize = 8;

/// Cap on the rendered title so one absurdly long page name can't dominate the tool result.
const TITLE_MAX: usize = 200;

/// LOUD per-object-kind attribution. Deliberately a FIXED `&'static str` chosen from the object
/// kind — never server-supplied text — so the attribution field itself carries no untrusted input.
fn source_label_for(object: &str) -> &'static str {
    match object {
        "page" => "notion · page",
        "database" => "notion · database",
        _ => "notion",
    }
}

pub struct NotionConnector {
    token: String,
}

impl NotionConnector {
    /// FAIL-CLOSED gate — see the module doc. A Keychain error degrades to `None`, never a crash.
    pub fn from_config_if_available(config: &AppConfig) -> Option<Self> {
        if !config.notion_enabled || !config.notion_consented {
            return None;
        }
        let token = crate::secrets::get_secret(NOTION_TOKEN_ACCOUNT)
            .ok()
            .flatten()
            .filter(|k| !k.trim().is_empty())?;
        Some(Self { token })
    }

    /// Parse a `POST /v1/search` JSON body into [`ConnectorHit`]s. Pulled out so it is unit-testable
    /// with a fixture and NO network. Missing `results` → empty (clean "no results"). An object
    /// without an id is skipped.
    pub(crate) fn parse_results(body: &str) -> ConnectorResult {
        let parsed: NotionSearchResponse = serde_json::from_str(body)
            .map_err(|e| ConnectorError::Failed(format!("notion response parse: {e}")))?;
        let hits = parsed
            .results
            .into_iter()
            .filter_map(|o| {
                if o.id.trim().is_empty() {
                    return None;
                }
                let mut title = title_of(&o);
                if title.is_empty() {
                    title = "(untitled)".to_string();
                }
                if title.chars().count() > TITLE_MAX {
                    title = title.chars().take(TITLE_MAX).collect::<String>() + "…";
                }
                // An unknown/absent object kind renders generically — never raw server text.
                let kind: &str = match o.object.as_str() {
                    "page" => "Page",
                    "database" => "Database",
                    _ => "Notion item",
                };
                let mut parts: Vec<String> = vec![kind.to_string()];
                if let Some(edited) = o.last_edited_time.as_deref().and_then(date_part) {
                    parts.push(format!("Last edited: {edited}"));
                }
                if o.archived {
                    parts.push("Archived".to_string());
                }
                Some(ConnectorHit {
                    title,
                    snippet: parts.join(" · "),
                    url: o.url.unwrap_or_default(),
                    source_label: source_label_for(&o.object).to_string(),
                })
            })
            .collect();
        Ok(hits)
    }
}

#[async_trait]
impl Connector for NotionConnector {
    fn id(&self) -> &str {
        "notion"
    }

    fn egress_class(&self) -> EgressClass {
        EgressClass::External
    }

    fn egress_attribution(&self) -> (&'static str, &'static str) {
        ("notion_search", "Notion (connector)")
    }

    async fn search(&self, redacted_query: &str) -> ConnectorResult {
        let q = redacted_query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let body = serde_json::json!({
            "query": q,
            "page_size": PAGE_SIZE,
            "sort": { "direction": "descending", "timestamp": "last_edited_time" },
        });
        let client = super::http_client();
        let resp = client
            .post("https://api.notion.com/v1/search")
            .bearer_auth(&self.token)
            .header("Notion-Version", NOTION_VERSION)
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| ConnectorError::Failed(format!("notion request: {}", e.without_url())))?;
        let status = resp.status();
        if !status.is_success() {
            tracing::warn!(target: "connector", provider = "notion", status = status.as_u16(), "notion search HTTP error");
            return Err(ConnectorError::Failed(format!("notion HTTP {status}")));
        }
        let text = resp
            .text()
            .await
            .map_err(|e| ConnectorError::Failed(format!("notion body: {}", e.without_url())))?;
        let hits = Self::parse_results(&text)?;
        tracing::info!(target: "connector", provider = "notion", hits = hits.len(), "notion search returned");
        Ok(hits)
    }
}

/// The best title for a search result: a DATABASE carries a top-level `title` rich-text array; a
/// PAGE carries it on whichever property has `"type": "title"`. Falls back across both shapes so an
/// unusual object still renders a name when one exists.
fn title_of(o: &NotionObject) -> String {
    let from_page = o
        .properties
        .values()
        .find(|p| p.kind == "title")
        .map(|p| rich_text_plain(&p.title))
        .unwrap_or_default();
    let title = if from_page.trim().is_empty() {
        rich_text_plain(&o.title)
    } else {
        from_page
    };
    title.trim().to_string()
}

/// Concatenate the `plain_text` of a Notion rich-text array. Kept `Value`-based (rather than a typed
/// `Vec`) so a differently-shaped `title` key anywhere in `properties` can never fail the WHOLE
/// response parse — a malformed corner degrades to an empty title, never a dropped search.
fn rich_text_plain(v: &serde_json::Value) -> String {
    v.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.get("plain_text").and_then(|p| p.as_str()))
                .collect::<String>()
        })
        .unwrap_or_default()
}

/// `2026-07-01T10:00:00.000Z` → `2026-07-01`. `None` when the value is not an ISO-8601 timestamp.
fn date_part(ts: &str) -> Option<&str> {
    let d = ts.split('T').next()?;
    (d.len() == 10 && d.bytes().all(|b| b.is_ascii_digit() || b == b'-')).then_some(d)
}

/// Only the fields we consume; `#[serde(default)]` everywhere so a missing field never fails.
#[derive(Debug, Deserialize)]
struct NotionSearchResponse {
    #[serde(default)]
    results: Vec<NotionObject>,
}

#[derive(Debug, Deserialize)]
struct NotionObject {
    #[serde(default)]
    object: String,
    #[serde(default)]
    id: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    archived: bool,
    #[serde(default)]
    last_edited_time: Option<String>,
    /// Database-level title rich text (absent on pages). Raw `Value` — see [`rich_text_plain`].
    #[serde(default)]
    title: serde_json::Value,
    /// Page properties, keyed by property name. `BTreeMap` (not `HashMap`) so title resolution is
    /// DETERMINISTIC when a page somehow carries more than one title-typed property.
    #[serde(default)]
    properties: BTreeMap<String, NotionProperty>,
}

#[derive(Debug, Deserialize, Default)]
struct NotionProperty {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    title: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A canned Notion `POST /v1/search` body: one page, one database, one id-less object.
    const FIXTURE: &str = r#"{
        "object": "list",
        "results": [
            {
                "object": "page",
                "id": "p-1",
                "url": "https://www.notion.so/Q3-Roadmap-p1",
                "archived": false,
                "last_edited_time": "2026-07-01T10:00:00.000Z",
                "properties": {
                    "Owner": { "id": "abc", "type": "rich_text", "rich_text": [{"plain_text": "Anna"}] },
                    "Name": { "id": "title", "type": "title", "title": [{"plain_text": "Q3 "}, {"plain_text": "Roadmap"}] }
                }
            },
            {
                "object": "database",
                "id": "d-1",
                "url": "https://www.notion.so/db1",
                "archived": true,
                "last_edited_time": "2026-06-30T08:00:00.000Z",
                "title": [{"plain_text": "Engineering tasks"}]
            },
            { "object": "page", "id": "", "url": "https://www.notion.so/skipme" }
        ],
        "has_more": false
    }"#;

    #[test]
    fn notion_parser_maps_json_to_hits() {
        let hits = NotionConnector::parse_results(FIXTURE).unwrap();
        assert_eq!(hits.len(), 2, "the id-less object is skipped");

        assert_eq!(hits[0].title, "Q3 Roadmap");
        assert_eq!(hits[0].snippet, "Page · Last edited: 2026-07-01");
        assert_eq!(hits[0].url, "https://www.notion.so/Q3-Roadmap-p1");
        assert_eq!(hits[0].source_label, "notion · page");

        assert_eq!(hits[1].title, "Engineering tasks");
        assert_eq!(
            hits[1].snippet,
            "Database · Last edited: 2026-06-30 · Archived"
        );
        assert_eq!(hits[1].source_label, "notion · database");
    }

    #[test]
    fn notion_parser_tolerates_missing_fields_and_empty() {
        assert!(NotionConnector::parse_results("{}").unwrap().is_empty());
        assert!(NotionConnector::parse_results(r#"{"results":[]}"#)
            .unwrap()
            .is_empty());
        // No title anywhere → "(untitled)"; no url → empty string; unknown object kind → generic.
        let hits =
            NotionConnector::parse_results(r#"{"results":[{"object":"block","id":"b-1"}]}"#)
                .unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "(untitled)");
        assert_eq!(hits[0].snippet, "Notion item");
        assert_eq!(hits[0].url, "");
        assert_eq!(hits[0].source_label, "notion");
    }

    /// A `title` key with an UNEXPECTED shape on some other property must not fail the whole parse
    /// (the raw-`Value` choice in [`NotionProperty`]) — the object still renders.
    #[test]
    fn notion_parser_survives_oddly_shaped_title_keys() {
        let body = r#"{"results":[{"object":"page","id":"p-2","properties":{
            "Weird": {"type":"rollup","title":"not-an-array"},
            "Name": {"type":"title","title":[{"plain_text":"Real title"}]}
        }}]}"#;
        let hits = NotionConnector::parse_results(body).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Real title");
    }

    #[test]
    fn notion_parser_truncates_absurd_titles() {
        let long = "x".repeat(1000);
        let body = format!(
            r#"{{"results":[{{"object":"page","id":"p-3","properties":{{"Name":{{"type":"title","title":[{{"plain_text":"{long}"}}]}}}}}}]}}"#
        );
        let hits = NotionConnector::parse_results(&body).unwrap();
        assert!(hits[0].title.chars().count() <= TITLE_MAX + 1);
        assert!(hits[0].title.ends_with('…'));
    }

    #[test]
    fn notion_parser_rejects_malformed_json() {
        assert!(matches!(
            NotionConnector::parse_results("not json"),
            Err(ConnectorError::Failed(_))
        ));
    }

    #[test]
    fn notion_ignores_a_bogus_last_edited_time() {
        let body = r#"{"results":[{"object":"page","id":"p-4","last_edited_time":"yesterday"}]}"#;
        let hits = NotionConnector::parse_results(body).unwrap();
        assert_eq!(hits[0].snippet, "Page", "an unparseable timestamp is dropped");
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    /// The connector declares the EXTERNAL egress class and its OWN truthful ledger attribution —
    /// a Notion egress must never be recorded as a web search (mirrors the Jira/Slack invariant).
    #[test]
    fn notion_declares_external_egress_and_truthful_attribution() {
        let c = NotionConnector {
            token: "t".to_string(),
        };
        assert_eq!(c.id(), "notion");
        assert_eq!(c.egress_class(), EgressClass::External);
        assert_eq!(
            c.egress_attribution(),
            ("notion_search", "Notion (connector)")
        );
        assert_ne!(c.egress_attribution().0, "web_search");
    }

    /// An empty (post-redaction) query short-circuits to zero hits WITHOUT any network call — the
    /// test would hang/fail if a request were attempted against the fake token.
    #[test]
    fn notion_empty_query_egresses_nothing() {
        let c = NotionConnector {
            token: "t".to_string(),
        };
        assert!(block_on(c.search("   ")).unwrap().is_empty());
    }

    #[test]
    fn from_config_fail_closed_when_disabled_or_unconsented() {
        // Default config: everything off → None, and no Keychain read is even attempted (enable /
        // consent are checked first).
        let cfg = AppConfig::default();
        assert!(NotionConnector::from_config_if_available(&cfg).is_none());
        let cfg = AppConfig {
            notion_enabled: true,
            ..AppConfig::default()
        };
        assert!(
            NotionConnector::from_config_if_available(&cfg).is_none(),
            "unconsented"
        );
        let cfg = AppConfig {
            notion_enabled: false,
            notion_consented: true,
            ..AppConfig::default()
        };
        assert!(
            NotionConnector::from_config_if_available(&cfg).is_none(),
            "consented but disabled"
        );
    }
}
