# Brain Competitive 4-Steps Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close the only pillar ClickUp Brain leads on: (1) activate the already-shipped brain features for fresh installs ("Enable the brain" onboarding), (2) Jira live connector, (3) Slack live connector, (4) a deterministic connector-verify pass that marks note claims `✓ confirmed / ⚠ not found / ⧗ conflict` against live Jira.

**Architecture:** Each phase is an independent, shippable PR. Phases 2–3 clone the proven `connectors/web.rs` pattern (fail-closed enable+consent+Keychain-key gate, framework-level query redaction + egress ledger, loud source attribution). Phase 4 adds a pure deterministic verifier (`verify.rs`) that reuses the `annotate_unverified` marker mechanics — the LLM is NEVER involved; regex extracts claims, live Jira supplies truth, pure Rust judges. Phase 1 is FE-only (backend download commands already exist).

**Tech Stack:** Rust (Tauri 2.11, `meetnotes_lib` crate), Angular 18 zoneless (standalone, signals), in-tree `reqwest` (rustls) — **zero new dependencies**.

**Research grounding:** `docs/research/2026-07-05-connectors-live-vs-rag.md` (live-not-RAG verdict), `docs/research/2026-07-05-competing-with-clickup-brain.md` (sequence rationale), `docs/research/2026-07-02-brain-connectors-slack-clickup-jira.md` (API shapes).

## Global Constraints

- **Errors:** every fallible fn returns `crate::error::Result<T>` (= `Result<T, AppError>`). NEVER `unwrap()`/`expect()` in non-test code. A locked-content refusal is `AppError::Locked(..)`.
- **Commands:** every new `#[tauri::command]` in `src-tauri/src/commands.rs` MUST also be added to `tauri::generate_handler![…]` in `src-tauri/src/lib.rs` (~line 51 onward) — a compiled-but-unregistered command is silently un-callable.
- **Lock model:** every content read/write gated by `meeting_is_unlocked` (`commands.rs:8343`, symbol not line — grep it). Phases 2–4 need a **lock-security-reviewer** pass before merge (new egress class instances + note read/write).
- **No PII in logs:** connector code logs id + hit counts + HTTP status only — never query text, snippets, tokens, note content.
- **No new deps:** reuse in-tree `reqwest`, `serde`, `async_trait`. No new crates, no new npm packages.
- **FE rules:** standalone + OnPush + signals; component = directory `name/name.component.{ts,html,scss}`; `@if`/`@for` only; `inject()` only; tokens (`var(--token)`) only; overlays opaque; no `setTimeout` in components; one typed `IpcService` method per command with the DTO in `src/app/core/models.ts`.
- **Test loop:** `(cd src-tauri && cargo test --lib)` — NEVER `cargo clippy --all-targets`. FE: `npx ng lint && npx ng build`. Final gate ONCE at the end of each phase: `bash scripts/ci.sh`.
- **Git:** one branch per phase off `murmur` (`feat/brain-enable-onboarding`, `feat/jira-connector`, `feat/slack-connector`, `feat/note-verify`). Commits authored `QueaT <kgm004a@gmail.com>`, **no Claude trailers**, no backticks in `git commit -m`. Merge via PR (`gh pr create -R murmur-io/murmur`), never direct push (hook-blocked).
- **Consent flags are PRESERVE-ONLY:** a `*_consented` flag is flipped true ONLY by its dedicated `consent_to_*` command; a settings save must never grant or clear it. Mirror `grant_web_search_consent` (`settings/config.rs:1087`) exactly, including the durable-record pattern its tests assert (`config.rs:1543`).
- **Anchors drift:** `commands.rs`/`db.rs` are >8k lines. Every `file:line` here is a hint — **grep the symbol** before editing.

---

# Phase 1 — "Enable the brain" onboarding (fix the dead fresh install)

**Problem (code-proven):** `extract_user_fact_candidates` returns empty on `StubReasoner` (`user_memory.rs:226`) and semantic search silently runs FTS until the e5 model exists on disk (`settings/config.rs:1189`, `embed.rs:264 embed_model_present`). A fresh install therefore has user-memory and semantic retrieval OFF even though both flags default ON. Fix = one FE card that downloads the heavy brain GGUF + the e5 embed model with progress, reused in (a) the onboarding wizard as a new step and (b) the Brain page as a nudge banner.

**Backend: NO CHANGES.** All commands exist and are registered: `brain_model_present`, `list_brain_models`, `download_brain_model(model_id)` (emits `EVENT_BRAIN_DOWNLOAD` progress), `embed_model_present`, `download_embed_model` (emits per-file embed progress) — see `src-tauri/src/lib.rs:228-245`.

**Store to reuse:** `src/app/features/settings/settings.store.ts` already wires everything: `brainModels()` (DTOs with `downloaded`, `selectedHeavy`, `selectedLight`), `brainDownloadingId`, `brainPct`, `downloadBrainModel(id)` (`settings.store.ts:1368`), `downloadEmbedModel()` (`settings.store.ts:1747`). **Verify first** that `SettingsStore` is `@Injectable({ providedIn: "root" })` (grep `providedIn` in that file); if it is not, it IS injectable from onboarding only if provided — in that case add `providedIn: "root"` is NOT allowed without checking how settings provides it; instead inject `IpcService` directly and copy the download-call pattern from the store. (Expected: it is root-provided; the tasks below assume that. If not, escalate to the human.)

### Task 1.1: `brain-enable-card` component

**Files:**
- Create: `src/app/features/brain/brain-enable-card/brain-enable-card.component.ts`
- Create: `src/app/features/brain/brain-enable-card/brain-enable-card.component.html`
- Create: `src/app/features/brain/brain-enable-card/brain-enable-card.component.scss`

**Interfaces:**
- Consumes: `SettingsStore.brainModels()`, `brainDownloadingId()`, `brainPct()`, `downloadBrainModel(id)`, `downloadEmbedModel()`; `IpcService.brainModelPresent()`, `embedModelPresent()`.
- Produces: `<app-brain-enable-card>` with `output<void>()` named `enabled` (fires when both models are present), and a `computed` `allReady` — Task 1.2 and 1.3 embed it.

- [ ] **Step 1: Read the exemplars.** Open `src/app/features/settings/sections/ai/ai-setup-block/ai-setup-block.component.ts` (store wiring pattern) and `src/app/features/onboarding/onboarding/onboarding.component.ts` (download-progress UX for the Whisper step). Match their idioms exactly.

- [ ] **Step 2: Write the component.**

`brain-enable-card.component.ts`:

```ts
import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  output,
  signal,
} from "@angular/core";
import { IpcService } from "../../../core/ipc.service";
import { SettingsStore } from "../../settings/settings.store";

/**
 * "Enable the brain" — the one-click activation card for the two on-device
 * models a fresh install is missing: the heavy brain GGUF (powers user memory
 * extraction + on-device Ask) and the e5 embed model (powers semantic search;
 * until it exists, retrieval silently falls back to FTS).
 *
 * Backend untouched: this drives the EXISTING SettingsStore download actions
 * (downloadBrainModel / downloadEmbedModel) and the existing presence probes.
 * Everything stays on the user's Mac — the card copy says so loudly.
 *
 * Embedded in two places: the onboarding "brain" step and the Brain page
 * nudge banner (hidden once both models are present).
 */
@Component({
  selector: "app-brain-enable-card",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./brain-enable-card.component.html",
  styleUrl: "./brain-enable-card.component.scss",
})
export class BrainEnableCardComponent {
  private readonly ipc = inject(IpcService);
  private readonly store = inject(SettingsStore);

  /** Fires once when both models land (parent may advance the wizard). */
  readonly enabled = output<void>();

  readonly brainPresent = signal<boolean | null>(null);
  readonly embedPresent = signal<boolean | null>(null);
  readonly running = signal(false);
  readonly stage = signal<"idle" | "brain" | "embed" | "done">("idle");
  readonly error = signal<string | null>(null);

  /** Live % for the in-flight brain GGUF (store listens to EVENT_BRAIN_DOWNLOAD). */
  readonly brainPct = this.store.brainPct;

  readonly allReady = computed(
    () => this.brainPresent() === true && this.embedPresent() === true,
  );

  /** Total size hint from the registry DTOs (heavy model approx size). */
  readonly sizeHint = computed(() => {
    const heavy = this.store.brainModels().find((m) => m.selectedHeavy);
    if (!heavy?.approxSizeBytes) return "~2 GB";
    return `~${Math.round(heavy.approxSizeBytes / 1_000_000_000 * 10) / 10} GB`;
  });

  private readonly _probe = effect(
    () => {
      void this.refresh();
    },
    { allowSignalWrites: true },
  );

  async refresh(): Promise<void> {
    try {
      const [brain, embed] = await Promise.all([
        this.ipc.brainModelPresent(),
        this.ipc.embedModelPresent(),
      ]);
      this.brainPresent.set(brain);
      this.embedPresent.set(embed);
      if (brain && embed) this.stage.set("done");
    } catch {
      // Presence probes never block the card; leave nulls (renders as unknown).
    }
  }

  /** The one click: heavy brain model first (big), then the e5 embed model. */
  async enable(): Promise<void> {
    if (this.running()) return;
    this.running.set(true);
    this.error.set(null);
    try {
      if (this.brainPresent() !== true) {
        this.stage.set("brain");
        const heavy = this.store.brainModels().find((m) => m.selectedHeavy);
        if (!heavy) throw new Error("no on-device model available");
        await this.store.downloadBrainModel(heavy.id);
      }
      if (this.embedPresent() !== true) {
        this.stage.set("embed");
        await this.store.downloadEmbedModel();
      }
      await this.refresh();
      if (this.allReady()) {
        this.stage.set("done");
        this.enabled.emit();
      }
    } catch (e) {
      this.error.set(e instanceof Error ? e.message : String(e));
      this.stage.set("idle");
    } finally {
      this.running.set(false);
    }
  }
}
```

