# Reference — the provider capability seam + the ONE egress seam

Deep, code-grounded material for `/design-ai-seam` steps 2 and 3. Cite by symbol; grep before you rely
(`summarize/` files are large and drift). All symbols below verified against the current tree.

---

## A. The provider trait — add a CAPABILITY method, never a fork or a flatten

`summarize/provider.rs` defines the ONE provider abstraction:

```rust
#[async_trait]
pub trait SummarizerProvider: Send + Sync {
    fn id(&self) -> &str;                                   // "claude_code" | "anthropic" | "ollama" | "gateway"
    async fn availability(&self) -> Availability;          // cheap non-failing probe
    async fn summarize(&self, req: &SummarizeRequest) -> Result<String>;
    async fn complete(&self, system: &str, user: &str) -> Result<String>;
    // + *_with_meta variants (default-delegate, return empty CallMeta)
    // + complete_json / complete_json_with_meta (default = schema-in-prompt + parse_first_json)
    fn supports_native_json(&self) -> bool { false }       // ← the CAPABILITY-SEAM exemplar
}
```

### The capability-method pattern (the design rule)

When providers differ in a capability, express it as a **trait method with a safe default that keeps
every existing provider byte-identical**, and let only the provider that has the capability override
it. The canonical example is `supports_native_json()`:

- **Default `false`** — every provider that does schema-in-prompt + `crate::reason::parse_first_json`
  recovery gets the safe path for free (test `supports_native_json_defaults_to_false`).
- **The gateway overrides to `true`** — it sends `response_format: {"type":"json_schema", …}` in
  `complete_json_with_meta` (native constrained decoding).
- **Load-bearing note in the code:** it is a "CAPABILITY SEAM ONLY for now — nothing dispatches on it
  yet." This is the seam-when-earned discipline in action: the method exists so a future cutover CAN
  branch on it, but no caller branches until shadow data justifies it (`CloudReasoner` keeps its
  current path). Designing a capability seam ahead of its second consumer is fine ONLY when it changes
  no behavior (a default that keeps everything byte-identical) — that is exactly this.

Second exemplar — the **default-delegation** shape that keeps the trait cheap to extend:
`complete_json_with_meta` has a full default (embed the schema in the system prompt →
`complete_with_meta` → `parse_first_json`), and `complete_json` just calls it and drops the meta. A
provider with native JSON overrides ONLY `complete_json_with_meta` and both the meta and non-meta
paths become correct automatically. `summarize_with_meta`/`complete_with_meta` likewise default to
`(self.summarize(...).await?, CallMeta::default())` so a provider that doesn't capture token usage is
unchanged.

### Anti-patterns to reject in a design

- **Flatten to lowest common denominator** — e.g. removing native-JSON support so "all providers look
  the same." That strips a capable provider; the capability method exists precisely to avoid it.
- **A forked second trait / a parallel dispatch path** for the new provider. There is ONE
  `SummarizerProvider`; a new provider is a new impl + a new arm in `make_provider_resolved`, not a new
  seam.
- **A capability seam with no default** (forces every existing impl to change) — always ship the safe
  default.

### `SummarizeRequest` is the egress payload contract

`SummarizeRequest` carries the fields that reach the provider: `transcript`, `meta`, `template`,
`vault_titles`, and the three `Option<String>` grounding fields `related_context`, `user_notes`,
`live_bullets`. Each grounding field's doc-comment states the EGRESS contract: `None` ⇒ the rendered
prompt is byte-identical to before the field existed, AND `RedactingProvider` MUST scrub it. **Adding a
string field here is adding an egress field — see §C (the coverage-guard).**

---

## B. The ONE egress seam — `make_provider_resolved`

`summarize/mod.rs`. Every cloud/network provider path funnels here. The thin wrappers
`make_provider(id, config)` and `provider_for(role, config)` both resolve a `RoleTarget` and call
`make_provider_resolved(&target, config)` — so there is exactly one place the invariants live, keyed
off the RESOLVED connection (a role can never bypass them).

Inside `make_provider_resolved`, in order:

1. **Fail-closed consent gate** — `if egress_is_cloud(id, config) && !config.cloud_egress_consented →
   Err(AppError::Unavailable(...))`. No cloud provider is even CONSTRUCTED before one-time consent, so
   no content can be sent. Tests: `cloud_providers_refused_without_consent`,
   `cloud_providers_refused_after_consent_revoked`, `remote_ollama_requires_consent`.
2. **Classification** — `egress_is_cloud(id, config)`:
   - `claude_code` / `anthropic` / `gateway` → always cloud (gateway is cloud even on loopback — a
     localhost gateway can forward onward).
   - `ollama` → cloud ONLY when its base URL host is NOT loopback (a remote `ollama_base_url` is cloud
     and must be gated + redacted); unparseable URL → fail-safe cloud.
   - `roles::CONN_LOCAL` / `CONN_OFF` / `CONN_AFM` → NOT cloud (on-device; classifying them cloud would
     demand phantom consent + write a phantom ledger row + lie on the Privacy Receipt).
   - Any UNKNOWN id → cloud (fail-safe `_ => true`).
