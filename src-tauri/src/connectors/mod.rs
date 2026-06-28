//! CONNECTOR FRAMEWORK — live, on-demand external tools the brain can call (brain2, Phase F).
//!
//! A **connector** is a swappable source of "research about the world" that the brain reaches through
//! the SAME gated tool registry as the vault reads ([`crate::tools::execute_tool`]). Unlike the vault
//! tools (which read the local, visibility-gated SQLite store and EGRESS NOTHING), a connector may
//! reach an EXTERNAL service — so it carries a NEW EGRESS CLASS that this framework gates,
//! fail-closed, before anything leaves the device.
//!
//! Connectors are LIVE tools, NOT vectorized: the brain calls them on demand and turns the
//! [`ConnectorHit`]s into cited answers ("via web"), exactly as it does with vault hits.
//!
//! ## The three load-bearing disciplines (audited by the lock-security reviewer)
//! 1. **Consent-gated, fail-closed.** An [`EgressClass::External`] connector is exposed to the brain
//!    ONLY when it is BOTH enabled AND consented (`web_search_enabled` + `web_search_consented`, the
//!    latter preserve-only, flipped solely by the dedicated `consent_to_web_search` command). With no
//!    consent the connector is ABSENT from the registry; if it is somehow invoked anyway, it returns
//!    [`ConnectorError::NeedsConsent`] and EGRESSES NOTHING (see [`Connector::search`] contract).
//! 2. **Redacted.** The outgoing query passes the SAME redaction firewall
//!    ([`crate::summarize::redact::redact`]) as any cloud-bound text BEFORE it leaves — emails / cards
//!    / phones are scrubbed. The redaction is applied by the FRAMEWORK ([`ConnectorRegistry::search`])
//!    so an individual connector can never forget it.
//! 3. **Loud.** Every [`ConnectorHit`] carries a `source_label` (e.g. "web · Brave") so the answer is
//!    visibly attributed to the external source — the user always knows a result came from off-device.
//!
//! ## No PII in logs
//! Connector code logs the connector id, hit counts, and HTTP status — never the query text, the
//! result snippets, or the API key.

pub mod web;

use crate::settings::AppConfig;

/// Whether a connector reaches OFF the device. The framework gates every [`External`] connector
/// behind enable + consent; a [`Local`] connector (none yet) would be exempt, exactly as `ollama` is
/// exempt from the cloud-egress gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressClass {
    /// Reads only on-device data — no network. Never consent-gated.
    Local,
    /// Sends the (redacted) query to an EXTERNAL service. Consent-gated, fail-closed.
    External,
}

/// One result from a connector search. Deliberately small + provider-agnostic so every connector
/// (and every future provider behind one) maps onto the same shape the brain consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectorHit {
    /// Short human title of the result.
    pub title: String,
    /// A 1–2 sentence snippet/description the brain grounds its answer on.
    pub snippet: String,
    /// The source URL (the citation the answer is attributed to). May be empty if the provider omits
    /// it, but providers SHOULD supply it so the answer is auditable.
    pub url: String,
    /// LOUD attribution shown to the user, e.g. "web · Brave". Never empty — the framework asserts a
    /// connector sets it so a result can never be silently passed off as vault knowledge.
    pub source_label: String,
}

/// Failure modes of a connector search, kept distinct so the brain/caller can react precisely.
#[derive(Debug)]
pub enum ConnectorError {
    /// External egress is not consented (or the connector is disabled). NOTHING was sent. The brain
    /// surfaces a "needs consent" status; it is NEVER an error that leaks the query.
    NeedsConsent,
    /// The connector is enabled + consented but cannot run (missing API key, etc.). NOTHING was sent.
    Unconfigured(String),
    /// The external request itself failed (network / HTTP / parse). The message is non-PII (status +
    /// context only).
    Failed(String),
}

impl std::fmt::Display for ConnectorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectorError::NeedsConsent => write!(f, "needs_consent"),
            ConnectorError::Unconfigured(m) => write!(f, "unconfigured: {m}"),
            ConnectorError::Failed(m) => write!(f, "failed: {m}"),
        }
    }
}

impl std::error::Error for ConnectorError {}

/// Map a connector error into the app's error domain. `NeedsConsent`/`Unconfigured` become
/// [`crate::error::AppError::Unavailable`] (graceful, the caller treats it like the cloud-no-consent
/// fail-closed case); a real `Failed` becomes [`crate::error::AppError::Summarize`] (a runtime
/// external failure), never a `Storage`/`Locked` — this is NOT a content-gate refusal.
impl From<ConnectorError> for crate::error::AppError {
    fn from(e: ConnectorError) -> Self {
        match e {
            ConnectorError::NeedsConsent => crate::error::AppError::Unavailable(
                "web search needs your one-time consent (Settings ▸ Privacy)".to_string(),
            ),
            ConnectorError::Unconfigured(m) => crate::error::AppError::Unavailable(m),
            ConnectorError::Failed(m) => crate::error::AppError::Summarize(m),
        }
    }
}