**NOTE for the implementer:** field names on the brain-model DTO (`approxSizeBytes`, `selectedHeavy`, `id`, `downloaded`) must be checked against `src/app/core/models.ts` (grep `selectedHeavy`) — use the exact existing names. If `brainModels()` is empty until the settings page loads it, call the store's loader (grep how `ai-setup-block` guarantees it — likely `store.init()`/constructor-loaded) or fall back to `ipc.listBrainModels()` directly.

`brain-enable-card.component.html`:

```html
<div class="card enable-card">
  <div class="enable-copy">
    <h3>Enable the brain</h3>
    <p class="text-secondary">
      Downloads two on-device models ({{ sizeHint() }} total). They run
      entirely on your Mac — nothing is sent anywhere. This switches on
      <strong>memory</strong> (the brain remembers facts about you) and
      <strong>semantic search</strong> (find things by meaning, not keywords).
    </p>
  </div>

  @if (stage() === "done" || allReady()) {
    <span class="pill is-success"><span class="pill-dot"></span> Brain enabled</span>
  } @else if (running()) {
    <div class="enable-progress">
      @if (stage() === "brain") {
        <span class="text-secondary">Downloading the brain model… {{ brainPct() }}%</span>
      } @else {
        <span class="text-secondary">Downloading the search model…</span>
      }
    </div>
  } @else {
    <button type="button" class="btn btn-primary" (click)="enable()">
      Enable the brain
    </button>
  }

  @if (error(); as e) {
    <p class="banner is-warning" role="alert">Download failed: {{ e }} — try again.</p>
  }
</div>
```

`brain-enable-card.component.scss`:

```scss
.enable-card {
  display: flex;
  flex-direction: column;
  gap: var(--space-3);
}
.enable-copy h3 {
  margin: 0 0 var(--space-1);
}
.enable-progress {
  display: flex;
  align-items: center;
  gap: var(--space-2);
}
```

- [ ] **Step 3: Lint + build.**

Run: `npx ng lint && npx ng build`
Expected: clean (fix any DTO field-name mismatches surfaced by the compiler).

- [ ] **Step 4: Commit.**

```bash
git add src/app/features/brain/brain-enable-card/
git commit -m "feat(brain): one-click Enable the brain card (heavy GGUF + e5 embed download)"
```

### Task 1.2: Onboarding step "brain"

**Files:**
- Modify: `src/app/features/onboarding/onboarding/onboarding.component.ts` (STEPS at line ~18)
- Modify: `src/app/features/onboarding/onboarding/onboarding.component.html`

**Interfaces:**
- Consumes: `<app-brain-enable-card>` from Task 1.1.

- [ ] **Step 1: Extend the wizard.** In `onboarding.component.ts` change:

```ts
type Step = "welcome" | "model" | "provider" | "brain" | "vault" | "done";
const STEPS: readonly Step[] = [
  "welcome",
  "model",
  "provider",
  "brain",
  "vault",
  "done",
];
```

Add `BrainEnableCardComponent` to the component's `imports: [...]` array and import it at the top. Grep the existing `@switch (currentStep())` / `@if (currentStep() === …)` structure in `onboarding.component.html` and add the new step panel between provider and vault, matching the surrounding markup style exactly:

```html
@if (currentStep() === "brain") {
  <section class="step-panel">
    <app-brain-enable-card />
    <div class="step-actions">
      <button type="button" class="btn btn-ghost" (click)="next()">Skip for now</button>
      <button type="button" class="btn btn-primary" (click)="next()">Continue</button>
    </div>
  </section>
}
```

(Grep the actual `next()`/advance method name and the actual step-panel/action classes in the file and reuse them — do not invent new ones. "Skip for now" MUST exist: downloading ~2 GB is optional, never a wall.)

- [ ] **Step 2: Lint + build.** Run: `npx ng lint && npx ng build` — clean.

- [ ] **Step 3: Live smoke.** With the dev app running (skill `tauri-dev`: `MURMUR_DEV_DEK=… npm run dev`), drive `http://localhost:1420` via Playwright (mock `window.__TAURI_INTERNALS__.invoke` with `brain_model_present:false`, `embed_model_present:false`, `list_brain_models:[…]`) and screenshot the new step; verify the card renders, the Skip button advances, and clicking Enable calls `download_brain_model` in the mock log.

- [ ] **Step 4: Commit.**

```bash
git add src/app/features/onboarding/
git commit -m "feat(onboarding): Enable the brain wizard step (skippable)"
```

### Task 1.3: Brain-page nudge

**Files:**
- Modify: the Brain page shell component (grep: `ls src/app/features/brain/` — the top-level brain view component; it already renders header status with a "semantic badge on/off/model-absent").

- [ ] **Step 1:** Import `BrainEnableCardComponent` into the brain page component and render it at the top of the page **only when models are missing**:

```html
@if (!enableCard.allReady()) {
  <app-brain-enable-card #enableCard (enabled)="refresh()" />
}
```

(If a template-ref-driven `@if` on the child's own signal is awkward, hoist: inject `IpcService` in the page, add `readonly brainReady = signal<boolean | null>(null)` probed in an effect via `Promise.all([ipc.brainModelPresent(), ipc.embedModelPresent()])`, and gate on that. `refresh()` = the page's existing overview reload method — grep it.)

- [ ] **Step 2:** `npx ng lint && npx ng build` — clean. Playwright smoke: with mocked `brain_model_present:false` the card shows on /brain; with `true/true` it does not.

- [ ] **Step 3: Commit + PR.**

```bash
git add src/app/features/brain/
git commit -m "feat(brain): nudge card on the Brain page while on-device models are missing"
gh pr create -R murmur-io/murmur --title "Enable-the-brain onboarding (fix the dead fresh install)" --body "Fresh installs had user-memory + semantic search structurally OFF (stub reasoner / FTS fallback). One-click card downloads heavy GGUF + e5; onboarding step + Brain-page nudge. FE-only."
```

**Phase 1 verify gate:** adversarial-verifier pass (FE-only: lint+build green, Playwright live-reproduces the fresh-install state and the enabled state, no NG0600 — the probe effect writes signals and MUST carry `{ allowSignalWrites: true }`).

---

# Phase 2 — Jira live connector

**Shape:** exact clone of the web connector. New egress class instance ⇒ **lock-security-reviewer required**. Read `connectors/web.rs` + `connectors/mod.rs` fully before starting.

### Task 2.1: `JiraConnector` with fixture-tested parser (RED first)

**Files:**
- Create: `src-tauri/src/connectors/jira.rs`
- Modify: `src-tauri/src/connectors/mod.rs` (add `pub mod jira;` after `pub mod calendar;` at line 29; add registry-build line)
- Modify: `src-tauri/src/secrets/keychain.rs` (const near line 31 + routing arm near line 1564)

**Interfaces:**
- Produces: `JiraConnector::from_config_if_available(&AppConfig) -> Option<Self>`; `Connector` impl with `id() == "jira"`, `EgressClass::External`; `pub const JIRA_TOKEN_ACCOUNT: &str = "jira_api_token"`; `pub(crate) fn parse_results(body: &str) -> ConnectorResult`; `pub(crate) fn escape_jql(q: &str) -> String`. Task 4.x adds `get_issue` to this struct.
- Consumes: `AppConfig.jira_enabled / jira_consented / jira_base_url / jira_email` (Task 2.2 — write this file to compile against those names; Task 2.2 lands them).

**Order note:** Tasks 2.1 and 2.2 touch disjoint files but 2.1 compiles only after 2.2's config fields exist. Implement 2.2 FIRST, then 2.1 (kept in this order on paper because the parser is the risk; read both before starting).

- [ ] **Step 1: Write `connectors/jira.rs`** (complete file):

```rust
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
```

- [ ] **Step 2: Wire the module + registry + keychain routing.**

In `connectors/mod.rs`: add `pub mod jira;` next to `pub mod calendar;` (line 29), and in `ConnectorRegistry::build` (line 159) after the web line add:

```rust
        if let Some(c) = jira::JiraConnector::from_config_if_available(config) {
            connectors.push(Box::new(c));
        }
```

In `secrets/keychain.rs`: near line 31 (next to `ACCOUNT_WEB_SEARCH_KEY`) add:

```rust
/// Jira connector BYO API token — see [`crate::connectors::jira::JIRA_TOKEN_ACCOUNT`].
pub const ACCOUNT_JIRA_TOKEN: &str = "jira_api_token";
```

and add the routing arm in the match near line 1564 (mirror the `ACCOUNT_WEB_SEARCH_KEY` arm exactly):

```rust
        ACCOUNT_JIRA_TOKEN => ACCOUNT_JIRA_TOKEN,
```

- [ ] **Step 3: Run the tests** (compiles only after Task 2.2 — see order note).

Run: `cd src-tauri && cargo test --lib jira`
Expected: all `jira_*` tests PASS; `registry_excludes_web_when_disabled_or_unconsented_failclosed` still PASSES.

- [ ] **Step 4: Commit.**

```bash
git add src-tauri/src/connectors/ src-tauri/src/secrets/keychain.rs
git commit -m "feat(connectors): Jira live connector - fail-closed gate, fixture-tested JQL search parser"
```

### Task 2.2: Config flags (`jira_enabled/_consented/_base_url/_email`)

**Files:**
- Modify: `src-tauri/src/settings/config.rs`

**Interfaces:**
- Produces: `AppConfig { jira_enabled: bool, jira_consented: bool, jira_base_url: String, jira_email: String }` + `AppConfig::grant_jira_consent(&mut self, db: &Db) -> Result<()>`.

