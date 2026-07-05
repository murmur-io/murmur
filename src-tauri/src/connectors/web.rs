//! WEB SEARCH connector — the first connector (brain2, Phase F). Lets "research about the world"
//! (weather, facts, who-won-X) reach the web instead of only the user's vault.
//!
//! ## Egress posture (NEW EGRESS CLASS — audited by the lock-security reviewer)
//! This connector is [`EgressClass::External`]. It is exposed to the brain ONLY when ALL of:
//! - `config.web_search_enabled` is true (the master toggle), AND
//! - `config.web_search_consented` is true (one-time consent, preserve-only, flipped solely by the
//!   `consent_to_web_search` command), AND
//! - a Brave API key is present in the Keychain (`web_search_api_key`).
//!
//! Otherwise [`WebConnector::from_config_if_available`] returns `None` and the connector is ABSENT
//! from the registry, so the brain never even offers the tool (fail-closed). The framework
//! ([`crate::connectors::ConnectorRegistry::search`]) REDACTS the query through the firewall before
//! it reaches [`WebConnector::search`], so the outgoing query is scrubbed of emails/cards/phones.
//!
//! ## The provider sub-seam — Brave today, swappable tomorrow
//! [`WebSearchProvider`] is a sub-seam so the HTTP backend (Brave) is swappable for Tavily /
//! DuckDuckGo / etc. with zero change to the connector or the brain. The default impl is
//! [`BraveSearch`] (GET `https://api.search.brave.com/res/v1/web/search` with the
//! `X-Subscription-Token` header). The key is BYO and lives in the Keychain — NEVER logged, NEVER
//! handed to the FE.
//!
//! ## No PII in logs
//! Logs carry the connector id + hit count + HTTP status only — never the query, the snippets, or the
//! API key.

use async_trait::async_trait;
use serde::Deserialize;

use super::{Connector, ConnectorError, ConnectorHit, ConnectorResult, EgressClass};
use crate::settings::AppConfig;

/// Keychain account holding the BYO web-search API key (Brave). A non-gated string secret, stored via
/// the same data-protection keychain seam as the Anthropic key. NEVER logged / NEVER sent to the FE.
pub const WEB_SEARCH_KEY_ACCOUNT: &str = "web_search_api_key";

/// The loud attribution label every web hit carries, so the brain's answer is visibly "via web".
const SOURCE_LABEL: &str = "web · Brave";

/// Sub-seam: a swappable web-search backend. Takes an ALREADY-REDACTED query (the connector relies on
/// the framework having redacted it) + the API key, returns provider-agnostic [`ConnectorHit`]s.
///
/// Swapping Brave for Tavily/DuckDuckGo later means one new impl here — the connector, the registry,
/// and the brain are untouched.
#[async_trait]
pub trait WebSearchProvider: Send + Sync {
    /// Stable provider id (for the source label / diagnostics), e.g. `"brave"`.
    fn id(&self) -> &str;

    /// Run the web search. `api_key` is resolved by the connector from the Keychain (never logged);
    /// `redacted_query` is post-firewall. Returns the parsed hits or a [`ConnectorError::Failed`] on a
    /// network/HTTP/parse failure (non-PII message).
    async fn search(&self, redacted_query: &str, api_key: &str) -> ConnectorResult;
}

/// The default web-search provider: Brave Search API.
///
/// `GET https://api.search.brave.com/res/v1/web/search?q=…` with `X-Subscription-Token: <key>` and
/// `Accept: application/json`. Parses `web.results[].{title, description, url}` into [`ConnectorHit`]s.
/// Uses the crate's existing rustls `reqwest` — no new dependency.
pub struct BraveSearch {
    base_url: String,
}

impl Default for BraveSearch {
    fn default() -> Self {
        Self {
            base_url: "https://api.search.brave.com/res/v1/web/search".to_string(),
        }
    }
}

impl BraveSearch {
    /// Parse a Brave `/web/search` JSON body into [`ConnectorHit`]s. Pulled out of `search` so it can
    /// be unit-tested with a fixture body and NO network. Missing `web`/`results` → empty vec (a clean
    /// "no results"), never an error. Each result without a usable title is skipped.
    fn parse_results(body: &str) -> ConnectorResult {
        let parsed: BraveResponse = serde_json::from_str(body)
            .map_err(|e| ConnectorError::Failed(format!("brave response parse: {e}")))?;
        let results = parsed.web.map(|w| w.results).unwrap_or_default();
        let hits = results
            .into_iter()
            .filter_map(|r| {
                let title = r.title.trim().to_string();
                if title.is_empty() {
                    return None;
                }
                Some(ConnectorHit {
                    title,
                    snippet: r.description.unwrap_or_default().trim().to_string(),
                    url: r.url.unwrap_or_default().trim().to_string(),
                    source_label: SOURCE_LABEL.to_string(),
                })
            })
            .collect();
        Ok(hits)
    }
}

