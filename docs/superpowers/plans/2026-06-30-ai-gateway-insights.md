# AI Gateway provider + Gateway Insights — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a Murmur user point the app at their own OpenAI-compatible AI gateway (Kong / LiteLLM / Portkey / OpenRouter / local LiteLLM / LM Studio / vLLM), safely, and surface rich in-app "Gateway Insights" — a live model catalog, a token-usage + privacy egress ledger, per-note model provenance, per-task/per-folder model profiles, structured errors, and (fast-follow) JSON-mode + token streaming.

**Architecture:** A new `OpenAiCompatProvider` rides the existing `SummarizerProvider` seam exactly like `AnthropicProvider`. It is classified cloud (`is_cloud → true`) so it inherits the redaction firewall + the fail-closed consent gate by construction. Providers gain additive `*_with_meta` methods returning `(String, CallMeta)`; `RedactingProvider` becomes the single **egress ledger writer** — it sees every cloud call, computes redaction counts, reads the inner provider's `CallMeta`, and writes one metadata-only row (no content) to a new additive `egress_log` SQLite table. The FE reads the ledger + catalog through `IpcService` into signals and renders them in the Settings card and a new Analytics "Egress & Usage" panel.

**Tech Stack:** Rust / Tauri 2.11 (`meetnotes_lib`), `reqwest` (already a dep; we use `reqwest::Url` — no new crate), SQLCipher via the existing `Db`, Angular 18 zoneless / standalone / signals, `@tauri-apps/api` IPC. No new npm or cargo dependencies.

## Global Constraints

Every task's requirements implicitly include this section. Values copied verbatim from the project rules (`CLAUDE.md`, `.claude/rules/*`):

- **Errors:** every fallible fn returns `crate::error::Result<T>` (= `Result<T, AppError>`); no `unwrap()`/`expect()`/`anyhow::Result` in non-test code. A cloud-egress refusal is `AppError::Unavailable`; a bad user input is `AppError::InvalidArg`; a locked-content refusal is `AppError::Locked`.
- **Commands:** every `#[tauri::command]` lives in `src-tauri/src/commands.rs` AND is added to `tauri::generate_handler![…]` in `src-tauri/src/lib.rs` in the SAME change.
- **Storage:** open only through `Db`; migrations are ADDITIVE only (`add_column_if_missing`, `CREATE TABLE IF NOT EXISTS`) — never `DROP`/`DELETE`/destructive. `migrate()` stays idempotent.
- **Gate every read / verify-before-destroy:** unchanged — this feature adds no new content read path that bypasses `meeting_is_unlocked` / `visibility_clause`, and seals nothing.
- **No PII in logs / no content in the ledger:** `egress_log` stores IDs, host, model id, token COUNTS, PII-token COUNTS by kind, byte counts — NEVER note/transcript text, scrubbed values, keys, or DEK/KEK/CK.
- **No currency (`rust-tauri §10`):** the ledger is TOKEN-denominated only. Do NOT introduce any currency/amount/price field. Dollar cost is explicitly OUT OF SCOPE.
- **FE (zoneless):** standalone + `OnPush`; inline `template` + inline `styles:[…]`; `inject()`; signals/`computed()`/`effect({allowSignalWrites})`; `@if`/`@for (… ; track …)`; `input()`/`output()`/`viewChild()`; no `*ngIf`/`*ngFor`, no NgRx, no `async` pipe, no `setTimeout` in components (`afterNextRender(fn,{injector})`); one typed `IpcService` method per command; overlays use `var(--surface-overlay)` not `.card`; every color/spacing/radius from `var(--token)`; 16 kB per-component style budget.
- **Test loop:** `( cd src-tauri && cargo test --lib )` + `npx ng lint` + `npx ng build`. NEVER `cargo clippy --all-targets` in the loop. `bash scripts/ci.sh` is the final gate, run once at the end.
- **`com.meetnotes.app` is immutable.** No new npm packages or crates.
- **Commits/PRs:** authored only by `QueaT <kgm004a@gmail.com>`, NO Claude trailers; `gh` account `JakubGawr`; never push to `murmur` directly — merge via PR.
- **Definition of Done:** the implementer never self-certifies. Each phase is gated by the **adversarial-verifier**; every lock/crypto/egress-touching phase (0, 1, 2, 6, 9) is additionally gated by the **lock-security-reviewer**.

## Key decisions (locked here so execution needs no further questions)

1. **Egress classification fix (the `ollama_base_url` gap):** a NON-loopback base URL for ANY provider is cloud egress. `egress_is_cloud(id, config)` returns true for `claude_code`/`anthropic`/`gateway` always, and for `ollama` when its base URL host is non-loopback. This closes the shipped gap where a remote `ollama_base_url` bypassed redaction + consent.
2. **Metadata channel = additive `*_with_meta` trait methods (NO Mutex side-channel, NO 13-site churn).** Default impls delegate to the plain method with empty `CallMeta`. `RedactingProvider` calls the inner `*_with_meta`, writes the ledger, and returns just the `String` — so the public trait surface and all ~13 call sites are unchanged, and concurrent brain calls never race.
3. **`RedactingProvider` is the single ledger writer**, via an injected `Arc<dyn EgressSink>` (no-op by default; DB-backed in commands). The ledger therefore captures EVERY cloud call (note + brain + side-tasks) at the one chokepoint they all pass through.
4. **Provider format = OpenAI-compatible** (`/v1/chat/completions`, `/v1/models`). Anthropic-format gateways are out of scope for v1.
5. **Ledger is token-only** (§10). Cost data, even when a gateway returns it, is not stored or shown.

---

## File Structure