- [ ] **Step 1: Write failing tests** (append to `config.rs` tests, mirroring `web_search_flags_default_off_and_round_trip` at line 1340, `web_search_consent_grant_persists` at 1366, and `web_search_grant_durable_record_and_flag_agree` at 1543 — copy those three tests, rename `web_search`→`jira`, and extend the round-trip to also persist `jira_base_url`/`jira_email` string values):

```rust
    #[test]
    fn jira_flags_default_off_and_round_trip() {
        let (db, _dir) = test_db();
        let cfg = AppConfig::load(&db).unwrap();
        assert!(!cfg.jira_enabled, "jira must default OFF");
        assert!(!cfg.jira_consented, "jira consent must default ungranted");
        assert!(cfg.jira_base_url.is_empty());
        assert!(cfg.jira_email.is_empty());
        let cfg = AppConfig {
            jira_enabled: true,
            jira_base_url: "https://acme.atlassian.net".into(),
            jira_email: "me@acme.com".into(),
            ..cfg
        };
        cfg.save(&db).unwrap();
        let loaded = AppConfig::load(&db).unwrap();
        assert!(loaded.jira_enabled);
        assert_eq!(loaded.jira_base_url, "https://acme.atlassian.net");
        assert_eq!(loaded.jira_email, "me@acme.com");
        // PRESERVE-ONLY: a save can never grant consent.
        assert!(!loaded.jira_consented);
    }

    #[test]
    fn jira_consent_grant_persists_and_save_cannot_clobber() {
        let (db, _dir) = test_db();
        let mut cfg = AppConfig::load(&db).unwrap();
        cfg.grant_jira_consent(&db).unwrap();
        assert!(cfg.jira_consented);
        assert!(AppConfig::load(&db).unwrap().jira_consented);
        // A later plain save must PRESERVE the granted consent.
        let cfg2 = AppConfig { jira_consented: false, ..AppConfig::load(&db).unwrap() };
        cfg2.save(&db).unwrap();
        assert!(AppConfig::load(&db).unwrap().jira_consented, "save must not clear a granted consent");
    }
```

**NOTE:** grep the actual test-DB helper name used by the neighboring web tests (`test_db()` here is a guess — copy exactly what `web_search_consent_grant_persists` uses). If the durable-record test (`config.rs:1543`) asserts a separate consent-record row, mirror that assertion too.