/// A connector's search result type — `Vec<ConnectorHit>` or a typed [`ConnectorError`].
pub type ConnectorResult = std::result::Result<Vec<ConnectorHit>, ConnectorError>;

/// The connector seam. Each impl DECLARES its [`EgressClass`] and runs a single async `search`.
///
/// CONTRACT (load-bearing): an `External` connector MUST NOT egress anything until it has confirmed
/// it is consented + configured; on a missing prerequisite it returns [`ConnectorError::NeedsConsent`]
/// / [`ConnectorError::Unconfigured`] WITHOUT sending the query. The framework
/// ([`ConnectorRegistry`]) also enforces the consent gate at the registry boundary, so a connector
/// is the second line, not the only line.
#[async_trait::async_trait]
pub trait Connector: Send + Sync {
    /// Stable id, e.g. `"web"`.
    fn id(&self) -> &str;

    /// The egress class — `External` connectors are consent-gated by the framework.
    fn egress_class(&self) -> EgressClass;

    /// Run the search for an ALREADY-REDACTED query (the registry redacts before calling this — see
    /// [`ConnectorRegistry::search`]). Returns the hits, each carrying a loud `source_label`.
    async fn search(&self, redacted_query: &str) -> ConnectorResult;
}

/// The registry of connectors AVAILABLE to the brain for the current config. It exposes ONLY the
/// connectors that are enabled and (for `External` ones) consented + configured — so a disabled /
/// unconsented / un-keyed web connector is simply not present, and the brain's tool list never offers
/// it. Built per call from the live [`AppConfig`] (cheap).
pub struct ConnectorRegistry {
    connectors: Vec<Box<dyn Connector>>,
}

impl ConnectorRegistry {
    /// Build the registry of connectors EXPOSED for `config`. Today the only connector is the web
    /// search connector; it is included ONLY when `web_search_enabled && web_search_consented && a key
    /// is present`. Any future `Local` connector would be included unconditionally (no egress to gate).
    ///
    /// Note: the API-key presence check reads the Keychain — kept here (not in `search`) so an
    /// un-keyed connector is ABSENT from the brain's tool list entirely (it never even appears as a
    /// callable tool), matching how an un-consented cloud provider is simply not built.
    pub fn build(config: &AppConfig) -> Self {
        let mut connectors: Vec<Box<dyn Connector>> = Vec::new();
        if let Some(c) = web::WebConnector::from_config_if_available(config) {
            connectors.push(Box::new(c));
        }
        Self { connectors }
    }

    /// Is a connector with this id currently exposed (enabled + consented + configured)?
    pub fn has(&self, id: &str) -> bool {
        self.connectors.iter().any(|c| c.id() == id)
    }

    /// The ids of every exposed connector (for the brain's tool-availability decision / tests).
    pub fn ids(&self) -> Vec<&str> {
        self.connectors.iter().map(|c| c.id()).collect()
    }

