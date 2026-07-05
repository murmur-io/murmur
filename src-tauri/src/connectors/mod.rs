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

pub mod calendar;
pub mod jira;
pub mod slack;
pub mod web;

use std::sync::Arc;

use crate::settings::AppConfig;
use crate::summarize::egress_log::{active_sink, EgressEntry, EgressSink};
use crate::summarize::meta::CallMeta;
use crate::summarize::redact::{active_name_redactor, redact_connector_query, NameRedactor};

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

    /// The TRUTHFUL egress-ledger attribution for THIS connector: the `(call_kind, destination)`
    /// pair the framework records in the content-free ledger row so the Analytics "receipt of what
    /// left your Mac" names the RIGHT external service — a Jira egress is labeled Jira, a web search
    /// is labeled web, never one masquerading as the other. Each field is a non-PII `&'static str`
    /// (a fixed label, never query content). The default is a GENERIC fallback so a connector that
    /// forgets to declare its own is still audited (never mis-attributed to web); every real
    /// `External` connector OVERRIDES it with its own truthful pair.
    fn egress_attribution(&self) -> (&'static str, &'static str) {
        ("connector_search", "external connector")
    }

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
    /// The active on-device NER name-redactor, applied by the FRAMEWORK to every outgoing connector
    /// query (mirroring the cloud provider path). `NoopNameRedactor` when no model is installed →
    /// byte-identical to before this seam existed. Held on the registry (not resolved per call) so a
    /// single lazy-loaded redactor is reused, exactly as `make_provider` reuses one for the provider.
    names: Arc<dyn NameRedactor>,
    /// The content-free egress ledger sink. One [`EgressEntry`] is recorded per external connector
    /// search ATTEMPT so the Analytics "receipt of what left your Mac" covers connector egress too.
    /// `NoopEgressSink` before startup wiring / in tests that do not install a sink.
    sink: Arc<dyn EgressSink>,
}

impl ConnectorRegistry {
    /// Build the registry of connectors EXPOSED for `config`. Today the only connector is the web
    /// search connector; it is included ONLY when `web_search_enabled && web_search_consented && a key
    /// is present`. Any future `Local` connector would be included unconditionally (no egress to gate).
    ///
    /// Note: the API-key presence check reads the Keychain — kept here (not in `search`) so an
    /// un-keyed connector is ABSENT from the brain's tool list entirely (it never even appears as a
    /// callable tool), matching how an un-consented cloud provider is simply not built.
    ///
    /// The framework's NER name-redactor and the egress ledger sink are wired from the process-global
    /// seams ([`active_name_redactor`] / [`active_sink`]) so EVERY connector inherits both — no
    /// individual connector can forget the name layer or skip the ledger row.
    pub fn build(config: &AppConfig) -> Self {
        let mut connectors: Vec<Box<dyn Connector>> = Vec::new();
        if let Some(c) = web::WebConnector::from_config_if_available(config) {
            connectors.push(Box::new(c));
        }
        if let Some(c) = jira::JiraConnector::from_config_if_available(config) {
            connectors.push(Box::new(c));
        }
        if let Some(c) = slack::SlackConnector::from_config_if_available(config) {
            connectors.push(Box::new(c));
        }
        Self {
            connectors,
            names: active_name_redactor(),
            sink: active_sink(),
        }
    }

    /// Is a connector with this id currently exposed (enabled + consented + configured)?
    pub fn has(&self, id: &str) -> bool {
        self.connectors.iter().any(|c| c.id() == id)
    }

    /// The ids of every exposed connector (for the brain's tool-availability decision / tests).
    pub fn ids(&self) -> Vec<&str> {
        self.connectors.iter().map(|c| c.id()).collect()
    }