- [ ] **Step 2: Run to verify RED.** `cd src-tauri && cargo test --lib jira_flags` → FAIL (fields don't exist).

- [ ] **Step 3: Implement** — mirror the `web_search_*` handling at each of these anchors (grep, don't trust line numbers):
  - Struct fields near `config.rs:305-317` (copy the doc comments' style; `jira_consented` documented PRESERVE-ONLY, mutated solely by `consent_to_jira`):

```rust
    /// Master toggle for the Jira connector (Settings ▸ Connectors). Even when ON, the connector is
    /// exposed only once `jira_consented` is granted and a token + base URL + email are configured.
    pub jira_enabled: bool,
    /// One-time egress consent for Jira. PRESERVE-ONLY on save; flipped true solely by the
    /// dedicated `consent_to_jira` command.
    pub jira_consented: bool,
    /// The Jira Cloud site base URL, e.g. `https://acme.atlassian.net` (non-secret).
    pub jira_base_url: String,
    /// The Atlassian account email paired with the API token for Basic auth (non-secret).
    pub jira_email: String,
```

  - Defaults near `:472`: `jira_enabled: false, jira_consented: false, jira_base_url: String::new(), jira_email: String::new(),`
  - Key consts near `:542`:

```rust
const K_JIRA_ENABLED: &str = "jira_enabled";
const K_JIRA_CONSENTED: &str = "jira_consented";
const K_JIRA_BASE_URL: &str = "jira_base_url";
const K_JIRA_EMAIL: &str = "jira_email";
```

  - Load arms near `:701` (mirror the two web arms; string keys assign directly: `cfg.jira_base_url = v;`).
  - Save writes near `:946` (mirror the web pattern EXACTLY, including how the preserve-only consent is written — copy the `web_search_consented` save semantics verbatim; strings are saved unconditionally like other string settings — grep how e.g. the ollama base-url string is saved and mirror that).
  - `grant_jira_consent` next to `grant_web_search_consent` (`:1087`) — copy its body verbatim (including any durable-record write), renamed keys.

- [ ] **Step 4: Run to verify GREEN.** `cd src-tauri && cargo test --lib jira` → PASS; full `cargo test --lib` → no regressions (the config merge tests are sensitive — if `save_config_merge_never_clobbers_or_grants_web_search_consent`-style tests enumerate fields, extend them for jira the way they cover web).

- [ ] **Step 5: Commit.**

```bash
git add src-tauri/src/settings/config.rs
git commit -m "feat(config): jira connector flags - enabled, preserve-only consent, base url, email"
```

### Task 2.3: Tool wiring (`jira_search`)

**Files:**
- Modify: `src-tauri/src/tools.rs`

**Interfaces:**
- Produces: `ToolCall::JiraSearch { query: String }`, ToolSpec `"jira_search"`, `pub async fn execute_jira_search(query: &str, config: &AppConfig) -> Result<String>`.

- [ ] **Step 1: Write the failing tests** (append to `tools.rs` tests, mirroring `sync_execute_tool_refuses_websearch` at ~line 833 and `web_search_fail_closed_returns_sentinel_no_egress` at ~856 — copy their exact setup helpers):

```rust
    #[test]
    fn sync_execute_tool_refuses_jira_search() {
        // Copy the body of the WebSearch refusal test, with:
        //   &ToolCall::JiraSearch { query: "x".into() }
        // and assert the same InvalidArg refusal.
    }

    #[test]
    fn jira_search_fail_closed_returns_sentinel_no_egress() {
        let cfg = AppConfig::default(); // jira disabled + unconsented
        let out = block_on(execute_jira_search("login bug", &cfg)).unwrap();
        assert!(out.contains("not available"), "fail-closed sentinel, no egress: {out}");
    }
```

(The first test body is written by copying the existing WebSearch refusal test verbatim and swapping the variant — write the REAL body, the comment above tells you the source.)

- [ ] **Step 2: RED.** `cargo test --lib refuses_jira` → FAIL (variant doesn't exist).

- [ ] **Step 3: Implement** — five surgical edits in `tools.rs`, each mirroring the neighboring `WebSearch` code:

1. `ToolCall` enum (near line 65): add

```rust
    /// LIVE JIRA SEARCH (consent-gated EXTERNAL connector). Like [`Self::WebSearch`], dispatched
    /// exclusively via the async [`execute_jira_search`], and ONLY when the Jira connector is
    /// exposed (enabled + consented + configured + token).
    JiraSearch { query: String },
```

2. `tool_specs()` (after the `web_search` entry at line ~151):

```rust
        ToolSpec {
            name: "jira_search",
            description: "Search the user's Jira issues (summary, status, assignee, due date). Only \
                          available when the user has enabled + consented to the Jira connector; \
                          results are loud-attributed '(via Jira)'. Use for questions about tickets, \
                          deadlines, sprint work, or to check an issue's current state.",
            parameters: str_arg("query", "What to look for in Jira, in the user's own language."),
            write: false,
        },
```

3. `execute_tool` refusal arm (next to the `ToolCall::WebSearch` arm at ~line 355):

```rust
        ToolCall::JiraSearch { .. } => {
            return Err(AppError::InvalidArg(
                "JiraSearch is an egress connector and cannot run through the egress-free tool path; \
                 use execute_jira_search"
                    .into(),
            ))
        }
```

4. Async dispatcher (next to `execute_web_search` at ~line 399) — reuses `format_web_hits` (it renders any `ConnectorHit` loudly):

```rust
/// CONNECTOR DISPATCH — run a LIVE JIRA search through the connector seam. Mirrors
/// [`execute_web_search`]: fail-closed sentinel when not exposed (NOTHING egresses), redaction +
/// egress-ledger applied by [`crate::connectors::ConnectorRegistry::search`], loud attribution.
pub async fn execute_jira_search(query: &str, config: &AppConfig) -> Result<String> {
    let q = query.trim();
    if q.is_empty() {
        return Ok("No Jira results for an empty query.".to_string());
    }
    let registry = crate::connectors::ConnectorRegistry::build(config);
    match registry.search("jira", q).await {
        Ok(hits) if hits.is_empty() => Ok(format!("No Jira results for \"{q}\".")),
        Ok(hits) => Ok(format_web_hits(&hits)),
        Err(crate::connectors::ConnectorError::NeedsConsent) => Ok(
            "Jira search is not available (not enabled, not consented, or not configured)."
                .to_string(),
        ),
        Err(crate::connectors::ConnectorError::Unconfigured(_)) => {
            Ok("Jira search is not available (not configured).".to_string())
        }
        Err(e @ crate::connectors::ConnectorError::Failed(_)) => Err(e.into()),
    }
}
```

5. `GatedToolExecutor`: in `specs()` extend the connector filter arm at ~line 601 to `"web_search" | "calendar_lookup" | "jira_search" => has_app,` and in `run()` add next to the `"web_search"` arm (~line 686):

```rust
            "jira_search" => match self.app {
                Some(_) => block_on_tool(execute_jira_search(&s("query"), self.config)),
                None => Err(AppError::InvalidArg("jira_search needs an AppHandle".into())),
            },
```

**Also grep** for where `ToolCall` is parsed from the model's tool-call name (the `match name` / serde mapping that turns `"web_search"` into `ToolCall::WebSearch`) and add the `"jira_search"` ↔ `ToolCall::JiraSearch` mapping there too — a spec without a parse arm is un-invokable.

- [ ] **Step 4: GREEN.** `cargo test --lib jira && cargo test --lib tools` → PASS, no regressions.

- [ ] **Step 5: Commit.**

```bash
git add src-tauri/src/tools.rs
git commit -m "feat(tools): jira_search tool - gated dispatch through the connector registry"
```

### Task 2.4: Commands + registration

**Files:**
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/lib.rs`

**Interfaces:**
- Produces commands: `consent_to_jira`, `set_jira_token(key: String)`, `has_jira_token() -> bool`.

- [ ] **Step 1:** Copy the three web-search commands (`consent_to_web_search` at `commands.rs:4154`, `set_web_search_api_key` at `:4862`, `has_web_search_key` at `:4871`) verbatim, renamed, using `crate::connectors::jira::JIRA_TOKEN_ACCOUNT`:

```rust
/// One-time Jira egress consent — the ONLY way `jira_consented` flips true. Persists the flag AND
/// updates the in-memory config cache, so the next `ConnectorRegistry::build` exposes the jira tool
/// (provided Jira is also enabled + configured + a token is stored). Idempotent.
#[tauri::command]
pub fn consent_to_jira(state: State<'_, AppState>) -> Result<(), AppError> {
    let mut cache = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    cache.grant_jira_consent(&state.db)?;
    Ok(())
}

/// Store/replace the BYO Jira API token in the Keychain (account "jira_api_token"). An empty input
/// clears it. NEVER logged, NEVER returned to the FE — only `has_*` reports presence.
#[tauri::command]
pub fn set_jira_token(key: String) -> Result<(), AppError> {
    if key.trim().is_empty() {
        return secrets::delete_secret(crate::connectors::jira::JIRA_TOKEN_ACCOUNT);
    }
    secrets::set_secret(crate::connectors::jira::JIRA_TOKEN_ACCOUNT, key.trim())
}

/// Whether a Jira token is currently stored (UI shows "set"/"not set"; never the value).
#[tauri::command]
pub fn has_jira_token() -> Result<bool, AppError> {
    Ok(
        secrets::get_secret(crate::connectors::jira::JIRA_TOKEN_ACCOUNT)?
            .filter(|k| !k.trim().is_empty())
            .is_some(),
    )
}
```

Also extend `config_to_dto` (grep it in `commands.rs` — right below `consent_to_web_search`) with the four new fields, mirroring how `web_search_enabled`/`web_search_consented` cross to the DTO.

- [ ] **Step 2:** Register all three in `lib.rs` `generate_handler![…]` next to the web-search entries.

- [ ] **Step 3:** If `commands.rs` has a config-DTO / save path that enumerates fields FE-side (grep `webSearchEnabled` in `commands.rs` — the AppConfig↔DTO mapping), add `jira_enabled`/`jira_base_url`/`jira_email` (+ read-only `jira_consented`) mirroring web. The consent flag mirrors web's BLK-4 comment (`commands.rs:4354`): the save path must copy the LIVE value, never the FE's.

- [ ] **Step 4:** `cargo test --lib` → green. Commit:

```bash
git add src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat(commands): consent_to_jira + set_jira_token + has_jira_token, registered"
```

### Task 2.5: FE — settings Connectors section + IPC

**Files:**
- Modify: `src/app/core/ipc.service.ts`, `src/app/core/models.ts`
- Modify: `src/app/features/settings/settings.store.ts`
- Modify: `src/app/features/settings/sections/settings-connectors-section/settings-connectors-section.component.{html,ts}`

- [ ] **Step 1: IPC + DTO.** In `models.ts` extend `AppConfigDto` with `jiraEnabled: boolean; jiraConsented: boolean; jiraBaseUrl: string; jiraEmail: string;` (match the exact serde casing the backend DTO emits — grep how `webSearchEnabled` crosses; mirror it). In `ipc.service.ts` next to the web methods (line ~471):

```ts
  /** One-time Jira egress consent — the ONLY way jiraConsented flips true. */
  consentToJira(): Promise<void> {
    return invoke<void>("consent_to_jira");
  }
  setJiraToken(key: string): Promise<void> {
    return invoke<void>("set_jira_token", { key });
  }
  hasJiraToken(): Promise<boolean> {
    return invoke<boolean>("has_jira_token");
  }
```

- [ ] **Step 2: Store.** In `settings.store.ts` mirror every `webSearchEnabled` touchpoint for `jiraEnabled` + the two strings (form control defaults, load mapping, save mapping) — grep `webSearchEnabled` in the file and replicate each site. Add `hasJiraToken` state + `saveJiraToken(key)` action mirroring the web-key ones (grep `hasWebKey`).

- [ ] **Step 3: Section UI.** In `settings-connectors-section.component.html` append a Jira block AFTER the web-search block, copying its exact structure (toggle-row → egress banner → fieldset): toggle bound to `formControlName="jiraEnabled"`; inside the `@if (form.controls.jiraEnabled.value)` body: the warning banner (copy web's, s/web search/Jira/; state clearly *"your (redacted) query and the matching issue summaries pass through your configured AI model"*), two text inputs bound to `jiraBaseUrl` (`placeholder="https://your-site.atlassian.net"`) and `jiraEmail`, the token password input + Save button (mirror the Brave key row, calling the store's `saveJiraToken`), the key-status pill, and the consent row: if `!cfg.jiraConsented` show a `btn-primary` "Allow Jira access (one-time consent)" calling a component method that runs `await ipc.consentToJira()` then reloads config. Copy the web block's classes verbatim — no new CSS.

- [ ] **Step 4:** `npx ng lint && npx ng build` → clean. Playwright smoke against a mocked invoke: toggling Jira shows banner + fields; consent button calls `consent_to_jira`.

- [ ] **Step 5: Commit + PR.**

```bash
git add src/app
git commit -m "feat(settings): Jira connector UI - toggle, base URL, email, token, one-time consent"
gh pr create -R murmur-io/murmur --title "Jira live connector" --body "Live, fail-closed, consent-gated jira_search tool (JQL text search via /rest/api/3/search/jql, Basic auth, BYO token in Keychain). Clone of the web connector pattern; query redacted at the registry boundary + egress-ledgered. Settings UI mirrors web search."
```

**Phase 2 verify gates (both required):** (1) **lock-security-reviewer** — new External egress instance: consent fail-closed? query redacted at boundary? ledger row per attempt? token never logged/FE-exposed? inbound Jira text riding the existing RedactingProvider path to cloud reasoners (same posture as web — call it out in the PR body)? (2) **adversarial-verifier** — full `cargo test --lib`, `ng lint`, `ng build`, live Playwright pass, and a REAL round-trip needs a real Jira workspace token (manual smoke: honest bar — record it in the PR).

---

# Phase 3 — Slack live connector

Identical shape to Phase 2. Slack specifics: GET `https://slack.com/api/search.messages` with `Authorization: Bearer xoxp-…` (a **user** token with `search:read` — NOT a bot token; bot tokens cannot call search). **Slack returns HTTP 200 with `{"ok":false,"error":"…"}` on failure — the parser MUST check `ok`.**

### Task 3.1: `SlackConnector` with fixture-tested parser

**Files:**
- Create: `src-tauri/src/connectors/slack.rs`
- Modify: `src-tauri/src/connectors/mod.rs` (add `pub mod slack;` + registry line, mirroring Task 2.1 Step 2)
- Modify: `src-tauri/src/secrets/keychain.rs` (`pub const ACCOUNT_SLACK_TOKEN: &str = "slack_user_token";` + routing arm — mirror Task 2.1 Step 2)

**Interfaces:**
- Produces: `SlackConnector::from_config_if_available(&AppConfig) -> Option<Self>`; `Connector` impl `id() == "slack"`, `EgressClass::External`; `pub const SLACK_TOKEN_ACCOUNT: &str = "slack_user_token"`.
- Consumes: `AppConfig.slack_enabled / slack_consented` (Task 3.2).

- [ ] **Step 1: Write `connectors/slack.rs`** (complete file):

```rust
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

    async fn search(&self, redacted_query: &str) -> ConnectorResult {
        let q = redacted_query.trim();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        let client = reqwest::Client::new();
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
```

- [ ] **Step 2:** Registry + keychain wiring exactly as Task 2.1 Step 2 (slack names). RED→GREEN: `cargo test --lib slack` (after Task 3.2 lands the config fields — same ordering note as Phase 2).

- [ ] **Step 3: Commit.**

```bash
git add src-tauri/src/connectors/ src-tauri/src/secrets/keychain.rs
git commit -m "feat(connectors): Slack live connector - search.messages, ok:false handling, fixture-tested"
```

### Task 3.2: Config flags (`slack_enabled/_consented`)

**Files:** Modify `src-tauri/src/settings/config.rs`.

- [ ] **Step 1–4:** Repeat Task 2.2 exactly for `slack_enabled` + `slack_consented` + `grant_slack_consent` (no base-url/email strings — Slack needs only the token). Tests: `slack_flags_default_off_and_round_trip`, `slack_consent_grant_persists_and_save_cannot_clobber` — same bodies as Task 2.2's with `jira`→`slack` and without the string fields. RED → implement → GREEN → commit:

```bash
git add src-tauri/src/settings/config.rs
git commit -m "feat(config): slack connector flags - enabled + preserve-only consent"
```

### Task 3.3: Tool wiring (`slack_search`)

**Files:** Modify `src-tauri/src/tools.rs`.

- [ ] **Step 1–4:** Repeat Task 2.3 exactly with: `ToolCall::SlackSearch { query: String }`, spec:

```rust
        ToolSpec {
            name: "slack_search",
            description: "Search the user's Slack messages (channels + DMs their token can see). Only \
                          available when the user has enabled + consented to the Slack connector; \
                          results are loud-attributed '(via Slack)'. Use for 'what did we say/decide \
                          about X in Slack' questions.",
            parameters: str_arg("query", "What to look for in Slack, in the user's own language."),
            write: false,
        },
```

`execute_slack_search` mirrors `execute_jira_search` (registry id `"slack"`, sentinel copy s/Jira/Slack/); sync `execute_tool` refusal arm; `specs()` filter becomes `"web_search" | "calendar_lookup" | "jira_search" | "slack_search" => has_app,`; `run()` arm; the name↔variant parse mapping. Tests: `sync_execute_tool_refuses_slack_search` + `slack_search_fail_closed_returns_sentinel_no_egress` (same bodies as 2.3's, renamed). RED → GREEN → commit:

```bash
git add src-tauri/src/tools.rs
git commit -m "feat(tools): slack_search tool - gated dispatch through the connector registry"
```

### Task 3.4: Replace the shipped voice stub

**Files:** Modify `src-tauri/src/voice_action.rs` (arm at line ~222; test `slack_search_is_unavailable` at ~1321).

- [ ] **Step 1:** Open `voice_action.rs` and find how the `WebSearch` voice intent executes (grep `web_search_blocking` — the blocking-bridge helper `tools.rs:792` comments reference). Rewrite the `VoiceIntent::SlackSearch { .. }` arm to mirror the WebSearch arm exactly (same blocking helper, calling `crate::tools::execute_slack_search` with the live config), keeping the current "isn't available yet" text as the sentinel the tool itself returns when unconsented (the tool's own fail-closed sentinel replaces the hardcoded stub message).
- [ ] **Step 2:** Update the `slack_search_is_unavailable` test: with a default (unconsented) config the result must now carry the tool's fail-closed sentinel (`"not available"` substring) — still a non-answer, still zero egress. RED if you changed the arm wrong; GREEN when the sentinel flows through.
- [ ] **Step 3:** `cargo test --lib voice_action` → PASS. Commit:

```bash
git add src-tauri/src/voice_action.rs
git commit -m "feat(voice): SlackSearch voice intent rides the real connector (fail-closed sentinel when unconsented)"
```

### Task 3.5: Commands + FE

**Files:** as Phase 2 Tasks 2.4 + 2.5, for Slack.

- [ ] **Step 1:** Commands `consent_to_slack` / `set_slack_token` / `has_slack_token` (copy the jira trio, `SLACK_TOKEN_ACCOUNT`), registered in `lib.rs`. Config-DTO mapping for `slack_enabled` (+read-only `slack_consented`).
- [ ] **Step 2:** FE: `models.ts` (`slackEnabled`, `slackConsented`), `ipc.service.ts` (three methods), `settings.store.ts` (mirror every `jiraEnabled` touchpoint minus the strings), connectors-section HTML block AFTER Jira (toggle → banner → token fieldset with help copy: *"Paste a user token (xoxp-…) from a single-workspace Slack app with the search:read scope"* → consent button).
- [ ] **Step 3:** `cargo test --lib` + `npx ng lint && npx ng build` → green. Playwright smoke. Commit + PR:

```bash
git add src-tauri/src src/app
git commit -m "feat(settings): Slack connector UI - toggle, user token, one-time consent"
gh pr create -R murmur-io/murmur --title "Slack live connector" --body "Live, fail-closed, consent-gated slack_search tool (search.messages, xoxp user token in Keychain, ok:false handled, snippets capped). Voice SlackSearch stub replaced by the real connector. Mirrors the web/Jira pattern."
```

**Phase 3 verify gates:** same two as Phase 2 (lock-security-reviewer + adversarial-verifier). Real-token round-trip = manual smoke, recorded in the PR (honest bar).

---

# Phase 4 — Note verify pass (deterministic, live-Jira)

**Design (from `2026-07-05-connectors-live-vs-rag.md`):** on-demand, consent-gated, NEVER in the zero-egress proactive path. v1 is **fully deterministic — no LLM anywhere**: regex extracts Jira issue keys from the note, live Jira supplies the current issue state, pure Rust judges, non-destructive `> ` blockquote markers mirror `annotate_unverified` (`summarize/grounding.rs:66`). Marker application is **idempotent** (strip-then-reinsert). Depends on Phase 2 (JiraConnector). **lock-security-reviewer required** (note read/write + new lookup egress).

### Task 4.1: `verify.rs` — pure extraction + judgment + markers (no network)

**Files:**
- Create: `src-tauri/src/verify.rs`
- Modify: `src-tauri/src/lib.rs` (add `pub mod verify;` next to the other module decls — grep `mod proactive;` and mirror)

**Interfaces:**
- Produces:
  - `pub struct IssueSnapshot { pub key: String, pub summary: String, pub status: String, pub due: Option<String>, pub url: String }`
  - `pub struct VerifyFinding { pub line_no: usize, pub key: String, pub verdict: Verdict, pub detail: String, pub url: String }` with `pub enum Verdict { Confirmed, NotFound, Conflict }` (both `serde::Serialize + Deserialize`, `#[serde(rename_all = "camelCase")]` on the struct, verdict serialized lowercase via `#[serde(rename_all = "lowercase")]`)
  - `pub fn extract_issue_keys(note_md: &str) -> Vec<(usize, String)>`
  - `pub fn judge(line_text: &str, key: &str, snap: Option<&IssueSnapshot>) -> (Verdict, String)`
  - `pub fn apply_verify_markers(note_md: &str, findings: &[VerifyFinding]) -> String`

- [ ] **Step 1: Write the failing tests first** (bottom of the new `verify.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn snap(key: &str, status: &str, due: Option<&str>) -> IssueSnapshot {
        IssueSnapshot {
            key: key.into(),
            summary: "Fix login".into(),
            status: status.into(),
            due: due.map(String::from),
            url: format!("https://acme.atlassian.net/browse/{key}"),
        }
    }

    #[test]
    fn extracts_unique_keys_with_line_numbers_capped() {
        let md = "---\ntitle: x\n---\n# Notes\n- Ship PROJ-123 by Friday\n- PROJ-123 again\n- also ABC-9\n";
        let keys = extract_issue_keys(md);
        // 1-based line numbers COUNT the frontmatter lines: PROJ-123 sits on line 5, ABC-9 on 7.
        assert_eq!(keys, vec![(5, "PROJ-123".to_string()), (7, "ABC-9".to_string())]);
        // Cap at 10 unique keys.
        let many: String = (1..=15).map(|i| format!("- K{i}A-{i}\n")).collect();
        assert_eq!(extract_issue_keys(&many).len(), 10);
    }

    #[test]
    fn frontmatter_and_existing_markers_are_not_scanned() {
        let md = "---\nref: FM-1\n---\n> ✓ OLD-1 · Status: Done (via Jira)\n- real REAL-2\n";
        let keys = extract_issue_keys(md);
        // FM-1 (frontmatter) and OLD-1 (our own marker line) are skipped; REAL-2 is on line 5.
        assert_eq!(keys, vec![(5, "REAL-2".to_string())]);
    }

    #[test]
    fn judge_not_found_confirmed_and_date_conflict() {
        // Missing issue → NotFound.
        let (v, d) = judge("- Ship PROJ-1 by 2026-07-08", "PROJ-1", None);
        assert!(matches!(v, Verdict::NotFound));
        assert!(d.contains("PROJ-1"));
        // Found, no date in the line → Confirmed with status/due detail.
        let s = snap("PROJ-1", "In Progress", Some("2026-07-10"));
        let (v, d) = judge("- Ship PROJ-1 soon", "PROJ-1", Some(&s));
        assert!(matches!(v, Verdict::Confirmed));
        assert!(d.contains("In Progress") && d.contains("2026-07-10"));
        // ISO date in the line ≠ issue due → Conflict naming both dates.
        let (v, d) = judge("- Ship PROJ-1 by 2026-07-08", "PROJ-1", Some(&s));
        assert!(matches!(v, Verdict::Conflict));
        assert!(d.contains("2026-07-08") && d.contains("2026-07-10"));
        // ISO date matching the due → Confirmed.
        let (v, _) = judge("- Ship PROJ-1 by 2026-07-10", "PROJ-1", Some(&s));
        assert!(matches!(v, Verdict::Confirmed));
    }

    #[test]
    fn markers_are_inserted_after_lines_and_idempotent() {
        let md = "# N\n- Ship PROJ-1 by 2026-07-08\n- other line\n";
        let f = VerifyFinding {
            line_no: 2,
            key: "PROJ-1".into(),
            verdict: Verdict::Conflict,
            detail: "note says 2026-07-08, PROJ-1 due 2026-07-10".into(),
            url: "https://x/browse/PROJ-1".into(),
        };
        let once = apply_verify_markers(md, &[f.clone()]);
        assert!(once.contains("\n> ⧗ note says 2026-07-08, PROJ-1 due 2026-07-10 (via Jira)\n"));
        // Idempotent: applying again yields byte-identical output (old markers stripped first).
        let twice = apply_verify_markers(&once, &[f]);
        assert_eq!(once, twice);
        // Non-destructive: original lines untouched.
        assert!(twice.contains("- Ship PROJ-1 by 2026-07-08"));
        assert!(twice.contains("- other line"));
    }

    #[test]
    fn empty_findings_strip_stale_markers_only() {
        let md = "# N\n- done PROJ-1\n> ✓ PROJ-1 · Status: Done (via Jira)\n";
        let out = apply_verify_markers(md, &[]);
        assert!(!out.contains("(via Jira)"), "stale markers removed");
        assert!(out.contains("- done PROJ-1"));
    }
}
```

- [ ] **Step 2: RED.** `cargo test --lib verify` → FAIL (module doesn't exist).

- [ ] **Step 3: Implement** (top of `verify.rs`; check whether the crate already depends on `regex` — grep `use regex` / `Cargo.toml`; if NOT present, implement the key-matcher with a hand-rolled scanner as below — **no new crates**):

```rust
//! NOTE VERIFY — deterministic verification of note claims against LIVE connector truth (v1: Jira).
//!
//! Design (docs/research/2026-07-05-connectors-live-vs-rag.md): the LLM is NEVER the judge.
//! `extract_issue_keys` (pure regex-free scanner) finds ticket keys; the caller fetches each
//! issue's CURRENT state live (staleness would INVERT a verification); `judge` (pure) compares;
//! `apply_verify_markers` appends non-destructive `> ` blockquote markers exactly like
//! `summarize/grounding.rs::annotate_unverified` — idempotent (strip old `(via Jira)` markers,
//! re-insert), byte-preserving for every original line.
//!
//! On-demand + consent-gated ONLY (rides the Jira connector gates). NEVER wired into the
//! zero-egress proactive path (`proactive.rs` contract D1).

use serde::{Deserialize, Serialize};

/// The live state of one Jira issue, fetched at verify time.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueSnapshot {
    pub key: String,
    pub summary: String,
    pub status: String,
    pub due: Option<String>,
    pub url: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Confirmed,
    NotFound,
    Conflict,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyFinding {
    /// 1-based line number in the note markdown the claim sits on.
    pub line_no: usize,
    pub key: String,
    pub verdict: Verdict,
    /// Human detail rendered in the marker/panel. Contains ONLY connector-sourced values +
    /// dates already present in the note line — never other note content.
    pub detail: String,
    pub url: String,
}

/// Max unique keys verified per pass (bounds egress + latency).
const MAX_KEYS: usize = 10;

/// A verify marker line we own (and may strip on re-apply).
fn is_verify_marker(line: &str) -> bool {
    let t = line.trim_start();
    (t.starts_with("> ✓") || t.starts_with("> ⚠") || t.starts_with("> ⧗"))
        && t.trim_end().ends_with("(via Jira)")
}

/// Scan for Jira-style issue keys (`ABC-123`): 1 uppercase letter + up to 9 uppercase
/// alphanumerics, a dash, 1–6 digits, on WORD BOUNDARIES. Skips YAML frontmatter and our own
/// marker lines. Returns (1-based line_no, key), first occurrence per unique key, capped.
pub fn extract_issue_keys(note_md: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut in_frontmatter = false;
    for (idx, line) in note_md.lines().enumerate() {
        let line_no = idx + 1;
        if idx == 0 && line.trim() == "---" {
            in_frontmatter = true;
            continue;
        }
        if in_frontmatter {
            if line.trim() == "---" {
                in_frontmatter = false;
            }
            continue;
        }
        if is_verify_marker(line) {
            continue;
        }
        for key in scan_keys(line) {
            if out.len() >= MAX_KEYS {
                return out;
            }
            if seen.insert(key.clone()) {
                out.push((line_no, key));
            }
        }
    }
    out
}

/// Hand-rolled key scanner (no `regex` dependency): uppercase run then '-' then digit run,
/// bounded by non-alphanumerics.
fn scan_keys(line: &str) -> Vec<String> {
    let bytes = line.as_bytes();
    let mut keys = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        // Word boundary: previous char must not be alphanumeric.
        if i > 0 && bytes[i - 1].is_ascii_alphanumeric() {
            i += 1;
            continue;
        }
        // Project part: uppercase letter then 0..=9 uppercase alphanumerics.
        if !bytes[i].is_ascii_uppercase() {
            i += 1;
            continue;
        }
        let start = i;
        let mut j = i + 1;
        while j < bytes.len()
            && j - start < 10
            && (bytes[j].is_ascii_uppercase() || bytes[j].is_ascii_digit())
        {
            j += 1;
        }
        if j < bytes.len() && bytes[j] == b'-' {
            let dash = j;
            let mut k = dash + 1;
            while k < bytes.len() && k - dash <= 6 && bytes[k].is_ascii_digit() {
                k += 1;
            }
            let digits = k - dash - 1;
            let boundary_ok = k >= bytes.len() || !bytes[k].is_ascii_alphanumeric();
            if digits >= 1 && boundary_ok {
                keys.push(line[start..k].to_string());
                i = k;
                continue;
            }
        }
        i = j;
    }
    keys
}

/// Find the first ISO date (`YYYY-MM-DD`) in a line, if any (hand-rolled, no regex).
fn first_iso_date(line: &str) -> Option<String> {
    let b = line.as_bytes();
    for i in 0..b.len().saturating_sub(9) {
        if i > 0 && b[i - 1].is_ascii_digit() {
            continue;
        }
        let w = &b[i..i + 10];
        let shape = w[0].is_ascii_digit()
            && w[1].is_ascii_digit()
            && w[2].is_ascii_digit()
            && w[3].is_ascii_digit()
            && w[4] == b'-'
            && w[5].is_ascii_digit()
            && w[6].is_ascii_digit()
            && w[7] == b'-'
            && w[8].is_ascii_digit()
            && w[9].is_ascii_digit();
        let boundary = i + 10 >= b.len() || !b[i + 10].is_ascii_digit();
        if shape && boundary {
            return Some(line[i..i + 10].to_string());
        }
    }
    None
}

/// PURE deterministic verdict — the load-bearing property (mirrors `facts::reconcile_facts`:
/// the LLM never judges; injected text has no judgment step to hijack).
pub fn judge(line_text: &str, key: &str, snap: Option<&IssueSnapshot>) -> (Verdict, String) {
    match snap {
        None => (Verdict::NotFound, format!("{key} not found in Jira")),
        Some(s) => {
            if let (Some(note_date), Some(due)) = (first_iso_date(line_text), s.due.as_deref()) {
                if note_date != due {
                    return (
                        Verdict::Conflict,
                        format!("note says {note_date}, {key} due {due}"),
                    );
                }
            }
            let mut detail = format!("{key} · Status: {}", s.status);
            if let Some(due) = s.due.as_deref() {
                detail.push_str(&format!(" · due {due}"));
            }
            (Verdict::Confirmed, detail)
        }
    }
}

/// Append one non-destructive marker blockquote after each finding's line. IDEMPOTENT: all
/// existing `(via Jira)` marker lines are stripped first, so re-verifying replaces (never stacks).
/// Every ORIGINAL line is preserved byte-identically (the annotate_unverified discipline).
pub fn apply_verify_markers(note_md: &str, findings: &[VerifyFinding]) -> String {
    // 1) Strip our old markers, remembering the original line numbering AFTER the strip.
    let kept: Vec<&str> = note_md.lines().filter(|l| !is_verify_marker(l)).collect();
    // 2) Group findings by line_no (computed against the STRIPPED text — the command recomputes
    //    findings from the stripped note, see verify_note_sources).
    let mut out: Vec<String> = Vec::with_capacity(kept.len() + findings.len());
    for (idx, line) in kept.iter().enumerate() {
        out.push((*line).to_string());
        let line_no = idx + 1;
        for f in findings.iter().filter(|f| f.line_no == line_no) {
            let glyph = match f.verdict {
                Verdict::Confirmed => "✓",
                Verdict::NotFound => "⚠",
                Verdict::Conflict => "⧗",
            };
            out.push(format!("> {glyph} {} (via Jira)", f.detail));
        }
    }
    let mut s = out.join("\n");
    if note_md.ends_with('\n') {
        s.push('\n');
    }
    s
}
```

- [ ] **Step 4: GREEN.** `cargo test --lib verify` → PASS.

- [ ] **Step 5: Commit.**

```bash
git add src-tauri/src/verify.rs src-tauri/src/lib.rs
git commit -m "feat(verify): pure deterministic note-claim extraction, judgment, idempotent markers"
```

### Task 4.2: `JiraConnector::get_issue` + registry lookup with ledger

**Files:**
- Modify: `src-tauri/src/connectors/jira.rs`
- Modify: `src-tauri/src/connectors/mod.rs`

**Interfaces:**
- Produces: `JiraConnector::get_issue(&self, key: &str) -> Result<Option<crate::verify::IssueSnapshot>, ConnectorError>` (async); `ConnectorRegistry::jira_lookup(&self, key: &str) -> Result<Option<crate::verify::IssueSnapshot>, ConnectorError>` (async; fail-closed + one content-free ledger row per attempt).

- [ ] **Step 1: Failing test** (in `connectors/mod.rs` tests — mirrors `connector_search_records_one_content_free_egress_row`):

```rust
    #[test]
    fn jira_lookup_fails_closed_without_ledger_when_not_exposed() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let registry = ConnectorRegistry::with_parts(
            vec![],
            std::sync::Arc::new(NoopNameRedactor),
            std::sync::Arc::new(CaptureEgressSink(captured.clone())),
        );
        let res = block_on(registry.jira_lookup("PROJ-1"));
        assert!(matches!(res, Err(ConnectorError::NeedsConsent)));
        assert!(captured.lock().unwrap().is_empty(), "no egress ⇒ no ledger row");
    }

    #[test]
    fn jira_lookup_rejects_malformed_keys_without_egress() {
        let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let registry = ConnectorRegistry::with_parts(
            vec![],
            std::sync::Arc::new(NoopNameRedactor),
            std::sync::Arc::new(CaptureEgressSink(captured.clone())),
        );
        // Even before exposure is checked, a key that isn't a strict issue key is refused.
        let res = block_on(registry.jira_lookup("not a key; DROP TABLE"));
        assert!(res.is_err());
        assert!(captured.lock().unwrap().is_empty());
    }
```

- [ ] **Step 2: RED.** `cargo test --lib jira_lookup` → FAIL.

- [ ] **Step 3: Implement.** In `jira.rs` add to `impl JiraConnector`:

```rust
    /// Fetch ONE issue's current state (verify pass). `Ok(None)` on 404 (not found / no access —
    /// Jira Cloud returns 404 for both). The KEY is validated by the caller (strict issue-key
    /// shape) — it is an identifier, not free text, so it needs no PII redaction.
    pub async fn get_issue(
        &self,
        key: &str,
    ) -> std::result::Result<Option<crate::verify::IssueSnapshot>, ConnectorError> {
        let client = reqwest::Client::new();
        let resp = client
            .get(format!(
                "{}/rest/api/3/issue/{key}?fields=summary,status,duedate",
                self.base_url
            ))
            .basic_auth(&self.email, Some(&self.api_token))
            .header("Accept", "application/json")
            .send()
            .await
            .map_err(|e| ConnectorError::Failed(format!("jira issue request: {}", e.without_url())))?;
        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        let status = resp.status();
        if !status.is_success() {
            tracing::warn!(target: "connector", provider = "jira", status = status.as_u16(), "jira issue HTTP error");
            return Err(ConnectorError::Failed(format!("jira HTTP {status}")));
        }
        let text = resp
            .text()
            .await
            .map_err(|e| ConnectorError::Failed(format!("jira body: {}", e.without_url())))?;
        Self::parse_issue(&text, &self.base_url)
    }

    /// Parse a single-issue body into a snapshot (fixture-testable, no network).
    pub(crate) fn parse_issue(
        body: &str,
        base_url: &str,
    ) -> std::result::Result<Option<crate::verify::IssueSnapshot>, ConnectorError> {
        let issue: JiraIssue = serde_json::from_str(body)
            .map_err(|e| ConnectorError::Failed(format!("jira issue parse: {e}")))?;
        if issue.key.trim().is_empty() {
            return Ok(None);
        }
        let f = issue.fields;
        Ok(Some(crate::verify::IssueSnapshot {
            key: issue.key.clone(),
            summary: f.summary.unwrap_or_default(),
            status: f.status.and_then(|s| s.name).unwrap_or_default(),
            due: f.duedate,
            url: format!("{base_url}/browse/{}", issue.key),
        }))
    }
```

Add a fixture test in `jira.rs`:

```rust
    #[test]
    fn jira_issue_parser_maps_snapshot() {
        let body = r#"{"key":"PROJ-1","fields":{"summary":"Fix login","status":{"name":"In Progress"},"duedate":"2026-07-10"}}"#;
        let s = JiraConnector::parse_issue(body, "https://acme.atlassian.net").unwrap().unwrap();
        assert_eq!(s.key, "PROJ-1");
        assert_eq!(s.status, "In Progress");
        assert_eq!(s.due.as_deref(), Some("2026-07-10"));
        assert_eq!(s.url, "https://acme.atlassian.net/browse/PROJ-1");
    }
```

In `mod.rs` add to `impl ConnectorRegistry` (below `search`):

```rust
    /// VERIFY-PASS lookup: fetch one Jira issue's live state through the SAME fail-closed +
    /// ledgered discipline as `search`. The key is a strict issue identifier (validated here),
    /// not free text — no PII redaction applies, but the attempt IS recorded content-free.
    pub async fn jira_lookup(
        &self,
        key: &str,
    ) -> std::result::Result<Option<crate::verify::IssueSnapshot>, ConnectorError> {
        // Strict shape guard BEFORE anything else (defense-in-depth against URL injection).
        let valid = crate::verify::extract_issue_keys(key)
            .first()
            .map(|(_, k)| k == key)
            .unwrap_or(false);
        if !valid {
            return Err(ConnectorError::Failed("invalid issue key".into()));
        }
        let Some(connector) = self.connectors.iter().find(|c| c.id() == "jira") else {
            return Err(ConnectorError::NeedsConsent);
        };
        if connector.egress_class() == EgressClass::External {
            self.sink.record(EgressEntry {
                provider_id: "jira".to_string(),
                destination: "Jira issue lookup (connector)".to_string(),
                model_requested: String::new(),
                call_kind: "connector_lookup",
                meta: CallMeta::default(),
                redactions: Default::default(),
                system_bytes: 0,
                user_bytes: key.len(),
                meeting_id: None,
            });
        }
        // Downcast-free dispatch: the registry stores `dyn Connector`; the lookup is Jira-only in
        // v1, so rebuild the concrete connector from the SAME config-derived state is not possible
        // here — instead expose lookup via the trait with a default "unsupported" impl:
        connector.lookup(key).await
    }
```

and extend the `Connector` trait (in the same file) with a defaulted method so no other connector changes:

```rust
    /// OPTIONAL identifier lookup (verify pass). Default: unsupported. `id` is a strict,
    /// caller-validated identifier (e.g. a Jira issue key), never free text.
    async fn lookup(
        &self,
        _id: &str,
    ) -> std::result::Result<Option<crate::verify::IssueSnapshot>, ConnectorError> {
        Err(ConnectorError::Unconfigured("lookup not supported".into()))
    }
```

then in `jira.rs` implement it on the `Connector` impl:

```rust
    async fn lookup(
        &self,
        id: &str,
    ) -> std::result::Result<Option<crate::verify::IssueSnapshot>, ConnectorError> {
        self.get_issue(id).await
    }
```

(Check the `redactions: Default::default()` field type — grep the `EgressEntry` struct and its redaction-counts type; if it lacks `Default`, construct the zero-counts value the way the tests at `mod.rs:491` read it.)

- [ ] **Step 4: GREEN.** `cargo test --lib jira_lookup && cargo test --lib jira` → PASS.

- [ ] **Step 5: Commit.**

```bash
git add src-tauri/src/connectors/
git commit -m "feat(connectors): Jira issue lookup via the registry - fail-closed, strict-key, ledgered"
```

### Task 4.3: Commands `verify_note_sources` + `apply_note_verify_markers`

**Files:**
- Modify: `src-tauri/src/commands.rs`, `src-tauri/src/lib.rs`

**Interfaces:**
- Produces: `verify_note_sources(meeting_id) -> Vec<crate::verify::VerifyFinding>` (async), `apply_note_verify_markers(meeting_id, findings: Vec<crate::verify::VerifyFinding>) -> NoteDto` (sync).

- [ ] **Step 1: Implement** (place next to `update_note`, `commands.rs:1147`; grep `get_latest_note_for_meeting` + copy `update_note`'s save/re-export tail):

```rust
/// VERIFY PASS (read-only): extract Jira issue keys from the meeting's note and check each against
/// LIVE Jira. GATED: sealed-not-unlocked meetings refuse (a verify against a blanked note would be
/// nonsense AND a read-gate bypass). Consent-gated: rides the Jira connector's enable+consent+key
/// gate (fail-closed `NeedsConsent` maps to `AppError::Unavailable`). NEVER called proactively —
/// FE-invoked only. Findings are computed against the note WITH OLD MARKERS STRIPPED so line
/// numbers line up with `apply_verify_markers`' post-strip numbering.
#[tauri::command]
pub async fn verify_note_sources(
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<crate::verify::VerifyFinding>, AppError> {
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to verify the note".into(),
        ));
    }
    let note = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;
    // Strip our own old markers so extraction/judgment sees the canonical note lines.
    let stripped = crate::verify::apply_verify_markers(&note.markdown, &[]);
    let keys = crate::verify::extract_issue_keys(&stripped);
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let config = state.config.lock().map_err(|_| AppError::Other(anyhow::anyhow!("config lock")))?.clone();
    let registry = crate::connectors::ConnectorRegistry::build(&config);
    let lines: Vec<&str> = stripped.lines().collect();
    let mut findings = Vec::with_capacity(keys.len());
    for (line_no, key) in keys {
        let snap = registry.jira_lookup(&key).await.map_err(AppError::from)?;
        let line_text = lines.get(line_no - 1).copied().unwrap_or("");
        let (verdict, detail) = crate::verify::judge(line_text, &key, snap.as_ref());
        let url = snap.map(|s| s.url).unwrap_or_default();
        findings.push(crate::verify::VerifyFinding { line_no, key, verdict, detail, url });
    }
    Ok(findings)
}

/// Apply verify markers to the note (WRITE — same gate + save/re-export tail as `update_note`).
/// Takes the findings the user just reviewed in the panel; validates every key's strict shape.
#[tauri::command]
pub fn apply_note_verify_markers(
    state: State<'_, AppState>,
    meeting_id: String,
    findings: Vec<crate::verify::VerifyFinding>,
) -> Result<NoteDto, AppError> {
    if !meeting_is_unlocked(state.inner(), &meeting_id)? {
        return Err(AppError::Locked(
            "this meeting's folder is locked — unlock it to edit the note".into(),
        ));
    }
    for f in &findings {
        let ok = crate::verify::extract_issue_keys(&f.key)
            .first()
            .map(|(_, k)| k == &f.key)
            .unwrap_or(false);
        if !ok {
            return Err(AppError::InvalidArg(format!("invalid issue key in findings")));
        }
    }
    let existing = state
        .db
        .get_latest_note_for_meeting(&meeting_id)?
        .ok_or_else(|| AppError::InvalidArg(format!("no note for meeting {meeting_id}")))?;
    let marked = crate::verify::apply_verify_markers(&existing.markdown, &findings);
    // Save + re-export — the exact `update_note` tail (commands.rs:1147), with `marked`.
    let created_at = chrono::Utc::now().to_rfc3339();
    state.db.upsert_note(&NoteRecord {
        meeting_id: meeting_id.clone(),
        provider_id: existing.provider_id.clone(),
        markdown: marked.clone(),
        created_at,
        exported_path: existing.exported_path.clone(),
        model_requested: existing.model_requested.clone(),
        model_served: existing.model_served.clone(),
        gateway_host: existing.gateway_host.clone(),
    })?;
    if let Some(path) = existing.exported_path.as_deref() {
        crate::export::overwrite_note(std::path::Path::new(path), &marked)?;
    }
    Ok(NoteDto {
        meeting_id,
        provider_id: existing.provider_id,
        markdown: marked,
        exported_path: existing.exported_path,
    })
}
```

(Verify the `NoteRecord`/`NoteDto` field lists against the current `update_note` before compiling — they are copied from it; if `update_note` gained fields since, mirror the current tail.) Also grep how `state.config` is actually accessed in async commands (`commands.rs` has many examples — e.g. `execute_web_search` callers; copy the exact lock/clone idiom used there instead of my sketch if it differs).

- [ ] **Step 2:** Register both in `lib.rs` `generate_handler![…]`.

- [ ] **Step 3: Tests** (in `commands.rs` tests — grep how neighboring command tests build a test `AppState`; if command-level tests aren't the local pattern, cover the gate indirectly): the load-bearing unit tests already live in `verify.rs`; add ONE gate test if the file's test harness supports building `AppState` (grep `fn build_state` in commands tests — memory says it exists): sealed meeting → `verify_note_sources` returns `AppError::Locked` (RED-before-GREEN by asserting on a sealed fixture).

- [ ] **Step 4:** `cargo test --lib` → green. Commit:

```bash
git add src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat(commands): verify_note_sources + apply_note_verify_markers - gated, consent-riding, strict-key"
```

### Task 4.4: FE — Verify panel in the note detail

**Files:**
- Create: `src/app/features/detail/verify-panel/verify-panel.component.{ts,html,scss}`
- Modify: `src/app/core/ipc.service.ts`, `src/app/core/models.ts`
- Modify: `src/app/features/detail/detail/detail.component.{ts,html}` (the Note tab)

**Interfaces:**
- Consumes: `verify_note_sources` / `apply_note_verify_markers` commands.
- Produces: `<app-verify-panel [meetingId]="…" (noteChanged)="…">`.

- [ ] **Step 1: DTO + IPC.** `models.ts`:

```ts
export interface VerifyFindingDto {
  lineNo: number;
  key: string;
  verdict: "confirmed" | "notfound" | "conflict";
  detail: string;
  url: string;
}
```

(**Check the serde casing**: `Verdict::NotFound` with `rename_all = "lowercase"` serializes as `"notfound"` — confirm against a quick `cargo test` debug or adjust the backend to `#[serde(rename_all = "snake_case")]` → `"not_found"` and mirror here; pick ONE and test it.)

`ipc.service.ts`:

```ts
  verifyNoteSources(meetingId: string): Promise<VerifyFindingDto[]> {
    return invoke<VerifyFindingDto[]>("verify_note_sources", { meetingId });
  }
  applyNoteVerifyMarkers(meetingId: string, findings: VerifyFindingDto[]): Promise<NoteDto> {
    return invoke<NoteDto>("apply_note_verify_markers", { meetingId, findings });
  }
```

- [ ] **Step 2: Component.** `verify-panel.component.ts`:

```ts
import {
  ChangeDetectionStrategy,
  Component,
  inject,
  input,
  output,
  signal,
} from "@angular/core";
import { IpcService } from "../../../core/ipc.service";
import type { VerifyFindingDto } from "../../../core/models";

/**
 * VERIFY WITH JIRA — on-demand deterministic check of the note's ticket claims against LIVE Jira
 * (docs/research/2026-07-05-connectors-live-vs-rag.md). On-demand ONLY (an explicit click = the
 * egress consent moment is visible); results render as a list; "Add markers to note" persists the
 * non-destructive > blockquote markers through the gated backend command.
 */
@Component({
  selector: "app-verify-panel",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./verify-panel.component.html",
  styleUrl: "./verify-panel.component.scss",
})
export class VerifyPanelComponent {
  private readonly ipc = inject(IpcService);

  readonly meetingId = input.required<string>();
  /** Fires after markers are applied so the parent reloads the note. */
  readonly noteChanged = output<void>();

  readonly running = signal(false);
  readonly applied = signal(false);
  readonly findings = signal<VerifyFindingDto[] | null>(null);
  readonly error = signal<string | null>(null);

  glyph(v: VerifyFindingDto["verdict"]): string {
    return v === "confirmed" ? "✓" : v === "conflict" ? "⧗" : "⚠";
  }

  async verify(): Promise<void> {
    if (this.running()) return;
    this.running.set(true);
    this.error.set(null);
    this.applied.set(false);
    try {
      this.findings.set(await this.ipc.verifyNoteSources(this.meetingId()));
    } catch (e) {
      this.error.set(e instanceof Error ? e.message : String(e));
    } finally {
      this.running.set(false);
    }
  }

  async apply(): Promise<void> {
    const f = this.findings();
    if (!f || this.running()) return;
    this.running.set(true);
    try {
      await this.ipc.applyNoteVerifyMarkers(this.meetingId(), f);
      this.applied.set(true);
      this.noteChanged.emit();
    } catch (e) {
      this.error.set(e instanceof Error ? e.message : String(e));
    } finally {
      this.running.set(false);
    }
  }
}
```

`verify-panel.component.html`:

```html
<div class="verify-panel">
  <div class="verify-head">
    <button type="button" class="btn btn-ghost" (click)="verify()" [disabled]="running()">
      @if (running()) { Checking Jira… } @else { Verify with Jira }
    </button>
    @if (findings(); as f) {
      @if (f.length > 0 && !applied()) {
        <button type="button" class="btn btn-primary" (click)="apply()" [disabled]="running()">
          Add markers to note
        </button>
      }
    }
  </div>

  @if (findings(); as f) {
    @if (f.length === 0) {
      <p class="text-secondary">No ticket references found in this note.</p>
    } @else {
      <ul class="verify-list">
        @for (item of f; track item.key) {
          <li class="verify-item" [class]="'is-' + item.verdict">
            <span class="verify-glyph">{{ glyph(item.verdict) }}</span>
            <span class="verify-detail">{{ item.detail }}</span>
            @if (item.url) {
              <a [href]="item.url" target="_blank" rel="noopener">{{ item.key }}</a>
            }
          </li>
        }
      </ul>
    }
  }
  @if (applied()) {
    <p class="text-secondary">Markers added — the note now carries the verification inline.</p>
  }
  @if (error(); as e) {
    <p class="banner is-warning" role="alert">{{ e }}</p>
  }
</div>
```

`verify-panel.component.scss`:

```scss
.verify-panel {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
}
.verify-head {
  display: flex;
  gap: var(--space-2);
  align-items: center;
}
.verify-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: flex;
  flex-direction: column;
  gap: var(--space-1);
}
.verify-item {
  display: flex;
  gap: var(--space-2);
  align-items: baseline;
  &.is-conflict .verify-glyph { color: var(--live, orange); }
  &.is-notfound .verify-glyph { color: var(--text-secondary); }
  &.is-confirmed .verify-glyph { color: var(--accent); }
}
```

(Token names `--live`/`--accent`/`--text-secondary` — grep `src/design-tokens/colors.css` for the real warn/success tokens and use those; never a raw hex.)

- [ ] **Step 3: Mount in detail.** In `detail.component.html`, inside the Note tab (grep the tab structure — memory: Note/Audio/Share tabs from #194), below the note body render:

```html
@if (config()?.jiraEnabled && config()?.jiraConsented && !detail()?.locked) {
  <app-verify-panel [meetingId]="meetingId()" (noteChanged)="reloadDetail()" />
}
```

(Grep the component's real signals: the config signal, the locked flag on the detail DTO, the note-reload method — use the actual names; the panel must be HIDDEN for locked meetings and when Jira isn't consented.) Add `VerifyPanelComponent` to `imports`.

- [ ] **Step 4:** `npx ng lint && npx ng build` → clean. Playwright smoke with mocked invoke: `verify_note_sources` returns a 3-finding fixture (one of each verdict) → panel renders glyphs; `apply_note_verify_markers` called on Apply; panel absent when `locked:true`.

- [ ] **Step 5: Commit + PR.**

```bash
git add src/app src-tauri/src
git commit -m "feat(detail): Verify with Jira panel - findings list + inline note markers"
gh pr create -R murmur-io/murmur --title "Note verify pass (deterministic, live Jira)" --body "On-demand verify: regex-extracted ticket claims checked against LIVE Jira (no LLM judge - pure deterministic compare, injection-resistant by construction), idempotent non-destructive markers mirroring annotate_unverified. Gated by meeting_is_unlocked + the Jira connector consent; every lookup egress-ledgered; never proactive."
```

**Phase 4 verify gates (both required):** (1) **lock-security-reviewer** — sealed meeting refuses both commands (`AppError::Locked`)? markers never applied to a blanked note? lookup fail-closed + ledgered + strict-key? findings' `detail` carries only connector values + note-line dates (no note content beyond the claim's own date)? (2) **adversarial-verifier** — full gates + live Playwright + the idempotency property (apply twice = byte-identical) + RED-before-GREEN on the sealed-refusal test. Real-Jira round-trip = manual smoke, recorded.

---

## Execution order & dependencies

```
Phase 1 (FE-only)            — independent, ship first (highest leverage)
Phase 2 (Jira)               — independent of 1
Phase 3 (Slack)              — after 2 merges (same files: config.rs, tools.rs, mod.rs, connectors section — rebase, don't parallelize)
Phase 4 (Verify)             — after 2 merges (extends JiraConnector); independent of 3
```

Each phase: branch → tasks in order → `bash scripts/ci.sh` ONCE at the end → PR → the phase's verify gates → merge. The implementer NEVER self-certifies — the adversarial-verifier (and lock-security-reviewer where marked) owns PASS/FAIL.

## Post-plan follow-ups (explicitly OUT of scope)

- Linear connector (same clone; after Slack).
- Slack existence-check sub-profile in the verify pass (search-existence, not value-compare).
- Session RAM cache for connector hits (add only if dogfooding shows repeat-fetch latency).
- Pin-to-note (connector hit → owned document via `import_text`).
- Write-out (create Jira issue via propose-accept) — separate track, lethal-trifecta review required.
