//! Brain v2 L5 — the MCP CONNECTOR: one [`McpConnector`] per user-configured external MCP server
//! (`mcp_servers` row), riding the EXISTING connector framework so every discipline is inherited,
//! never re-implemented:
//!
//! - **Consent-gated, fail-closed.** [`McpConnector::from_row_if_available`] returns `None`
//!   unless the row is BOTH `enabled` AND `consented` (per-server consent, default OFF, flipped
//!   only by `consent_to_mcp_server`) — an unconsented server is ABSENT from the registry and the
//!   brain's tool list, and a direct `search` on an unexposed id fails closed with NO egress.
//! - **Redacted.** The framework ([`super::ConnectorRegistry::search`]) scrubs the query through
//!   the FULL redaction firewall (regex + NER names) BEFORE [`Connector::search`] runs — the MCP
//!   server only ever sees the redacted query.
//! - **Ledgered + truthfully attributed.** One content-free egress row per attempt. The row's
//!   `provider_id` is this connector's id (`mcp_<server_id>`), which is the PER-SERVER truthful
//!   attribution; `call_kind`/`destination` are the fixed non-PII labels below (the
//!   `egress_attribution` pair must be `&'static str`, so per-server identity rides
//!   `provider_id`).
//! - **Loud.** Every hit's `source_label` is `mcp · <label>` so an MCP-grounded answer is visibly
//!   attributed to the external server.
//!
//! ## Prompt-injection stance (load-bearing, audited)
//! Server-supplied tool NAMES/DESCRIPTIONS are UNTRUSTED INPUT and are NEVER interpolated into
//! system prompts or the model-facing tool catalog — the advertised `mcp_<id>_query` spec is built
//! in `tools.rs` from the USER-AUTHORED label only (sanitized + capped). Tool discovery
//! (`tools/list`) happens HERE, at call time, purely to pick which server tool to invoke; the
//! server's text reaches the model only as a TOOL RESULT (data, `RESULT_BUDGET`-truncated), which
//! the agentic loop's system prompt already instructs to treat as data, never instructions.
//!
//! ## stdio = code execution
//! A stdio server launches a local binary (absolute path only). The add-server command carries the
//! explicit trust warning; consent is what arms it.

use async_trait::async_trait;

use super::mcp_client::{McpClient, McpToolInfo};
use super::{Connector, ConnectorError, ConnectorHit, ConnectorResult, EgressClass};
use crate::storage::models::McpServer;

/// Cap on the single hit's snippet (the loop re-truncates at `RESULT_BUDGET`; this only bounds
/// connector-side memory).
const SNIPPET_CAP: usize = 4_000;

/// The connector id for a server row: `mcp_<server_id>` (ids are minted hyphen-free).
pub fn connector_id(server_id: &str) -> String {
    format!("mcp_{server_id}")
}

pub struct McpConnector {
    id: String,
    label: String,
    client: McpClient,
}

impl McpConnector {
    /// FAIL-CLOSED gate: `None` unless the row is enabled AND consented AND its transport shape is
    /// valid ([`McpClient::for_server`] — http(s) endpoint / absolute stdio path).
    pub fn from_row_if_available(row: &McpServer) -> Option<Self> {
        if !row.enabled || !row.consented {
            return None;
        }
        let client = McpClient::for_server(row)?;
        Some(Self {
            id: connector_id(&row.id),
            label: row.label.clone(),
            client,
        })
    }
}

/// Pick the server tool a free-text query dispatches to: prefer a tool whose name mentions
/// `search` or `query`, else the first tool. Pure + unit-tested.
pub(crate) fn pick_primary_tool(tools: &[McpToolInfo]) -> Option<&McpToolInfo> {
    tools
        .iter()
        .find(|t| {
            let n = t.name.to_lowercase();
            n.contains("search") || n.contains("query")
        })
        .or_else(|| tools.first())
}

#[async_trait]
impl Connector for McpConnector {
    fn id(&self) -> &str {
        &self.id
    }