    /// Run a search through the connector `id`, REDACTING the query through the FULL firewall first
    /// and recording a content-free egress ledger row for the attempt.
    ///
    /// The redaction is applied HERE (not inside the connector) so every connector inherits the
    /// firewall and cannot forget it. An `id` that is not exposed (disabled / unconsented / un-keyed)
    /// returns [`ConnectorError::NeedsConsent`] WITHOUT touching the network — the fail-closed default
    /// (no egress ⇒ NO ledger row).
    ///
    /// The scrub is the SAME two layers the cloud provider path applies
    /// ([`RedactingProvider::summarize_with_meta`]): the regex firewall (emails/cards/phones) AND the
    /// on-device NER name layer — so a person name the provider would scrub is scrubbed here too
    /// (byte-identical to the regex-only behaviour when no NER model is installed). We discard the
    /// token→value maps — web results are attributed to the source as-is and never de-tokenized back
    /// into vault content.
    ///
    /// LEDGER: for an EXTERNAL connector we record ONE content-free [`EgressEntry`] on the attempt
    /// (a failed HTTP call is still attempted egress), carrying provider/destination, a per-connector
    /// `call_kind`/`destination` (from [`Connector::egress_attribution`] — so a Jira egress is
    /// labeled Jira, not a web search), the scrubbed-query BYTE SIZE, and the redaction COUNTS —
    /// never the query text (the ledger is content-free by design).
    pub async fn search(&self, id: &str, query: &str) -> ConnectorResult {
        let Some(connector) = self.connectors.iter().find(|c| c.id() == id) else {
            // Not exposed → fail closed. NOTHING egresses, so nothing is recorded.
            return Err(ConnectorError::NeedsConsent);
        };
        // FULL firewall scrub (regex + NER names), mirroring the provider path. `counts` are the
        // content-free redaction tallies for the ledger; `redacted` is what actually egresses.
        let (redacted, counts) = redact_connector_query(query, self.names.as_ref());

        // Record ONE content-free ledger row for this external egress ATTEMPT, BEFORE the network
        // call, so even a failing request is audited as attempted egress. `user_bytes` is the SIZE
        // of the scrubbed query (never its text); `system_bytes` is 0 (a connector has no system
        // prompt). `meta` is default (a search backend reports no token usage).
        if connector.egress_class() == EgressClass::External {
            // Per-connector truthful attribution (both are non-PII fixed labels) so the ledger row
            // names the RIGHT external service — a Jira egress is never recorded as a web search.
            let (call_kind, destination) = connector.egress_attribution();
            self.sink.record(EgressEntry {
                provider_id: connector.id().to_string(),
                destination: destination.to_string(),
                model_requested: String::new(),
                call_kind,
                meta: CallMeta::default(),
                redactions: counts,
                system_bytes: 0,
                user_bytes: redacted.len(),
                meeting_id: None,
            });
        }

        connector.search(&redacted).await
    }

    /// TEST-ONLY: build a registry around injected connectors + an explicit name redactor + sink, so
    /// the framework-level NER scrub and the ledger row can be exercised with fakes and NO network /
    /// NO Keychain. Never compiled into release.
    #[cfg(test)]
    fn with_parts(
        connectors: Vec<Box<dyn Connector>>,
        names: Arc<dyn NameRedactor>,
        sink: Arc<dyn EgressSink>,
    ) -> Self {
        Self { connectors, names, sink }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::summarize::egress_log::NoopEgressSink;
    use crate::summarize::redact::NoopNameRedactor;

    /// Captures every `EgressEntry` the framework records, so a test can assert exactly one
    /// content-free row was written for a connector search.
    struct CaptureEgressSink(std::sync::Arc<std::sync::Mutex<Vec<EgressEntry>>>);
    impl EgressSink for CaptureEgressSink {
        fn record(&self, entry: EgressEntry) {
            self.0.lock().unwrap().push(entry);
        }
    }

    /// Deterministic test-only name redactor: scrubs a FIXED known name → a stable ⟪NAME_1⟫ token,
    /// exactly mirroring the `FixtureNameRedactor` the redact.rs name-seam tests use. Stands in for
    /// the on-device NER model so the framework's name layer is exercised without a model download.
    struct FixtureNameRedactor;
    impl NameRedactor for FixtureNameRedactor {
        fn redact_names(&self, text: &str) -> (String, Vec<(String, String)>) {
            let name = "Anna Kowalska";
            if text.contains(name) {
                let tok = "\u{27ea}NAME_1\u{27eb}".to_string();
                (text.replace(name, &tok), vec![(tok, name.to_string())])
            } else {
                (text.to_string(), Vec::new())
            }
        }
    }

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
        let registry = ConnectorRegistry::with_parts(
            vec![Box::new(CaptureConnectorRef(cap.clone()))],
            std::sync::Arc::new(NoopNameRedactor),
            std::sync::Arc::new(NoopEgressSink),
        );
        let hits = block_on(registry.search("capture", "email bob@acme.com about weather")).unwrap();
        assert_eq!(hits.len(), 1);
        let seen = cap.last_query.lock().unwrap().clone().unwrap();
        assert!(
            !seen.contains("bob@acme.com"),
            "raw email must be redacted before egress: {seen}"
        );
        assert!(
            seen.contains("\u{27ea}EMAIL_"),
            "the query must carry the redaction token: {seen}"
        );
        assert!(
            seen.contains("weather"),
            "non-PII terms survive redaction: {seen}"
        );
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
        // never reaches a network path — and, being not-exposed, records NO ledger row.
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let registry = ConnectorRegistry::with_parts(
            vec![],
            std::sync::Arc::new(NoopNameRedactor),
            std::sync::Arc::new(CaptureEgressSink(captured.clone())),
        );
        let res = block_on(registry.search("web", "what's the weather"));
        assert!(matches!(res, Err(ConnectorError::NeedsConsent)));
        assert!(!registry.has("web"));
        assert!(registry.ids().is_empty());
        assert!(
            captured.lock().unwrap().is_empty(),
            "a fail-closed (not-exposed) search must egress nothing AND record no ledger row"
        );
    }