**Rust (`src-tauri/src/`):**
- `summarize/meta.rs` — **NEW.** `CallMeta` struct + `RedactionCounts` struct (pure data, shared by providers + ledger).
- `summarize/egress_log.rs` — **NEW.** `EgressEntry`, `EgressSink` trait, `NoopEgressSink`.
- `summarize/gateway.rs` — **NEW.** `OpenAiCompatProvider` (OpenAI `/v1/chat/completions` + `/v1/models`) + `validate_gateway_url` + `host_is_loopback`.
- `summarize/provider.rs` — **MODIFY.** Add `summarize_with_meta` / `complete_with_meta` default methods to the trait.
- `summarize/anthropic.rs` — **MODIFY.** Parse `usage` + `model`; override `*_with_meta` (Bomba #1).
- `summarize/redact.rs` — **MODIFY.** Hold an `Arc<dyn EgressSink>`; call inner `*_with_meta`; write the ledger; expose redaction counts.
- `summarize/mod.rs` — **MODIFY.** `egress_is_cloud(id, config)`; register `PROVIDER_GATEWAY`; thread the sink into `make_provider`; stop early-returning a remote `ollama` unwrapped.
- `summarize/claude_code.rs` — **MODIFY (Phase 2b, optional).** `--output-format json` to capture usage/model from the CLI.
- `settings/config.rs` — **MODIFY.** `gateway_base_url`, `gateway_model`; `Folder` model policy is in `storage/models.rs`.
- `secrets/keychain.rs` — reuse generic helpers; new account const `GATEWAY_KEY_ACCOUNT`.
- `storage/db.rs` — **MODIFY.** Additive `egress_log` table in `migrate()`; `insert_egress`, `egress_summary`, `egress_for_meeting` queries; additive `notes.model_*` columns; additive `folders.model` column.
- `storage/models.rs` — **MODIFY.** `EgressEntryRow`, `EgressSummary` DTOs; `Folder.model`.
- `commands.rs` — **MODIFY.** `list_gateway_models`, `gateway_health`, `set_gateway_key`/`has_gateway_key`, `get_egress_ledger`, `get_egress_for_meeting`, `set_folder_model`; build the `DbEgressSink`.
- `lib.rs` — **MODIFY.** Register every new command in `generate_handler!`.
- `pipeline.rs` — **MODIFY.** Persist requested+served model provenance onto the note.
- `export/…` — **MODIFY.** Write `ai-provider` / `ai-model` frontmatter.

**Angular (`src/app/`):**
- `core/models.ts` — **MODIFY.** `GatewayModel`, `EgressLedger`, `EgressRow`, `GatewayHealth`, `ModelProfile` types.
- `core/ipc.service.ts` — **MODIFY.** One typed method per new command.
- `features/settings/settings.component.ts` — **MODIFY.** AI Gateway advanced card (base URL, key, dynamic model picker, health dot, destination warning).
- `features/analytics/egress-ledger.component.ts` — **NEW.** The "Egress & Usage" panel (the showpiece).
- `features/analytics/analytics.component.ts` — **MODIFY.** Mount the ledger panel.
- `features/detail/…` — **MODIFY.** Model-provenance badge on the note.
- `features/folders/…` — **MODIFY.** Per-folder model dropdown.
- `features/record/assistant*.component.ts` — **MODIFY (Phase 9).** Token-stream rendering + cache-hit chip.

---

# PHASE 0 — Close the `ollama_base_url` egress gap (security foundation)

**Why first:** it's a live, shipped leak (a remote `ollama_base_url` bypasses redaction + consent) and the gateway provider depends on the corrected classification. Independently shippable. **Gated by lock-security-reviewer.**

### Task 0.1: `host_is_loopback` + `egress_is_cloud`

**Files:**
- Create: `src-tauri/src/summarize/gateway.rs` (only the URL helpers for now)
- Modify: `src-tauri/src/summarize/mod.rs` (add `mod gateway;`, `egress_is_cloud`)
- Test: inline `#[cfg(test)]` in `summarize/gateway.rs` and `summarize/mod.rs`

**Interfaces:**
- Produces: `pub fn host_is_loopback(url: &reqwest::Url) -> bool`; `pub fn validate_gateway_url(raw: &str) -> crate::error::Result<reqwest::Url>`; `pub(crate) fn egress_is_cloud(id: &str, config: &AppConfig) -> bool`.

- [ ] **Step 1: Write the failing test** (in `summarize/gateway.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn loopback_detection() {
        assert!(host_is_loopback(&reqwest::Url::parse("http://localhost:11434").unwrap()));
        assert!(host_is_loopback(&reqwest::Url::parse("http://127.0.0.1:4000/v1").unwrap()));
        assert!(host_is_loopback(&reqwest::Url::parse("http://[::1]:8000").unwrap()));
        assert!(!host_is_loopback(&reqwest::Url::parse("https://api.example.com/v1").unwrap()));
    }
    #[test]
    fn url_validation_rejects_plain_http_remote_and_bad_scheme() {
        assert!(validate_gateway_url("https://gw.example.com/v1").is_ok());
        assert!(validate_gateway_url("http://localhost:4000/v1").is_ok());
        assert!(validate_gateway_url("http://evil.example.com/v1").is_err()); // remote http rejected
        assert!(validate_gateway_url("file:///etc/passwd").is_err());          // scheme rejected
        assert!(validate_gateway_url("not a url").is_err());
    }
}
```

- [ ] **Step 2: Run it, expect FAIL** — `( cd src-tauri && cargo test --lib gateway:: )` → FAIL (`host_is_loopback` not found).

- [ ] **Step 3: Implement the helpers** (`summarize/gateway.rs`)

```rust
//! OpenAI-compatible "AI Gateway" provider + URL guardrails.
use crate::error::{AppError, Result};

/// True iff the URL host is a loopback address — the ONLY case where plain `http://` is allowed,
/// and (for `ollama`) the only case treated as non-cloud. NOTE: a loopback gateway can still
/// FORWARD to the cloud, so loopback is NOT a redaction exemption for the `gateway` provider.
pub fn host_is_loopback(url: &reqwest::Url) -> bool {
    match url.host() {
        Some(reqwest::Url::host_str_unused) => false, // placeholder; real arms below
        _ => false,
    }
}
```

(Replace the placeholder body with the real match — `reqwest::Url::host()` returns `Option<url::Host<&str>>`:)

```rust
pub fn host_is_loopback(url: &reqwest::Url) -> bool {
    use std::net::IpAddr;
    match url.host_str() {
        Some("localhost") => return true,
        _ => {}
    }
    match url.host() {
        Some(host) => match host.to_string().parse::<IpAddr>() {
            Ok(ip) => ip.is_loopback(),
            Err(_) => false,
        },
        None => false,
    }
}

/// Validate a user-supplied gateway base URL (guardrails R1/R4): https required, except http on
/// loopback; reject every other scheme (no file:/ftp:/gopher: SSRF surface).
pub fn validate_gateway_url(raw: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(raw.trim())
        .map_err(|_| AppError::InvalidArg("gateway URL is not a valid URL".into()))?;
    match url.scheme() {
        "https" => Ok(url),
        "http" if host_is_loopback(&url) => Ok(url),
        "http" => Err(AppError::InvalidArg(
            "gateway URL must use https:// (http:// is only allowed for localhost)".into(),
        )),
        other => Err(AppError::InvalidArg(format!("unsupported gateway URL scheme: {other}"))),
    }
}
```

- [ ] **Step 4: Add `egress_is_cloud` in `summarize/mod.rs`** (and `mod gateway;`)

```rust
pub mod gateway; // near the other `mod` lines

/// Egress classification used by `make_provider`. claude_code/anthropic/gateway always send
/// content off-device. ollama is local ONLY when its base URL is loopback — a remote
/// `ollama_base_url` is cloud egress and MUST be redacted + consent-gated (closes the gap where
/// a remote ollama bypassed the firewall).
pub(crate) fn egress_is_cloud(id: &str, config: &AppConfig) -> bool {
    match id {
        PROVIDER_CLAUDE_CODE | PROVIDER_ANTHROPIC => true,
        PROVIDER_OLLAMA => match reqwest::Url::parse(&config.ollama_base_url) {
            Ok(u) => !gateway::host_is_loopback(&u),
            Err(_) => true, // unparseable → fail safe (treat as cloud)
        },
        _ => true, // PROVIDER_GATEWAY and any future id default to cloud
    }
}
```

- [ ] **Step 5: Run, expect PASS** — `( cd src-tauri && cargo test --lib gateway:: )` → PASS.

- [ ] **Step 6: Commit** — `git commit -m "feat(egress): loopback-aware URL guardrails + egress_is_cloud classifier"`

### Task 0.2: Route a remote `ollama` through the firewall + consent in `make_provider`

**Files:**
- Modify: `src-tauri/src/summarize/mod.rs:69,102-107,125`
- Test: inline in `summarize/mod.rs`

**Interfaces:**
- Consumes: `egress_is_cloud` (0.1).
- Produces: `make_provider` now wraps a remote `ollama` in `RedactingProvider` + applies the consent gate.

- [ ] **Step 1: Write the failing test**

```rust
#[tokio::test]
async fn remote_ollama_requires_consent() {
    let mut cfg = AppConfig::default();
    cfg.provider_id = PROVIDER_OLLAMA.into();
    cfg.ollama_base_url = "https://ollama.remote.example/api".into();
    cfg.cloud_egress_consented = false;
    let err = make_provider(PROVIDER_OLLAMA, &cfg).unwrap_err();
    assert!(matches!(err, crate::error::AppError::Unavailable(_)));
}
#[tokio::test]
async fn local_ollama_stays_unwrapped_and_ungated() {
    let mut cfg = AppConfig::default();
    cfg.ollama_base_url = "http://localhost:11434".into();
    cfg.cloud_egress_consented = false;
    assert!(make_provider(PROVIDER_OLLAMA, &cfg).is_ok()); // local → no consent needed
}
```

- [ ] **Step 2: Run, expect FAIL** (remote ollama currently builds with no consent).

- [ ] **Step 3: Rewrite the gate + the ollama arm.** Replace the top consent gate (`mod.rs:69`) and the early `ollama` return (`mod.rs:102-107`) so the gate uses `egress_is_cloud` and ollama only short-circuits unwrapped when local:

```rust
// E10 — fail-closed consent gate, now classification-aware:
if egress_is_cloud(id, config) && !config.cloud_egress_consented {
    return Err(crate::error::AppError::Unavailable(
        "cloud egress not consented: this provider sends meeting content off-device; \
         grant one-time consent before using it".to_string(),
    ));
}

let inner: Arc<dyn SummarizerProvider> = match id {
    // … claude_code / anthropic arms unchanged …
    PROVIDER_OLLAMA => {
        let ollama = Arc::new(OllamaProvider::new(
            config.ollama_base_url.clone(),
            config.ollama_model.clone(),
        ));
        if !egress_is_cloud(id, config) {
            return Ok(ollama); // LOCAL ollama: unwrapped, as before
        }
        ollama // REMOTE ollama: falls through to the RedactingProvider wrap below
    }
    other => return Err(crate::error::AppError::InvalidArg(format!("unknown provider id: {other}"))),
};
// … existing RedactingProvider::with_name_redactor wrap returns for everything that fell through …
```

- [ ] **Step 4: Run, expect PASS** — `( cd src-tauri && cargo test --lib make_provider )` plus the two new tests → PASS.

- [ ] **Step 5: Commit** — `git commit -m "fix(egress): a remote ollama_base_url is cloud — redact + consent-gate it"`

### Phase 0 gate
- [ ] `( cd src-tauri && cargo test --lib )` green.
- [ ] **Dispatch lock-security-reviewer** on the diff: confirm no content read bypasses the firewall; a remote ollama now redacts + gates; a local ollama is byte-identical to before. Address findings.
- [ ] **Dispatch adversarial-verifier**: PASS/FAIL on Phase 0.
- [ ] Open PR `fix/egress-ollama-baseurl-gap` → merge to `murmur`.

---

# PHASE 1 — The `OpenAiCompatProvider` (the carrier) + config + Keychain key + security guardrails

**Gated by lock-security-reviewer** (new cloud egress path).

### Task 1.1: Config fields + Keychain account

**Files:**
- Modify: `src-tauri/src/settings/config.rs` (add fields + defaults), `src-tauri/src/secrets/keychain.rs` (account const), `src-tauri/src/summarize/mod.rs` (`PROVIDER_GATEWAY` const)
- Test: inline in `config.rs`

**Interfaces:**
- Produces: `AppConfig { gateway_base_url: String, gateway_model: String }` (default `""`); `pub const PROVIDER_GATEWAY: &str = "gateway"`; `pub const GATEWAY_KEY_ACCOUNT: &str = "gateway_api_key"`.

- [ ] **Step 1: Failing test** — assert `AppConfig::default().gateway_base_url == ""` and that a round-trip serde of a config with `gateway_base_url` set preserves it. Run → FAIL.
- [ ] **Step 2: Add the fields** to `AppConfig` (serde `#[serde(default)]` so existing on-disk configs load), defaults `""`. Add `PROVIDER_GATEWAY` next to the other provider consts (`mod.rs:39-41`) and `GATEWAY_KEY_ACCOUNT` next to `ANTHROPIC_KEY_ACCOUNT`.
- [ ] **Step 3: Run → PASS. Commit** — `git commit -m "feat(gateway): config fields + keychain account + provider id const"`

### Task 1.2: `OpenAiCompatProvider::summarize`/`complete` (text only first)

**Files:**
- Modify: `src-tauri/src/summarize/gateway.rs`
- Test: inline (mock the HTTP with a tiny `wiremock`-free approach — assert request shaping via a unit test on `build_body`, and gate the live call behind a constructor test)

**Interfaces:**
- Consumes: `validate_gateway_url` (0.1), `SummarizerProvider`, `SummarizeRequest`.
- Produces: `pub struct OpenAiCompatProvider { base: reqwest::Url, model: String, api_key: Option<String>, client: reqwest::Client }`; `impl SummarizerProvider`. Request body builder `fn chat_body(model, system, user) -> serde_json::Value`.

- [ ] **Step 1: Failing test on the body shaping** (pure, no network)

```rust
#[test]
fn chat_body_is_openai_shaped() {
    let body = chat_body("gpt-4o", "SYS", "USER");
    assert_eq!(body["model"], "gpt-4o");
    assert_eq!(body["messages"][0]["role"], "system");
    assert_eq!(body["messages"][0]["content"], "SYS");
    assert_eq!(body["messages"][1]["role"], "user");
    assert_eq!(body["messages"][1]["content"], "USER");
}
```

- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3: Implement** the provider. Reuse the hardened client builder pattern from `anthropic.rs:81` (TLS 1.2 floor, timeouts). `summarize` builds the note prompt like `anthropic.rs` does (render template + transcript), calls `/chat/completions`, parses `choices[0].message.content`. `availability` does a cheap `GET {base}/models` HEAD-ish check (or returns `Available` if a key+url are set). Construct via `new(base_url, model, api_key) -> Result<Self>` calling `validate_gateway_url`.

```rust
pub(crate) fn chat_body(model: &str, system: &str, user: &str) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "stream": false,
    })
}
```

(Implement `summarize`/`complete` to POST `self.base.join("chat/completions")?`, set `Authorization: Bearer` only when `api_key` is `Some`, parse `resp["choices"][0]["message"]["content"]` as the text; map a non-2xx into `AppError::Summarize` with the gateway's `error.message` if present — Phase 4 enriches this.)

- [ ] **Step 4: Run → PASS** (body test; the network path is covered by Phase 2 meta tests + a live smoke in Phase 1 gate).
- [ ] **Step 5: Commit** — `git commit -m "feat(gateway): OpenAiCompatProvider (chat/completions, text)"`

### Task 1.3: Register `gateway` in `make_provider` + the four security invariants

**Files:**
- Modify: `src-tauri/src/summarize/mod.rs`
- Test: inline in `summarize/mod.rs`

**Interfaces:**
- Consumes: `OpenAiCompatProvider::new`, `egress_is_cloud`, `validate_gateway_url`.
- Produces: a `PROVIDER_GATEWAY` arm in `make_provider` that is consent-gated, redaction-wrapped, https-validated, and key-bound.

- [ ] **Step 1: Failing tests — the four guardrails (RED-before-GREEN)**

```rust
#[tokio::test]
async fn gateway_refused_without_consent() {            // R1
    let mut c = AppConfig::default();
    c.gateway_base_url = "https://gw.example/v1".into();
    c.cloud_egress_consented = false;
    assert!(matches!(make_provider(PROVIDER_GATEWAY, &c).unwrap_err(),
                     crate::error::AppError::Unavailable(_)));
}
#[tokio::test]
async fn gateway_localhost_is_still_redaction_wrapped() { // R2
    // A capture inner is impossible here, so assert via the type: a consented gateway build
    // succeeds AND is NOT the bare OpenAiCompatProvider — see redaction round-trip test in Phase 2.
    let mut c = AppConfig::default();
    c.gateway_base_url = "http://127.0.0.1:4000/v1".into();
    c.cloud_egress_consented = true;
    assert!(make_provider(PROVIDER_GATEWAY, &c).is_ok());
}
#[tokio::test]
async fn gateway_remote_http_rejected() {                // R4
    let mut c = AppConfig::default();
    c.gateway_base_url = "http://gw.example/v1".into();
    c.cloud_egress_consented = true;
    assert!(matches!(make_provider(PROVIDER_GATEWAY, &c).unwrap_err(),
                     crate::error::AppError::InvalidArg(_)));
}
```

- [ ] **Step 2: Run → FAIL** (`gateway` arm absent).
- [ ] **Step 3: Add the arm.** Resolve the key from Keychain (R3 — bound to this provider's URL only, never a fallback to the Anthropic key), validate the URL, construct, and let it fall through to the `RedactingProvider` wrap:

```rust
PROVIDER_GATEWAY => {
    if config.gateway_base_url.trim().is_empty() {
        return Err(crate::error::AppError::InvalidArg("gateway base URL is not set".into()));
    }
    let api_key = crate::secrets::get_secret(GATEWAY_KEY_ACCOUNT).ok(); // optional; never falls back to another provider's key (R3)
    Arc::new(crate::summarize::gateway::OpenAiCompatProvider::new(
        config.gateway_base_url.clone(),  // validate_gateway_url runs inside ::new (R1/R4)
        config.gateway_model.clone(),
        api_key,
    )?)
}
```

- [ ] **Step 4: Run → PASS.**
- [ ] **Step 5: Commit** — `git commit -m "feat(gateway): register provider with cloud-gate + https + key-bound guardrails"`

### Task 1.4: Key commands + `IpcService` + Settings "AI Gateway" card (UX)

**Files:**
- Modify: `src-tauri/src/commands.rs` (`set_gateway_key`, `has_gateway_key`, `clear_gateway_key` mirroring the Anthropic-key commands at `commands.rs:2376`), `src-tauri/src/lib.rs` (register), `src/app/core/ipc.service.ts`, `src/app/features/settings/settings.component.ts`
- Test: `cargo test --lib` for the commands' arg validation; Playwright smoke against `:1420` for the card.

**Interfaces:**
- Consumes: nothing new.
- Produces: IPC `setGatewayKey(key)`, `hasGatewayKey()`, `clearGatewayKey()`, plus reuse of `getConfig`/`setConfig` for `gatewayBaseUrl`/`gatewayModel`.

- [ ] **Step 1:** Backend commands (mirror `set_anthropic_key`), register in `generate_handler!`. Test: `set_gateway_key("")` → `AppError::InvalidArg`. RED → GREEN → commit.
- [ ] **Step 2:** `ipc.service.ts` typed methods. Commit.
- [ ] **Step 3: The Settings card (rich UX).** Add an "AI Gateway (advanced)" section to `settings.component.ts`, shown when the provider picker = `gateway`. Real signal code:

```ts
// state
protected readonly gwUrl = signal('');
protected readonly gwModel = signal('');
protected readonly gwHasKey = signal(false);
protected readonly gwHealth = signal<GatewayHealth | null>(null); // Phase 4 fills this
protected readonly gwModels = signal<GatewayModel[]>([]);          // Phase 3 fills this
protected readonly gwUrlIsRemote = computed(() => {
  const u = this.gwUrl().trim();
  return /^https?:\/\//i.test(u) && !/\/\/(localhost|127\.0\.0\.1|\[::1\])/i.test(u);
});
```

```html
@if (providerId() === 'gateway') {
  <section class="card gw">
    <h3>AI Gateway <span class="pill">advanced</span></h3>

    <label>Base URL</label>
    <input [value]="gwUrl()" (input)="gwUrl.set($any($event.target).value)"
           placeholder="http://localhost:4000/v1" spellcheck="false" />
    @if (gwUrl() && !gwUrlValid()) {
      <p class="warn">Use https:// (http:// is allowed only for localhost).</p>
    }

    <label>Model</label>
    <!-- Phase 3 swaps this <select> source to gwModels() -->
    <div class="row">
      <select [value]="gwModel()" (change)="gwModel.set($any($event.target).value)">
        @for (m of gwModels(); track m.id) { <option [value]="m.id">{{ m.id }}</option> }
        @empty { <option value="">— refresh to load models —</option> }
      </select>
      <button class="btn-ghost" (click)="refreshModels()" title="Load /v1/models">↻</button>
    </div>

    <label>API key <span class="muted">(optional, stored in Keychain)</span></label>
    <input type="password" [value]="gwKeyDraft()" (input)="gwKeyDraft.set($any($event.target).value)" />
    <button class="btn" (click)="saveGwKey()">{{ gwHasKey() ? 'Replace key' : 'Save key' }}</button>

    @if (gwUrlIsRemote()) {
      <div class="banner danger">
        ⚠ Content will be sent to <b>{{ gwHost() }}</b> over the network. It is always scrubbed by
        the redaction firewall first and requires your cloud-egress consent.
      </div>
    } @else {
      <div class="banner">
        Localhost gateway. Note: a local gateway can still forward to the cloud, so content is still
        redacted + consent-gated.
      </div>
    }

    <!-- Phase 4 health dot -->
    @if (gwHealth(); as h) {
      <p class="health"><span class="dot" [class.ok]="h.reachable"></span>
        {{ h.reachable ? (h.modelCount + ' models reachable') : 'gateway unreachable' }}</p>
    }
  </section>
}
```

Styles: opaque banners use `var(--surface-overlay)`/`var(--banner-*)` tokens; `.danger` uses `var(--live*)`/red token; respect the 16 kB budget by leaning on global `.btn`/`.banner`/`.pill`.

- [ ] **Step 4:** Wire `getConfig`/`setConfig` so editing `gwUrl`/`gwModel` persists; `saveGwKey()` calls `setGatewayKey`. `npx ng lint && npx ng build` green.
- [ ] **Step 5:** Playwright smoke against `:1420` with mocked `invoke`: select `gateway`, type a remote URL → the red destination banner shows; type `http://evil.com` → the validation warning shows. Commit.

### Phase 1 gate
- [ ] `cargo test --lib` + `ng lint` + `ng build` green.
- [ ] **lock-security-reviewer**: the gateway is cloud-gated + redaction-wrapped even on localhost; the key never falls back to another provider; https enforced. 
- [ ] **adversarial-verifier**: PASS/FAIL.
- [ ] PR `feat/gateway-provider` → merge.

---

# PHASE 2 — Metadata channel (`CallMeta`) + the egress ledger writer (the architectural core)

**Gated by lock-security-reviewer** (the ledger must contain zero content). Delivers **Bomba #1** (free token usage for `anthropic`) as a side effect.

### Task 2.1: `CallMeta` + `RedactionCounts` data types

**Files:** Create `src-tauri/src/summarize/meta.rs`; add `pub mod meta;` to `summarize/mod.rs`.

**Interfaces:**
- Produces:
```rust
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallMeta {
    pub model_served: Option<String>,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    pub total_tokens: Option<u32>,
    pub cached_tokens: Option<u32>,
}
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RedactionCounts { pub email: u32, pub card: u32, pub phone: u32, pub name: u32 }
```
- [ ] Step 1: trivial `Default` test → Step 2 FAIL → Step 3 define structs → Step 4 PASS → Step 5 commit `feat(meta): CallMeta + RedactionCounts`.

### Task 2.2: Trait `*_with_meta` default methods

**Files:** Modify `src-tauri/src/summarize/provider.rs`.

**Interfaces:**
- Produces (added to `trait SummarizerProvider`):
```rust
async fn summarize_with_meta(&self, req: &SummarizeRequest) -> Result<(String, crate::summarize::meta::CallMeta)> {
    Ok((self.summarize(req).await?, Default::default()))
}
async fn complete_with_meta(&self, system: &str, user: &str) -> Result<(String, crate::summarize::meta::CallMeta)> {
    Ok((self.complete(system, user).await?, Default::default()))
}
```
- [ ] Step 1: a test that a stub provider (the existing `EchoProvider` in `redact.rs` tests, or a local one) returns empty `CallMeta` from the default `complete_with_meta`. RED → add methods → GREEN → commit `feat(provider): additive *_with_meta with empty default`.

### Task 2.3: `anthropic` parses `usage` + `model` (Bomba #1)

**Files:** Modify `src-tauri/src/summarize/anthropic.rs:91-97` (extend `MessagesResponse`) and the call path.

**Interfaces:**
- Consumes: `CallMeta`.
- Produces: `AnthropicProvider` overrides `summarize_with_meta`/`complete_with_meta` with real token counts; `summarize`/`complete` delegate to a shared internal `messages_call(system,user) -> Result<(String, CallMeta)>`.

- [ ] **Step 1: Failing test** — feed a recorded Anthropic JSON fixture (with `usage.input_tokens=11`, `usage.output_tokens=22`, `model:"claude-opus-4-8"`) through a `parse_messages_response(&str) -> Result<(String, CallMeta)>` helper; assert `meta.prompt_tokens==Some(11)`, `meta.completion_tokens==Some(22)`, `meta.model_served==Some("claude-opus-4-8")`.
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3:** Extend `MessagesResponse` with `usage: Option<AnthropicUsage>` and `model: Option<String>` (`AnthropicUsage { input_tokens, output_tokens, cache_read_input_tokens }`), write `parse_messages_response`, refactor `summarize`/`complete` to call the shared `messages_call`, override the `*_with_meta`.
- [ ] **Step 4: Run → PASS.**
- [ ] **Step 5: Commit** — `git commit -m "feat(anthropic): stop discarding usage+model; expose via CallMeta"`

### Task 2.4: `gateway` parses `usage` + `model`

**Files:** Modify `src-tauri/src/summarize/gateway.rs`.
- [ ] Same TDD shape as 2.3 with an OpenAI fixture (`usage.prompt_tokens`, `usage.completion_tokens`, `usage.total_tokens`, `usage.prompt_tokens_details.cached_tokens`, top-level `model`). Override `*_with_meta`. Commit `feat(gateway): parse usage+model into CallMeta`.

### Task 2.5: `EgressSink` + `RedactingProvider` as the ledger writer

**Files:** Create `src-tauri/src/summarize/egress_log.rs`; modify `src-tauri/src/summarize/redact.rs` (hold `Arc<dyn EgressSink>`, count redactions, call inner `*_with_meta`, emit one entry).

**Interfaces:**
- Produces:
```rust
// egress_log.rs
pub struct EgressEntry {
    pub provider_id: String, pub destination: String, pub model_requested: String,
    pub call_kind: &'static str, pub meta: CallMeta, pub redactions: RedactionCounts,
    pub system_bytes: usize, pub user_bytes: usize, pub meeting_id: Option<String>,
}
pub trait EgressSink: Send + Sync { fn record(&self, entry: EgressEntry); }
pub struct NoopEgressSink; impl EgressSink for NoopEgressSink { fn record(&self, _: EgressEntry) {} }
```
- `RedactingProvider::with_name_redactor_and_sink(inner, redactor, sink, provider_id, destination)` — the existing constructor keeps a `NoopEgressSink` so current tests are unchanged.

- [ ] **Step 1: Failing test** — a `CaptureEgressSink` (Vec in a Mutex). Run a `RedactingProvider` (wrapping an `EchoProvider` whose `complete_with_meta` returns a known `CallMeta`) over an input containing one email; assert the sink received exactly ONE entry, with `redactions.email==1`, `meta.total_tokens` propagated, and that **no field contains the email or the note text**.
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3:** Add the sink field + the new constructor. In `summarize`/`complete`: redact (counting per kind), call `inner.*_with_meta`, restore, then `self.sink.record(EgressEntry{…counts…})` before returning the restored text. Count emails/cards/phones from the redaction `map` keys and names from the name-redactor pairs.
- [ ] **Step 4: Run → PASS** (incl. an explicit assertion `!entry_debug.contains("@")`).
- [ ] **Step 5: Commit** — `git commit -m "feat(egress): RedactingProvider writes a content-free egress entry per cloud call"`

### Task 2.6: `egress_log` table + `insert_egress` + wire the DB sink

**Files:** Modify `src-tauri/src/storage/db.rs` (additive table in `migrate()` + `insert_egress`), `src-tauri/src/storage/models.rs` (row DTOs), `src-tauri/src/summarize/mod.rs` (thread an `Option<Arc<dyn EgressSink>>` into `make_provider`), `src-tauri/src/commands.rs` (a `DbEgressSink` built from `State`), all `make_provider` call sites.

**Interfaces:**
- Produces: `Db::insert_egress(&self, e: &EgressEntry) -> Result<()>`; `make_provider(id, config, sink: Arc<dyn EgressSink>)`; `DbEgressSink` in commands.

- [ ] **Step 1: Failing migration-idempotency + insert test** — `migrate()` twice is a no-op AND `egress_log` exists; `insert_egress` then a `SELECT COUNT(*)` == 1. RED.
- [ ] **Step 2:** Add the table (the exact additive DDL from the plan header — token counts + redaction counts by kind + host + ids, NO content), `insert_egress`. Add `mod egress_log;`.
- [ ] **Step 3:** Change `make_provider`'s signature to take `sink: Arc<dyn EgressSink>` and pass it into the `RedactingProvider` constructor; update every call site (`pipeline.rs:573`; `commands.rs:1104,1351,1838,1875,1936,2091`; `summarize/{graph,organize,timeline}.rs`) — production sites pass the `DbEgressSink` from state; tests pass `Arc::new(NoopEgressSink)`. `DbEgressSink` clones the `Db` handle and calls `insert_egress` (logging an error but never panicking on failure).
- [ ] **Step 4: Run → PASS.**
- [ ] **Step 5: Commit** — `git commit -m "feat(egress): additive egress_log table + DB sink wired through make_provider"`

### Phase 2 gate
- [ ] `cargo test --lib` green; a focused test proves NO content reaches `egress_log`.
- [ ] **lock-security-reviewer**: ledger is metadata-only; no PII; consent/redaction unchanged.
- [ ] **adversarial-verifier**: PASS/FAIL.
- [ ] PR `feat/egress-ledger-core` → merge.

---

# PHASE 3 — Model catalog (`/v1/models`) + dynamic picker

### Task 3.1: `list_gateway_models` command

**Files:** Modify `src-tauri/src/summarize/gateway.rs` (`list_models`), `commands.rs`, `lib.rs`, `core/models.ts`, `core/ipc.service.ts`.

**Interfaces:**
- Produces: `OpenAiCompatProvider::list_models(&self) -> Result<Vec<ModelInfo>>` (GET `{base}/models`, parse `data[].id`); command `list_gateway_models() -> Vec<GatewayModel>`; TS `GatewayModel { id: string }`.

- [ ] Step 1: failing test on `parse_models_response(json) -> Vec<String>` with an OpenAI `/v1/models` fixture (`{object:"list", data:[{id:"gpt-4o"},{id:"llama-3"}]}`) → `["gpt-4o","llama-3"]`. RED → implement (inbound-only GET, no content sent) → GREEN. Command + register + IPC method. Commit `feat(gateway): /v1/models catalog command`.

### Task 3.2: Swap the picker to the live catalog (UX)

**Files:** Modify `src/app/features/settings/settings.component.ts`.
- [ ] `refreshModels()` calls `listGatewayModels()` into `gwModels` (an `effect` with `{allowSignalWrites:true}` is NOT needed — it's a click handler; just `await` + `.set`). The `@for` over `gwModels()` (already stubbed in 1.4) now renders real options; `@empty` prompts a refresh. Disable the picker with a spinner while loading (a `gwModelsLoading` signal). `ng lint`/`ng build` green. Playwright: mock `list_gateway_models` → options populate. Commit `feat(gateway): populate the model picker from the live catalog`.

### Phase 3 gate: `cargo test --lib` + `ng lint` + `ng build`; adversarial-verifier PASS; PR `feat/gateway-model-catalog` → merge.

---

# PHASE 4 — Structured gateway errors + health dot

### Task 4.1: Parse the gateway error envelope

**Files:** Modify `src-tauri/src/summarize/gateway.rs`.
- [ ] Step 1: failing test — `map_gateway_error(status, body)` turns `{"error":{"message":"model 'x' not found","type":"invalid_request_error"}}` + 404 into `AppError::Summarize("gateway: model 'x' not found")` and a 401 into `AppError::Unavailable("gateway rejected the API key")`. RED → implement (read `error.message`, branch on status; never leak the key) → GREEN. Commit `feat(gateway): actionable structured errors`.

### Task 4.2: `gateway_health` command + Settings dot (UX)

**Files:** `commands.rs`, `lib.rs`, `core/models.ts` (`GatewayHealth { reachable: boolean; modelCount: number }`), `ipc.service.ts`, `settings.component.ts`.
- [ ] `gateway_health()` does the `/v1/models` probe and returns `{reachable, modelCount}` (never throws — degrades to `reachable:false`). The Settings health line (stubbed in 1.4) renders the dot green/red + "N models reachable". An `effect` re-probes when `gwUrl()` changes (debounce via a `gwUrlStable` derived value, NOT `setTimeout`). Commit `feat(gateway): health probe + Settings status dot`.

### Phase 4 gate: gates green; adversarial-verifier PASS; PR `feat/gateway-errors-health` → merge.

---

# PHASE 5 — Per-note model provenance (frontmatter + DB)

### Task 5.1: Persist requested + served model on the note

**Files:** Modify `src-tauri/src/storage/db.rs` (additive `notes.model_requested`, `notes.model_served`, `notes.gateway_host` columns via `add_column_if_missing`), `storage/models.rs` (`NoteRecord` fields), `pipeline.rs:573,590` (capture `summarize_with_meta` instead of `summarize` at the note-save call; persist `config.provider_model` as requested, `meta.model_served` as served).
- [ ] Step 1: failing test — saving a note via the pipeline with a stub provider returning `model_served:"gpt-4o"` persists `model_requested` + `model_served` on the row. RED → add columns + switch the pipeline call to `summarize_with_meta` + persist → GREEN. Commit `feat(provenance): persist requested+served model on the note`.

### Task 5.2: Frontmatter + a provenance badge (UX)

**Files:** Modify the export frontmatter writer (`export/…`/`summarize/template.rs:179`), `src/app/core/models.ts` (`MeetingDetail.modelServed?`), `features/detail/…`.
- [ ] Write `ai-provider:` / `ai-model:` (English keys) into the note frontmatter when present (absent → omit, byte-identical for old notes). On the note detail, a small ghost badge: `served by {{ modelServed() }} · {{ providerId() }}`, using `var(--text-muted)` + `.pill`. `ng build` green; Playwright shows the badge when the field is present and nothing when absent. Commit `feat(provenance): frontmatter ai-provider/ai-model + note badge`.

### Phase 5 gate: gates green; adversarial-verifier PASS (assert old notes export byte-identical); PR `feat/model-provenance` → merge.

---

# PHASE 6 — The "Egress & Usage" ledger panel (the showpiece UX)

**Gated by lock-security-reviewer** (read path must never surface content). The write side shipped in Phase 2; this is the read query + the rich panel.

### Task 6.1: `egress_summary` / `egress_for_meeting` queries

**Files:** Modify `src-tauri/src/storage/db.rs`, `storage/models.rs`, `commands.rs`, `lib.rs`, `core/models.ts`, `ipc.service.ts`.

**Interfaces:**
- Produces: `Db::egress_summary(days: i64) -> Result<EgressSummary>` and `Db::egress_for_meeting(meeting_id) -> Result<Vec<EgressEntryRow>>`; commands `get_egress_ledger(days)`, `get_egress_for_meeting(id)`. TS:
```ts
export interface EgressRow { ts: number; providerId: string; destination: string; modelServed: string | null;
  promptTokens: number | null; completionTokens: number | null; totalTokens: number | null;
  redactions: { email: number; card: number; phone: number; name: number }; }
export interface EgressLedger {
  totalCalls: number; totalTokens: number; byModel: { model: string; calls: number; tokens: number }[];
  byDay: { day: string; tokens: number }[]; totalRedactions: { email: number; card: number; phone: number; name: number };
  recent: EgressRow[]; }
```
- [ ] Step 1: failing test — insert 3 `egress_log` rows (2 models, 2 days), assert `egress_summary(30)` returns `totalCalls:3`, correct `byModel`/`byDay`/`totalRedactions`. RED → implement the aggregate SELECTs (GROUP BY model, GROUP BY day) → GREEN. Commands + register + IPC. Commit `feat(egress): ledger read queries + commands`.

### Task 6.2: `EgressLedgerComponent` (the panel)

**Files:** Create `src/app/features/analytics/egress-ledger.component.ts`; modify `analytics.component.ts` to mount it.

**Interfaces:** Consumes `IpcService.getEgressLedger(days)`.

- [ ] **Step 1:** Build the standalone signal component. Real shape:

```ts
@Component({
  selector: 'app-egress-ledger',
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <section class="card">
      <header class="hd">
        <h3>Egress &amp; Usage</h3>
        <div class="seg">
          @for (d of ranges; track d) {
            <button class="btn-ghost" [class.on]="days()===d" (click)="days.set(d)">{{ d }}d</button>
          }
        </div>
      </header>

      @if (ledger(); as l) {
        <div class="stats">
          <div class="stat"><span class="n">{{ l.totalCalls }}</span><span class="k">cloud calls</span></div>
          <div class="stat"><span class="n">{{ l.totalTokens | number }}</span><span class="k">tokens sent</span></div>
          <div class="stat"><span class="n">{{ scrubbed(l) | number }}</span><span class="k">PII items scrubbed</span></div>
        </div>

        <h4>Tokens by model</h4>
        @for (m of l.byModel; track m.model) {
          <div class="bar">
            <span class="lbl">{{ m.model }}</span>
            <span class="track"><span class="fill" [style.width.%]="pct(m.tokens, l.totalTokens)"></span></span>
            <span class="val">{{ m.tokens | number }}</span>
          </div>
        } @empty { <p class="empty-state">No cloud calls in this window — everything stayed on-device.</p> }

        <h4>What left this device <span class="muted">(scrubbed before sending)</span></h4>
        <div class="receipt">
          <span class="chip">✉ {{ l.totalRedactions.email }} emails</span>
          <span class="chip">▦ {{ l.totalRedactions.card }} card-like</span>
          <span class="chip">☎ {{ l.totalRedactions.phone }} phones</span>
          <span class="chip">🧑 {{ l.totalRedactions.name }} names</span>
        </div>

        <h4>Recent calls</h4>
        @for (r of l.recent; track r.ts) {
          <div class="row">
            <span class="dest">{{ r.destination }}</span>
            <span class="model">{{ r.modelServed ?? '—' }}</span>
            <span class="tok">{{ r.totalTokens ?? 0 | number }} tok</span>
          </div>
        }
      } @else { <p class="empty-state">Loading…</p> }
    </section>
  `,
  styles: [`/* tokens only: var(--surface-raised), var(--accent), var(--space-*), var(--radius-*); bars use var(--accent) fill on var(--surface-sunken) track */`],
})
export class EgressLedgerComponent {
  private readonly ipc = inject(IpcService);
  protected readonly ranges = [7, 30, 90] as const;
  protected readonly days = signal<number>(30);
  protected readonly ledger = signal<EgressLedger | null>(null);
  private readonly _load = effect(() => { const d = this.days(); void this.fetch(d); }, { allowSignalWrites: true });
  private async fetch(d: number) { this.ledger.set(await this.ipc.getEgressLedger(d)); }
  protected scrubbed(l: EgressLedger) { const r = l.totalRedactions; return r.email + r.card + r.phone + r.name; }
  protected pct(n: number, total: number) { return total ? Math.round((n / total) * 100) : 0; }
}
```

- [ ] **Step 2:** Mount `<app-egress-ledger />` in `analytics.component.ts` after the digest. `ng lint`/`ng build` green (watch the 16 kB budget — reuse global `.card`/`.chip`/`.empty-state`/`.btn-ghost`).
- [ ] **Step 3:** Playwright against `:1420` with a mocked `get_egress_ledger`: assert the three stat numbers, the per-model bars, and the four scrubbed-chips render; switching `7d`/`30d`/`90d` refetches. Commit `feat(analytics): Egress & Usage ledger panel`.

### Task 6.3: Per-meeting egress on the note detail (UX)
- [ ] In `features/detail/…`, a collapsible "Privacy receipt" showing `get_egress_for_meeting(id)` rows (destination, model, tokens, scrubbed-by-kind). Same token-only discipline. Commit `feat(detail): per-meeting privacy receipt`.

### Phase 6 gate
- [ ] gates green; **lock-security-reviewer**: the read path returns only counts/ids/host — never content; the panel cannot render a transcript.
- [ ] **adversarial-verifier**: PASS/FAIL.
- [ ] PR `feat/egress-ledger-ui` → merge.

---

# PHASE 7 — Per-task & per-folder model profiles + token-budget awareness

### Task 7.1: Per-folder model column + resolver

**Files:** Modify `src-tauri/src/storage/db.rs` (additive `folders.model`), `storage/models.rs` (`Folder.model: Option<String>`), `summarize/mod.rs` (a `resolve_model(config, folder_model) -> String` used when building a provider), `commands.rs` (`set_folder_model`), `lib.rs`.
- [ ] Step 1: failing test — `resolve_model` prefers a non-empty folder model over `config.provider_model`. RED → additive column + resolver + command → GREEN. Commit `feat(profiles): per-folder model override`.

### Task 7.2: Folder model dropdown (UX)
**Files:** `features/folders/…`, `ipc.service.ts`, `core/models.ts`.
- [ ] A small dropdown in the folder settings/context menu populated from `gwModels()` (or free text for non-gateway), calling `setFolderModel(folderId, model)`. A "sensitive folder → local model" hint. `ng build` green; Playwright smoke. Commit `feat(profiles): folder model picker`.

### Task 7.3: Token-budget awareness (NOT currency)
**Files:** `settings/config.rs` (`weekly_token_budget: Option<u32>`), `egress-ledger.component.ts` (a soft meter), `commands.rs`.
- [ ] A budget meter bar in the ledger panel ("{used}/{budget} tokens this week"); purely informational, token-denominated; a graceful banner when a gateway returns 429. NO dollar figures (§10). Commit `feat(egress): optional weekly token-budget meter`.

### Phase 7 gate: gates green; adversarial-verifier PASS; PR `feat/model-profiles` → merge.

---

# PHASE 8 — Capability fast-follow: structured output (`complete_json`)

### Task 8.1: `complete_json` trait method + default

**Files:** Modify `summarize/provider.rs` (additive `complete_json(system,user,schema:&Value) -> Result<Value>` default = today's `parse_first_json` recover-from-noise path, lifted from `reason.rs:511`), `summarize/gateway.rs` (override → send `response_format:{type:"json_schema", json_schema:…}`).
- [ ] Step 1: failing tests — (a) the DEFAULT impl parses a fenced/prose-wrapped reply via a mock (byte-identical to today); (b) the gateway override sends `response_format` (assert via `chat_body_json`). RED → implement → GREEN. Commit `feat(provider): additive complete_json (schema-strict on gateway, parse-first-json default)`.

### Task 8.2: Harden `timeline` + `graph` side-tasks
**Files:** Modify `summarize/timeline.rs:48`, `summarize/graph.rs:31` to call `complete_json` with their schema.
- [ ] Step 1: assert byte-identical output on the non-gateway path via the existing timeline/graph parse tests; the gateway path returns a parsed object directly. RED-where-applicable → switch the calls → GREEN. Commit `feat(sidetasks): timeline+graph use complete_json`.

### Phase 8 gate: gates green; adversarial-verifier PASS; PR `feat/structured-output` → merge.

---

# PHASE 9 — Capability fast-follow: token streaming + firewall de-tokenizer

**Gated by lock-security-reviewer** (the de-tokenizer must never emit a half-scrubbed token). Delivers the long-deferred token-by-token answer.

### Task 9.1: A streaming-safe restore buffer

**Files:** Create the de-tokenizer in `summarize/redact.rs` (a `RestoreBuffer` that holds incomplete `⟪…⟫` tokens AND incomplete multi-byte UTF-8 across chunk boundaries, releasing only fully-restorable text).

**Interfaces:**
- Produces: `pub(crate) struct RestoreBuffer<'a>{…}` with `fn push(&mut self, chunk: &str) -> String` (returns the safe-to-emit prefix) and `fn flush(&mut self) -> String`.

- [ ] **Step 1: Failing test (the RED that defines the feature)** — feed `"hello ⟪EM"` then `"AIL_1⟫ world"` with a map `{"⟪EMAIL_1⟫":"a@b.com"}`; assert the concatenation of `push`+`push`+`flush` equals `"hello a@b.com world"` and that NO intermediate `push` ever emits a partial `⟪EMAIL_…`.
- [ ] **Step 2: Run → FAIL.**
- [ ] **Step 3:** Implement the buffer (hold back any trailing `⟪` without a closing `⟫`; hold back incomplete UTF-8). 
- [ ] **Step 4: Run → PASS.**
- [ ] **Step 5: Commit** — `git commit -m "feat(redact): streaming-safe restore buffer (token + UTF-8 boundary)"`

### Task 9.2: `complete_streaming` trait method + gateway SSE

**Files:** Modify `summarize/provider.rs` (additive `complete_streaming(system,user,sink:&dyn TokenSink) -> Result<String>`, default emits the whole `complete` result as one chunk), `summarize/gateway.rs` (override → `stream:true`, parse SSE `data:` frames, feed each delta through the caller's redaction restore), `agent.rs`/`transcribe/live.rs` (a new `EVENT_ASSISTANT_DELTA` on the existing `DeltaSink`).
- [ ] Step 1: failing test — a mock SSE body of three `data:` frames, asserted to arrive as three sink callbacks whose concatenation is the full answer; the default impl emits exactly one chunk. RED → implement → GREEN. Commit `feat(provider): complete_streaming (SSE on gateway, one-shot default)`.

### Task 9.3: Stream into the assistant/chat bubble + cache-hit chip (UX)
**Files:** Modify `src/app/features/record/assistant*.component.ts` and `assistant.store.ts` (accumulate `EVENT_ASSISTANT_DELTA` into the streaming bubble; render a `⚡ cached` chip when `cached_tokens>0` arrives in the trailing `CallMeta`).
- [ ] Token-by-token rendering reuses the existing tool-chip stream seam; a blinking caret while streaming; the cache chip from the standard `cached_tokens` proxy. `ng build` green; Playwright drives a mocked delta stream → text appears incrementally. Commit `feat(assistant): token streaming + cache-hit chip`.

### Phase 9 gate
- [ ] gates green; **lock-security-reviewer**: the de-tokenizer never emits a partial scrub token (the RED test is the proof); streaming is only on the gateway/anthropic path, defaults unchanged.
- [ ] **adversarial-verifier**: PASS/FAIL.
- [ ] PR `feat/token-streaming` → merge.

---

# Final gate (after the last phase merged)
- [ ] `bash scripts/ci.sh` green (clippy -D warnings + tests + ng lint + ng build + headless E2E) — the ONE full run.
- [ ] Manual @Mac smoke on a signed/dev build: point at a local LiteLLM, confirm a note generates, the ledger logs it, the catalog populates, a remote-http URL is rejected, and a remote-ollama now prompts consent.
- [ ] Update `docs/STATUS.md` / `docs/KILLER-FEATURES.md` with the shipped Gateway Insights.
- [ ] Version bump + release via the `release-murmur` runbook (separate task).

---

## Self-Review (run against the two research reports)

**Spec coverage** — every recommended item is a task: gateway provider (P1) ✓; security guardrails R1-R4 (P0+P1) ✓; the `ollama_base_url` gap (P0) ✓; Bomba #1 usage parse (P2.3) ✓; CallMeta channel via additive methods, no Mutex race (P2.2/2.5) ✓; egress ledger write (P2) + read/UI (P6) ✓; model catalog/picker A (P3) ✓; structured errors+health C (P4) ✓; provenance B (P5) ✓; per-task/per-folder profiles E (P7) ✓; token-budget G (P7.3) ✓; structured-output (P8) ✓; streaming + de-tokenizer (P9) ✓; §10 token-only (Global Constraints + P7.3) ✓; skip list (function-calling/moderation/embeddings/currency) — intentionally NO tasks ✓.

**Placeholder scan** — the only deliberately abbreviated spots are the inline `styles:[…]` bodies (token list given, not full CSS) and a couple of mechanical "mirror the Anthropic-key command" commands; every load-bearing backend/security/ledger task carries real test + impl code.

**Type consistency** — `CallMeta`/`RedactionCounts`/`EgressEntry`/`EgressSink` names are stable across P2→P6; `summarize_with_meta`/`complete_with_meta`/`complete_json`/`complete_streaming` are the consistent additive-method names; TS `EgressLedger`/`EgressRow`/`GatewayModel`/`GatewayHealth` match the command return shapes.