    fn egress_class(&self) -> EgressClass {
        EgressClass::External
    }

    /// Fixed, non-PII labels (the trait requires `&'static str`). The PER-SERVER truthful
    /// attribution is the ledger row's `provider_id` = `mcp_<server_id>` (see the module doc).
    fn egress_attribution(&self) -> (&'static str, &'static str) {
        ("mcp_query", "MCP server (connector)")
    }

    async fn search(&self, redacted_query: &str) -> ConnectorResult {
        let q = redacted_query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        // Discover the primary tool AT CALL TIME (server metadata never reaches specs/prompts).
        let tools = self.client.list_tools().await?;
        let Some(primary) = pick_primary_tool(&tools) else {
            return Err(ConnectorError::Unconfigured(
                "the MCP server exposes no tools".into(),
            ));
        };
        let text = self
            .client
            .call_tool(&primary.name, serde_json::json!({ "query": q }))
            .await?;
        let text = text.trim();
        tracing::info!(target: "connector", provider = %self.id, chars = text.len(), "mcp query returned");
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let snippet: String = text.chars().take(SNIPPET_CAP).collect();
        Ok(vec![ConnectorHit {
            title: self.label.clone(),
            snippet,
            url: String::new(),
            source_label: format!("mcp · {}", self.label),
        }])
    }

    // `lookup` keeps the trait default: UNSUPPORTED (the verify pass stays Jira-only in v1).
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(enabled: bool, consented: bool) -> McpServer {
        McpServer {
            id: "abc123".into(),
            label: "Team Docs".into(),
            transport: "http".into(),
            endpoint: "https://127.0.0.1:9999/mcp".into(),
            args: vec![],
            enabled,
            consented,
            created_at: "2026-07-10T00:00:00Z".into(),
        }
    }

    /// FAIL-CLOSED: disabled or unconsented rows build NO connector (absent from the registry ⇒
    /// absent from the brain's tools ⇒ a direct search fails closed without egress).
    #[test]
    fn from_row_fail_closed_unless_enabled_and_consented() {
        assert!(McpConnector::from_row_if_available(&row(false, false)).is_none());
        assert!(
            McpConnector::from_row_if_available(&row(true, false)).is_none(),
            "unconsented"
        );
        assert!(
            McpConnector::from_row_if_available(&row(false, true)).is_none(),
            "disabled"
        );
        let c = McpConnector::from_row_if_available(&row(true, true)).expect("armed row builds");
        assert_eq!(c.id(), "mcp_abc123");
        assert_eq!(c.egress_class(), EgressClass::External);
        assert_eq!(
            c.egress_attribution(),
            ("mcp_query", "MCP server (connector)")
        );
    }

    /// A stdio row with a relative path is refused even when enabled + consented (code execution
    /// needs an absolute path).
    #[test]
    fn from_row_refuses_relative_stdio_path() {
        let mut r = row(true, true);
        r.transport = "stdio".into();
        r.endpoint = "npx".into();
        assert!(McpConnector::from_row_if_available(&r).is_none());
    }

    #[test]
    fn pick_primary_tool_prefers_search_like_names() {
        let tools = vec![
            McpToolInfo {
                name: "fetch_page".into(),
                description: String::new(),
            },
            McpToolInfo {
                name: "docs_search".into(),
                description: String::new(),
            },
        ];
        assert_eq!(pick_primary_tool(&tools).unwrap().name, "docs_search");
        let tools = vec![McpToolInfo {
            name: "run_query".into(),
            description: String::new(),
        }];
        assert_eq!(pick_primary_tool(&tools).unwrap().name, "run_query");
        // No search/query-shaped tool → the first one.
        let tools = vec![
            McpToolInfo {
                name: "alpha".into(),
                description: String::new(),
            },
            McpToolInfo {
                name: "beta".into(),
                description: String::new(),
            },
        ];
        assert_eq!(pick_primary_tool(&tools).unwrap().name, "alpha");
        assert!(pick_primary_tool(&[]).is_none());
    }
}