    #[test]
    fn registry_excludes_web_when_disabled_or_unconsented_failclosed() {
        use crate::settings::AppConfig;
        // Default config: web search OFF + unconsented → the web connector is NOT exposed, and a
        // `search("web", …)` fails closed WITHOUT egress. (RED if the gate were dropped: the connector
        // would be present and would attempt a network call.)
        let off = AppConfig::default();
        let registry = ConnectorRegistry::build(&off);
        assert!(
            !registry.has("web"),
            "web connector must be absent when disabled/unconsented"
        );
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

    /// (a) NER GAP FIX — RED-before-GREEN: with an ACTIVE name redactor (the model-installed case,
    /// stood in by `FixtureNameRedactor`), a person name in the query is scrubbed to a ⟪NAME_…⟫
    /// token BEFORE the connector — so no name egresses to the external service. On the pre-fix code
    /// (registry applied `redact(query)` = regex ONLY, never the NER layer) the raw name reached the
    /// connector → this asserts RED. GREEN once the framework runs `redact_connector_query`.
    #[test]
    fn registry_applies_ner_name_layer_before_the_connector_sees_it() {
        let cap = std::sync::Arc::new(CaptureConnector {
            last_query: std::sync::Mutex::new(None),
        });
        let registry = ConnectorRegistry::with_parts(
            vec![Box::new(CaptureConnectorRef(cap.clone()))],
            std::sync::Arc::new(FixtureNameRedactor), // the "NER model installed" stand-in
            std::sync::Arc::new(NoopEgressSink),
        );
        let hits = block_on(registry.search("capture", "what did Anna Kowalska decide about weather"))
            .unwrap();
        assert_eq!(hits.len(), 1);
        let seen = cap.last_query.lock().unwrap().clone().unwrap();
        assert!(
            !seen.contains("Anna Kowalska"),
            "the person NAME must be scrubbed by the NER layer before egress: {seen}"
        );
        assert!(
            seen.contains("\u{27ea}NAME_1\u{27eb}"),
            "the query must carry the NER name token: {seen}"
        );
        assert!(seen.contains("weather"), "non-PII terms survive: {seen}");
    }

    /// (b) LEDGER — a connector search records EXACTLY ONE content-free egress row: provider "capture"
    /// (the connector id), the GENERIC-FALLBACK attribution (`"capture"` declares no
    /// `egress_attribution` override → `call_kind "connector_search"` / destination
    /// "external connector"), the SCRUBBED-query byte size, and the redaction
    /// COUNTS (1 email + 1 name here). Copies the content-free invariant from redact.rs's
    /// `egress_entry_is_content_free_and_captures_meta_and_counts`: NO query text / NO PII appears in
    /// ANY field of the recorded entry (asserted over the full Debug output).
    #[test]
    fn connector_search_records_one_content_free_egress_row() {
        let cap = std::sync::Arc::new(CaptureConnector {
            last_query: std::sync::Mutex::new(None),
        });
        let recorded = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let registry = ConnectorRegistry::with_parts(
            vec![Box::new(CaptureConnectorRef(cap.clone()))],
            std::sync::Arc::new(FixtureNameRedactor),
            std::sync::Arc::new(CaptureEgressSink(recorded.clone())),
        );
        // Query carries one email (regex) + one person name (NER) + plain terms.
        let query = "email bob@acme.com ask Anna Kowalska about the weather";
        block_on(registry.search("capture", query)).unwrap();

        let rows = recorded.lock().unwrap();
        assert_eq!(rows.len(), 1, "exactly one content-free ledger row per connector search");
        let row = &rows[0];

        // Shape: connector-attributed via the GENERIC FALLBACK (the fake "capture" connector does
        // not override egress_attribution), sizes + counts only.
        assert_eq!(row.provider_id, "capture");
        assert_eq!(row.call_kind, "connector_search");
        assert_eq!(row.destination, "external connector");
        assert_eq!(row.model_requested, "");
        assert_eq!(row.system_bytes, 0);
        assert!(row.user_bytes > 0, "the scrubbed-query byte SIZE is recorded");
        assert_eq!(row.redactions.email, 1, "one email scrubbed");
        assert_eq!(row.redactions.name, 1, "one person name scrubbed by the NER layer");
        assert_eq!(row.redactions.card, 0);
        assert_eq!(row.redactions.phone, 0);

        // CONTENT-FREE INVARIANT: no query text / no PII in ANY field (full Debug output).
        let debug = format!("{:?}", row);
        assert!(!debug.contains("bob@acme.com"), "email must NOT appear in ledger row: {debug}");
        assert!(!debug.contains("Anna Kowalska"), "name must NOT appear in ledger row: {debug}");
        assert!(!debug.contains("weather"), "query terms must NOT appear in ledger row: {debug}");
    }

    /// (b, cont.) The ledger row is recorded even when the external call FAILS — a failed HTTP call
    /// is still ATTEMPTED egress and must be audited. Uses a connector that always errors; the row is
    /// still present and still content-free.
    #[test]
    fn failed_connector_search_still_records_the_attempt() {
        struct FailingConnector;
        #[async_trait::async_trait]
        impl Connector for FailingConnector {
            fn id(&self) -> &str {
                "capture"
            }
            fn egress_class(&self) -> EgressClass {
                EgressClass::External
            }
            async fn search(&self, _redacted_query: &str) -> ConnectorResult {
                Err(ConnectorError::Failed("brave HTTP 500".into()))
            }
        }
        let recorded = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let registry = ConnectorRegistry::with_parts(
            vec![Box::new(FailingConnector)],
            std::sync::Arc::new(NoopNameRedactor),
            std::sync::Arc::new(CaptureEgressSink(recorded.clone())),
        );
        let res = block_on(registry.search("capture", "email bob@acme.com weather"));
        assert!(matches!(res, Err(ConnectorError::Failed(_))), "the failure surfaces");
        let rows = recorded.lock().unwrap();
        assert_eq!(rows.len(), 1, "a failed attempt is still audited as attempted egress");
        // "capture" declares no override → generic fallback attribution.
        assert_eq!(rows[0].call_kind, "connector_search");
        assert_eq!(rows[0].redactions.email, 1);
        let debug = format!("{:?}", rows[0]);
        assert!(!debug.contains("bob@acme.com"), "email must NOT appear in ledger row: {debug}");
    }

    /// (c) PER-CONNECTOR ATTRIBUTION — RED-before-GREEN: a `"jira"`-id connector must record its OWN
    /// truthful ledger attribution, NOT the web-search label. On the pre-fix code (the framework
    /// hardcoded `call_kind: "web_search"` / destination "web search (connector)" for EVERY External
    /// connector) a Jira egress was recorded as a web search — this test asserts RED there. GREEN once
    /// the framework reads `Connector::egress_attribution` (jira → "jira_search" / "Jira (connector)").
    #[test]
    fn jira_connector_search_is_attributed_to_jira_not_web() {
        struct JiraLikeConnector;
        #[async_trait::async_trait]
        impl Connector for JiraLikeConnector {
            fn id(&self) -> &str {
                "jira"
            }
            fn egress_class(&self) -> EgressClass {
                EgressClass::External
            }
            fn egress_attribution(&self) -> (&'static str, &'static str) {
                ("jira_search", "Jira (connector)")
            }
            async fn search(&self, _redacted_query: &str) -> ConnectorResult {
                Ok(vec![ConnectorHit {
                    title: "t".into(),
                    snippet: "s".into(),
                    url: "https://example.atlassian.net/browse/ABC-1".into(),
                    source_label: "jira · fake".into(),
                }])
            }
        }
        let recorded = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let registry = ConnectorRegistry::with_parts(
            vec![Box::new(JiraLikeConnector)],
            std::sync::Arc::new(NoopNameRedactor),
            std::sync::Arc::new(CaptureEgressSink(recorded.clone())),
        );
        block_on(registry.search("jira", "sprint status for ABC")).unwrap();

        let rows = recorded.lock().unwrap();
        assert_eq!(rows.len(), 1, "exactly one ledger row for the jira search");
        let row = &rows[0];
        assert_eq!(row.provider_id, "jira");
        assert!(
            row.call_kind.contains("jira"),
            "a jira egress must be attributed to jira, not web: call_kind={}",
            row.call_kind
        );
        assert_eq!(row.call_kind, "jira_search");
        assert_eq!(row.destination, "Jira (connector)");
        assert_ne!(
            row.call_kind, "web_search",
            "a jira egress must NOT be mislabeled as a web search"
        );
        assert!(
            !row.destination.to_lowercase().contains("web search"),
            "a jira destination must not read as a web search: {}",
            row.destination
        );
    }

    /// (c, cont.) PER-CONNECTOR ATTRIBUTION for Slack — a `"slack"`-id connector must record its OWN
    /// truthful ledger attribution ("slack_search" / "Slack (connector)"), NOT the web-search label.
    /// Mirrors `jira_connector_search_is_attributed_to_jira_not_web`.
    #[test]
    fn slack_connector_search_is_attributed_to_slack_not_web() {
        struct SlackLikeConnector;
        #[async_trait::async_trait]
        impl Connector for SlackLikeConnector {
            fn id(&self) -> &str {
                "slack"
            }
            fn egress_class(&self) -> EgressClass {
                EgressClass::External
            }
            fn egress_attribution(&self) -> (&'static str, &'static str) {
                ("slack_search", "Slack (connector)")
            }
            async fn search(&self, _redacted_query: &str) -> ConnectorResult {
                Ok(vec![ConnectorHit {
                    title: "t".into(),
                    snippet: "s".into(),
                    url: "https://acme.slack.com/archives/C1/p1".into(),
                    source_label: "slack · fake".into(),
                }])
            }
        }
        let recorded = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let registry = ConnectorRegistry::with_parts(
            vec![Box::new(SlackLikeConnector)],
            std::sync::Arc::new(NoopNameRedactor),
            std::sync::Arc::new(CaptureEgressSink(recorded.clone())),
        );
        block_on(registry.search("slack", "what did we decide about launch")).unwrap();

        let rows = recorded.lock().unwrap();
        assert_eq!(rows.len(), 1, "exactly one ledger row for the slack search");
        let row = &rows[0];
        assert_eq!(row.provider_id, "slack");
        assert!(
            row.call_kind.contains("slack"),
            "a slack egress must be attributed to slack, not web: call_kind={}",
            row.call_kind
        );
        assert_eq!(row.call_kind, "slack_search");
        assert_eq!(row.destination, "Slack (connector)");
        assert_ne!(
            row.call_kind, "web_search",
            "a slack egress must NOT be mislabeled as a web search"
        );
        assert!(
            !row.destination.to_lowercase().contains("web search"),
            "a slack destination must not read as a web search: {}",
            row.destination
        );
    }
}