3. **Build the inner provider** (per-arm: `ClaudeCodeProvider`, `AnthropicProvider`, `OllamaProvider`,
   `OpenAiCompatProvider` gateway, or the on-device `LocalSummarizerProvider` for `CONN_LOCAL`). A
   LOCAL ollama and the `CONN_LOCAL` reasoner return UNWRAPPED here (they egress nothing).
4. **Wrap every cloud provider** in
   `RedactingProvider::with_name_redactor_and_sink(inner, active_name_redactor(), active_sink(), id,
   destination, model_requested)` — the redaction firewall + the content-free egress ledger.
   `effective_model_requested(target, config)` computes the provenance model (an empty resolved model →
   the connection's own default; the anthropic-empty-model provenance fix).

**The design rule:** a new provider is a new arm in this factory. A new SURFACE that reaches the cloud
routes through `make_provider` / `provider_for` — NEVER a hand-rolled provider or `reqwest` at the call
site. The agentic loop honors this automatically: every cloud turn re-routes through the factory, so
redaction + consent stay automatic and no NEW egress class is created (see
`agentic-loop-and-aci.md`).

---

## C. The redaction firewall + the coverage-guard (the war story)

`summarize/redact.rs`. `RedactingProvider` scrubs the outgoing `SummarizeRequest` through two layers
before the inner (cloud) provider sees it:

- **Regex layer** — `redact()` scrubs emails / cards / phones; deterministic (no model), restored in
  the reply.
- **NER name layer** — `active_name_redactor()` returns the real on-device DeBERTa PERSON-name redactor
  when the NER model is installed, else the byte-identical `NoopNameRedactor` (a no-model build's egress
  is unchanged). It only ever REMOVES content — a NER miss leaks no more than the no-op.

### The coverage-guard test — extend it on EVERY new egressing field

`every_string_field_of_summarize_request_is_scrubbed_or_exempt` is the future-proofing guard. It
constructs a `SummarizeRequest` via a struct LITERAL (there is no `Default`), puts a unique
email-shaped sentinel in every PII-bearing field, and asserts none reaches the inner provider. Because
it's a literal, **a newly-added string field forces a COMPILE ERROR here until it is explicitly
classified** — scrubbed (add a sentinel) or exempt (a documented non-PII format flag like `date_iso` /
`language`).

**Why it exists (the war story):** `RedactingProvider` scrubs an ALLOWLIST of fields. `vault_titles`
(and before it `user_notes`) once slipped through the allowlist and EGRESSED unredacted — a real leak
that a green build + lint did not catch. The literal-enumeration guard is the fix: the type system now
refuses to compile a new egress field that nobody classified.

**Design implication:** any seam that adds a field to `SummarizeRequest`, or any new payload the
provider sees, MUST (a) confirm the field is scrubbed by the firewall and (b) extend this test with a
sentinel (or document the exemption). This is a hard checklist item, not a nicety.

---

## D. Connectors — the SECOND egress seam (`EgressClass` + registry redaction)

`connectors/mod.rs`. A **connector** is a live, on-demand external tool (web / Jira / Slack / MCP)
reached through the same gated tool registry as vault reads, but it may egress — so it carries its own
firewall, structurally mirroring the provider path:

- `enum EgressClass { Local, External }` — `External` connectors are consent-gated fail-closed; a
  `Local` connector (e.g. the on-device calendar via EventKit) is exempt, exactly as loopback ollama is
  exempt.
- `ConnectorRegistry::build(config)` / `build_with_mcp(config, mcp_servers)` — a connector is EXPOSED
  ONLY when enabled + consented + keyed (else ABSENT from the brain's tool list — an un-keyed connector
  never even appears as a callable tool, matching how an un-consented provider is simply not built).
- `ConnectorRegistry::search(id, query)` is the registry boundary that:
  1. fails closed (`ConnectorError::NeedsConsent`, no network, no ledger row) if the id isn't exposed;
  2. redacts the query through the FULL firewall — `redact_connector_query(query, self.names)` (the
     same regex + NER layers as the provider path) — so an individual connector can NEVER forget it;
  3. records ONE content-free `EgressEntry` (via `active_sink()`) BEFORE the network call, using the
     connector's OWN `egress_attribution()` (`call_kind`, `destination`) so a Jira egress is labeled
     Jira and never masquerades as a web search — carrying byte SIZE + redaction COUNTS, never the text.

**Design rule:** a new external source is a new `Connector` impl declaring its `EgressClass` +
`egress_attribution` and slotting into `ConnectorRegistry::build`. The redaction + ledger are the
framework's job, not the connector's — never redact/ledger inside the connector, never reach the
network outside `ConnectorRegistry::search`.

### The two egress seams, side by side

| | Provider egress | Connector egress |
| --- | --- | --- |
| Factory / boundary | `make_provider_resolved` | `ConnectorRegistry::search` |
| Classification | `egress_is_cloud(id, config)` | `Connector::egress_class()` |
| Consent | `config.cloud_egress_consented` | per-connector enable + consent flag |
| Firewall | `RedactingProvider` (regex + NER) | `redact_connector_query` (regex + NER) |
| Ledger | `active_sink()` inside the wrap | `active_sink().record()` at the boundary |
| Fail-closed default | unknown id → cloud; refuse w/o consent | id not exposed → `NeedsConsent`, no egress |

Both keep the invariant: **the call site can't reach the network any other way, and can't forget the
firewall or the ledger.**