#[async_trait]
impl WebSearchProvider for BraveSearch {
    fn id(&self) -> &str {
        "brave"
    }

    async fn search(&self, redacted_query: &str, api_key: &str) -> ConnectorResult {
        let client = reqwest::Client::new();
        let resp = client
            .get(&self.base_url)
            .query(&[("q", redacted_query)])
            .header("X-Subscription-Token", api_key)
            .header("Accept", "application/json")
            .send()
            .await
            // `without_url()` strips the request URL (which carries the query string) from the
            // reqwest error — no query text leaks into the error/logs/FE (no-PII rule).
            .map_err(|e| ConnectorError::Failed(format!("brave request: {}", e.without_url())))?;
        let status = resp.status();
        if !status.is_success() {
            // Non-PII: status code only, never the query or the key.
            tracing::warn!(target: "connector", provider = "brave", status = status.as_u16(), "web search HTTP error");
            return Err(ConnectorError::Failed(format!("brave HTTP {status}")));
        }
        let body = resp
            .text()
            .await
            .map_err(|e| ConnectorError::Failed(format!("brave body: {}", e.without_url())))?;
        Self::parse_results(&body)
    }
}

/// Shape of the Brave `/web/search` JSON we consume — only `web.results[].{title,description,url}`.
/// `#[serde(default)]` everywhere so a missing field never fails the parse.
#[derive(Debug, Deserialize)]
struct BraveResponse {
    #[serde(default)]
    web: Option<BraveWeb>,
}

#[derive(Debug, Deserialize)]
struct BraveWeb {
    #[serde(default)]
    results: Vec<BraveResult>,
}

#[derive(Debug, Deserialize)]
struct BraveResult {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

/// The web-search connector — wires a [`WebSearchProvider`] + the BYO API key behind the
/// [`Connector`] seam. Built ONLY when enabled + consented + keyed (see
/// [`WebConnector::from_config_if_available`]).
pub struct WebConnector {
    provider: Box<dyn WebSearchProvider>,
    api_key: String,
}

impl WebConnector {
    /// Build the web connector IF AND ONLY IF the config enables web search, consent is granted, AND a
    /// Brave API key is in the Keychain. Any missing prerequisite ⇒ `None` (the connector is absent
    /// from the registry → the brain never offers the tool). This is the FAIL-CLOSED gate: no consent
    /// or no key means the tool simply does not exist for the session.
    ///
    /// Reads the Keychain for the key here; a Keychain error degrades to `None` (no connector) rather
    /// than surfacing — a missing/unreadable key just means "not available", never a crash.
    pub fn from_config_if_available(config: &AppConfig) -> Option<Self> {
        if !config.web_search_enabled || !config.web_search_consented {
            return None;
        }
        let key = crate::secrets::get_secret(WEB_SEARCH_KEY_ACCOUNT)
            .ok()
            .flatten()
            .filter(|k| !k.trim().is_empty())?;
        Some(Self {
            provider: Box::<BraveSearch>::default(),
            api_key: key,
        })
    }

    /// TEST-ONLY: build a connector around an injected provider + key, so the egress/parse path can be
    /// exercised with a fake provider and NO network. Never compiled into release.
    #[cfg(test)]
    fn with_provider(provider: Box<dyn WebSearchProvider>, api_key: &str) -> Self {
        Self {
            provider,
            api_key: api_key.to_string(),
        }
    }
}

#[async_trait]
impl Connector for WebConnector {
    fn id(&self) -> &str {
        "web"
    }

    fn egress_class(&self) -> EgressClass {
        EgressClass::External
    }

    fn egress_attribution(&self) -> (&'static str, &'static str) {
        ("web_search", "web search (connector)")
    }

    async fn search(&self, redacted_query: &str) -> ConnectorResult {
        let trimmed = redacted_query.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        let hits = self.provider.search(trimmed, &self.api_key).await?;
        tracing::info!(target: "connector", provider = self.provider.id(), hits = hits.len(), "web search returned");
        Ok(hits)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    /// A fake provider that captures the query + key it was handed and returns canned hits, so the
    /// connector's egress/wiring is exercised with NO real HTTP.
    struct FakeProvider {
        last_query: std::sync::Mutex<Option<String>>,
        last_key: std::sync::Mutex<Option<String>>,
    }
    #[async_trait]
    impl WebSearchProvider for FakeProvider {
        fn id(&self) -> &str {
            "fake"
        }
        async fn search(&self, redacted_query: &str, api_key: &str) -> ConnectorResult {
            *self.last_query.lock().unwrap() = Some(redacted_query.to_string());
            *self.last_key.lock().unwrap() = Some(api_key.to_string());
            Ok(vec![ConnectorHit {
                title: "Weather today".into(),
                snippet: "Sunny, 22°C".into(),
                url: "https://example.com/weather".into(),
                source_label: SOURCE_LABEL.to_string(),
            }])
        }
    }

    #[test]
    fn brave_parser_maps_json_to_hits() {
        // The exact shape the Brave /web/search API returns: web.results[].{title,description,url}.
        let body = r#"{
            "web": {
                "results": [
                    {"title":"Kraków weather","description":"Sunny and 22°C today.","url":"https://w.example/krakow"},
                    {"title":"Forecast","description":"Rain tomorrow.","url":"https://w.example/fc"}
                ]
            }
        }"#;
        let hits = BraveSearch::parse_results(body).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Kraków weather");
        assert_eq!(hits[0].snippet, "Sunny and 22°C today.");
        assert_eq!(hits[0].url, "https://w.example/krakow");
        // LOUD: every hit is attributed to the web source.
        assert_eq!(hits[0].source_label, "web · Brave");
    }