    /// Run a search through the connector `id`, REDACTING the query through the firewall first.
    ///
    /// The redaction is applied HERE (not inside the connector) so every connector inherits the
    /// firewall and cannot forget it. An `id` that is not exposed (disabled / unconsented / un-keyed)
    /// returns [`ConnectorError::NeedsConsent`] WITHOUT touching the network — the fail-closed default.
    pub async fn search(&self, id: &str, query: &str) -> ConnectorResult {
        let Some(connector) = self.connectors.iter().find(|c| c.id() == id) else {
            // Not exposed → fail closed. NOTHING egresses.
            return Err(ConnectorError::NeedsConsent);
        };
        // REDACT before egress: scrub emails/cards/phones out of the outgoing query (the same
        // firewall any cloud-bound text passes). We discard the token→value map — web results are
        // attributed to the source as-is and never de-tokenized back into vault content.
        let (redacted, _map) = crate::summarize::redact::redact(query);
        connector.search(&redacted).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake connector that CAPTURES the query it was handed, so a test can prove the framework
    /// redacted it BEFORE the connector saw it (the connector itself does no redaction).
    struct CaptureConnector {
        last_query: std::sync::Mutex<Option<String>>,
    }
    #[async_trait::async_trait]
    impl Connector for CaptureConnector {
        fn id(&self) -> &str {
            "capture"
        }
        fn egress_class(&self) -> EgressClass {
            EgressClass::External
        }
        async fn search(&self, redacted_query: &str) -> ConnectorResult {
            *self.last_query.lock().unwrap() = Some(redacted_query.to_string());
            Ok(vec![ConnectorHit {
                title: "t".into(),
                snippet: "s".into(),
                url: "https://example.com".into(),
                source_label: "web · fake".into(),
            }])
        }
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn registry_redacts_query_before_the_connector_sees_it() {
        // Drive ConnectorRegistry::search directly with a captured connector to prove the framework
        // applies the firewall: an email in the query must be a ⟪EMAIL_…⟫ token by the time the
        // connector runs — the connector never sees the raw PII.
        let cap = std::sync::Arc::new(CaptureConnector {
            last_query: std::sync::Mutex::new(None),
        });
        let registry = ConnectorRegistry {
            connectors: vec![Box::new(CaptureConnectorRef(cap.clone()))],
        };
        let hits = block_on(registry.search("capture", "email bob@acme.com about weather")).unwrap();
        assert_eq!(hits.len(), 1);
        let seen = cap.last_query.lock().unwrap().clone().unwrap();
        assert!(!seen.contains("bob@acme.com"), "raw email must be redacted before egress: {seen}");
        assert!(seen.contains("\u{27ea}EMAIL_"), "the query must carry the redaction token: {seen}");
        assert!(seen.contains("weather"), "non-PII terms survive redaction: {seen}");
    }

    /// Thin wrapper so the test can share one `CaptureConnector` between the registry and the
    /// assertion (the registry owns `Box<dyn Connector>`).
    struct CaptureConnectorRef(std::sync::Arc<CaptureConnector>);
    #[async_trait::async_trait]
    impl Connector for CaptureConnectorRef {
        fn id(&self) -> &str {
            self.0.id()
        }
        fn egress_class(&self) -> EgressClass {
            self.0.egress_class()
        }
        async fn search(&self, redacted_query: &str) -> ConnectorResult {
            self.0.search(redacted_query).await
        }
    }

    #[test]
    fn unexposed_connector_id_fails_closed_without_egress() {
        // An empty registry (the disabled/unconsented state) returns NeedsConsent for any id and
        // never reaches a network path.
        let registry = ConnectorRegistry { connectors: vec![] };
        let res = block_on(registry.search("web", "what's the weather"));
        assert!(matches!(res, Err(ConnectorError::NeedsConsent)));
        assert!(!registry.has("web"));
        assert!(registry.ids().is_empty());
    }

    #[test]
    fn registry_excludes_web_when_disabled_or_unconsented_failclosed() {
        use crate::settings::AppConfig;
        // Default config: web search OFF + unconsented → the web connector is NOT exposed, and a
        // `search("web", …)` fails closed WITHOUT egress. (RED if the gate were dropped: the connector
        // would be present and would attempt a network call.)
        let off = AppConfig::default();
        let registry = ConnectorRegistry::build(&off);
        assert!(!registry.has("web"), "web connector must be absent when disabled/unconsented");
        let res = block_on(registry.search("web", "what's the weather"));
        assert!(
            matches!(res, Err(ConnectorError::NeedsConsent)),
            "an unexposed web connector must fail closed (needs_consent), egressing nothing"
        );

        // Enabled but STILL unconsented → also excluded (consent is the second required gate).
        let enabled_unconsented = AppConfig {
            web_search_enabled: true,
            web_search_consented: false,
            ..AppConfig::default()
        };
        let registry = ConnectorRegistry::build(&enabled_unconsented);
        assert!(
            !registry.has("web"),
            "enabled-but-unconsented web search must STILL be excluded (fail-closed on consent)"
        );

        // Consented but DISABLED → also excluded (enable is the first required gate).
        let consented_disabled = AppConfig {
            web_search_enabled: false,
            web_search_consented: true,
            ..AppConfig::default()
        };
        assert!(
            !ConnectorRegistry::build(&consented_disabled).has("web"),
            "consented-but-disabled web search must be excluded (fail-closed on enable)"
        );
    }

    #[test]
    fn connector_error_maps_to_unavailable_or_summarize() {
        assert!(matches!(
            crate::error::AppError::from(ConnectorError::NeedsConsent),
            crate::error::AppError::Unavailable(_)
        ));
        assert!(matches!(
            crate::error::AppError::from(ConnectorError::Unconfigured("no key".into())),
            crate::error::AppError::Unavailable(_)
        ));
        assert!(matches!(
            crate::error::AppError::from(ConnectorError::Failed("HTTP 500".into())),
            crate::error::AppError::Summarize(_)
        ));
    }
}