    #[test]
    fn brave_parser_tolerates_missing_fields_and_empty_results() {
        // No `web` key → empty (clean "no results"), never an error.
        assert!(BraveSearch::parse_results(r#"{}"#).unwrap().is_empty());
        // `web.results` empty → empty.
        assert!(BraveSearch::parse_results(r#"{"web":{"results":[]}}"#)
            .unwrap()
            .is_empty());
        // A result missing description/url still maps (snippet/url default to empty); a result with
        // an empty title is skipped.
        let body = r#"{"web":{"results":[{"title":"Only Title"},{"title":""}]}}"#;
        let hits = BraveSearch::parse_results(body).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "Only Title");
        assert_eq!(hits[0].snippet, "");
        assert_eq!(hits[0].url, "");
    }

    #[test]
    fn brave_parser_rejects_malformed_json() {
        assert!(matches!(
            BraveSearch::parse_results("not json at all"),
            Err(ConnectorError::Failed(_))
        ));
    }

    #[test]
    fn connector_forwards_query_and_key_to_provider() {
        // The connector hands the (already-redacted, by the framework) query + the BYO key to the
        // provider and labels the hits "via web".
        let provider = FakeProvider {
            last_query: std::sync::Mutex::new(None),
            last_key: std::sync::Mutex::new(None),
        };
        let connector = WebConnector::with_provider(Box::new(provider), "test-api-key");
        let hits = block_on(connector.search("what's the weather in Kraków")).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source_label, "web · Brave");
        assert_eq!(connector.egress_class(), EgressClass::External);
        assert_eq!(connector.id(), "web");
    }

    /// REDACTION SEAM end-to-end: a query carrying an EMAIL, redacted through the firewall (exactly as
    /// `ConnectorRegistry::search` does) before it reaches the connector, arrives at the WEB PROVIDER
    /// with the email scrubbed to a ⟪EMAIL_…⟫ token. The fake provider captures what it actually
    /// received, proving the PII never leaves toward the external service. (RED if the registry
    /// stopped redacting: the raw email would reach the provider.)
    #[test]
    fn redacted_query_reaches_provider_with_pii_scrubbed() {
        let captured = std::sync::Arc::new(FakeProvider {
            last_query: std::sync::Mutex::new(None),
            last_key: std::sync::Mutex::new(None),
        });
        // Mirror the framework's pre-egress redaction step.
        let (redacted, _map) =
            crate::summarize::redact::redact("email bob@acme.com what's the weather");
        let connector = WebConnector {
            provider: Box::new(FakeProviderRef(captured.clone())),
            api_key: "key".into(),
        };
        let _ = block_on(connector.search(&redacted));
        let seen = captured.last_query.lock().unwrap().clone().unwrap();
        assert!(
            !seen.contains("bob@acme.com"),
            "raw email must not reach the provider: {seen}"
        );
        assert!(
            seen.contains("\u{27ea}EMAIL_"),
            "redaction token must reach the provider: {seen}"
        );
        assert!(seen.contains("weather"), "non-PII terms survive: {seen}");
        // The BYO key is forwarded to the provider (never logged / FE-exposed).
        assert_eq!(captured.last_key.lock().unwrap().as_deref(), Some("key"));
    }

    /// Share one `FakeProvider` between the connector and the assertion.
    struct FakeProviderRef(std::sync::Arc<FakeProvider>);
    #[async_trait]
    impl WebSearchProvider for FakeProviderRef {
        fn id(&self) -> &str {
            self.0.id()
        }
        async fn search(&self, redacted_query: &str, api_key: &str) -> ConnectorResult {
            self.0.search(redacted_query, api_key).await
        }
    }

    #[test]
    fn connector_empty_query_egresses_nothing() {
        // An empty/whitespace query returns no hits WITHOUT calling the provider (no egress).
        struct PanicProvider;
        #[async_trait]
        impl WebSearchProvider for PanicProvider {
            fn id(&self) -> &str {
                "panic"
            }
            async fn search(&self, _q: &str, _k: &str) -> ConnectorResult {
                panic!("provider must not be called for an empty query");
            }
        }
        let connector = WebConnector::with_provider(Box::new(PanicProvider), "k");
        let hits = block_on(connector.search("   ")).unwrap();
        assert!(hits.is_empty());
    }
}
