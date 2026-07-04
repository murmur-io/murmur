# AI & Models Settings Re-IA ("Jobs × Engines") Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restructure the Settings → AI & Models page around one mental model — *Jobs × Engines* — anchored by a new, always-visible **"What runs where" resolved map** that mirrors the backend role resolver, so a user can always answer "który model obsługuje którą funkcję".

**Architecture:** One new read-only backend projection (`ai_map_rows(cfg) -> Vec<AiMapRow>` + `resolved_ai_map` Tauri command) feeds a new FE map card. The rest is a frontend re-IA of the existing five blocks: rename/regroup ("Providers" → "Engines" with a first-class **Murmur Brain** card hosting the GGUF registry; "Default AI" → "Default engine"), honest posture copy, and the "On-device intelligence" card reduced to "Search index". **No dispatch behavior changes** — `roles::resolve` stays the single resolver; this plan only *mirrors* it.

**Tech Stack:** Rust (Tauri 2.11, `meetnotes_lib`), Angular 18 zoneless (standalone, signals, inline templates), no new dependencies.

## Global Constraints

- Binding rule files apply in full: `.claude/rules/rust-tauri.md`, `.claude/rules/angular-zoneless.md`, `.claude/rules/lock-model.md`, `.claude/rules/agentic-workflow.md`.
- **Cite by symbol, not line number** — `commands.rs`/`db.rs` line anchors drift; `rg` the symbol before editing.
- Rust: only `AppError` + `crate::error::Result<T>`; new command MUST be added to `generate_handler![…]` in `src-tauri/src/lib.rs` in the same change.
- **Lock model:** `resolved_ai_map` returns config-derived metadata only — NO meeting content, NO note text, NO keys. It is not a content read, so it needs no `meeting_is_unlocked`/`visibility_clause` gate; state this in the PR description so the reviewer doesn't have to re-derive it.
- No new config keys, no migrations — the whole plan is read-only over `AppConfig` plus FE re-IA.
- Angular: standalone + `OnPush` + inline `template`/`styles`; signals only (no plain mutable state); `@if`/`@for` only; `var(--token)` for all colors/spacing; 16 kB per-component style budget; effects that write signals need `{ allowSignalWrites: true }`; NO new npm packages.
- Test loop: `( cd src-tauri && cargo test --lib )` — NEVER `cargo clippy --all-targets`. Full `bash scripts/ci.sh` exactly ONCE at the end.
- Commits authored by `QueaT <kgm004a@gmail.com>` (repo git user already set), **no Claude/Co-Authored-By trailers**. Branch `feat/ai-settings-reia`; merge to `murmur` trunk **via PR only** (`gh pr create` → merge; direct push is hook-blocked). `gh` account = JakubGawr, repo = `murmur-io/murmur`.
- The implementer never self-certifies: the final task dispatches the **adversarial-verifier** for the PASS/FAIL verdict.
- App copy stays English (product language); keep the existing tone (short, honest, no marketing).

## File Structure

| File | Action | Responsibility |
| --- | --- | --- |
| `src-tauri/src/settings/ai_map.rs` | Create | Pure `ai_map_rows(cfg)` projection + `AiMapRow` DTO + unit tests |
| `src-tauri/src/settings/mod.rs` | Modify | `pub mod ai_map;` |
| `src-tauri/src/commands.rs` | Modify | `resolved_ai_map` command (next to `brain_posture`) |
| `src-tauri/src/lib.rs` | Modify | Register `commands::resolved_ai_map` |
| `src/app/core/models.ts` | Modify | `AiMapRow` interface |
| `src/app/core/ipc.service.ts` | Modify | `resolvedAiMap()` method |
| `src/app/features/settings/settings.store.ts` | Modify | `aiMap` signal + `refreshAiMap()` + hoisted `advancedExpanded` |
| `src/app/features/settings/sections/ai/ai-resolved-map.component.ts` | Create | The "What runs where" card |
| `src/app/features/settings/sections/settings-ai-section.component.ts` | Modify | Insert map card between posture and Advanced |
| `src/app/features/settings/sections/ai/brain-posture-block.component.ts` | Modify | Honest copy (intro + Cloud subtitle) |
| `src/app/features/settings/sections/ai/ai-advanced-block.component.ts` | Modify | Store-owned expansion; "Default engine" naming; toggle label |
| `src/app/features/settings/sections/ai/ai-connection-cards.component.ts` | Modify | "Providers"→"Engines" header; render Murmur Brain card first |
| `src/app/features/settings/sections/ai/brain-engine-card.component.ts` | Create | Built-in brain engine card wrapping the GGUF registry |
| `src/app/features/settings/sections/ai/ai-connection-card.component.ts` | Modify | Ollama differentiation note |
| `src/app/features/settings/sections/ai/ai-role-rows.component.ts` | Modify | Grouped `<optgroup>` options; GGUF list replaced with pointer to Engines |
| `src/app/features/settings/sections/ai/on-device-intelligence-block.component.ts` | Modify | Reduce to "Search index" (badges removed — the map owns them) |

Explicitly **out of scope** (follow-ups, do NOT attempt here): per-connection model keys for claude_code (unifying `provider_model` into cards would touch the roles zero-behavior-change contract), deep-link row→specific control scrolling, localized copy.

---

### Task 1: Backend — `ai_map` projection + `resolved_ai_map` command

**Files:**
- Create: `src-tauri/src/settings/ai_map.rs`
- Modify: `src-tauri/src/settings/mod.rs` (add `pub mod ai_map;` next to the existing `pub mod postures;`)
- Modify: `src-tauri/src/commands.rs` (new command — place directly below `set_brain_posture`; find it with `rg -n "pub fn set_brain_posture" src-tauri/src/commands.rs`)
- Modify: `src-tauri/src/lib.rs` (register in `generate_handler![…]` next to `commands::brain_posture,`)

**Interfaces:**
- Consumes: `roles::resolve(Role, &AppConfig) -> RoleTarget`, `roles::connection_display_name(&str) -> &str`, `roles::{CONN_LOCAL, CONN_OFF, CONN_AFM}` (all in `src-tauri/src/summarize/roles.rs`); `crate::summarize::egress_is_cloud(id, cfg)` (`pub(crate)`, `src-tauri/src/summarize/mod.rs`); `crate::reason::{brain_model_by_id, class_model_id, ModelClass}`; `crate::embed::{embed_model_by_id, default_embed_model}`; config fields `provider_id`, `ollama_model`, `gateway_model`, `anthropic_model`, `model_size`, `embed_model_id`, `brain_live`, `semantic_search_enabled` (verify each with `rg -n "<field>" src-tauri/src/settings/config.rs` before use).
- Produces: `pub fn ai_map_rows(cfg: &AppConfig) -> Vec<AiMapRow>` and `#[tauri::command] pub fn resolved_ai_map(state) -> Result<Vec<AiMapRow>>` returning camelCase-serialized rows `{job, title, engine, model, onDevice, redacted, active, routable}` — the contract Tasks 2–3 rely on.

- [ ] **Step 1: Create the branch**

```bash
git checkout murmur && git pull && git checkout -b feat/ai-settings-reia
```

- [ ] **Step 2: Write the module with failing tests first**

Create `src-tauri/src/settings/ai_map.rs` with ONLY the imports + `AiMapRow` struct + the `#[cfg(test)]` module below (no `ai_map_rows`/`role_row`/`brain_display` yet) — the tests reference `ai_map_rows`, so the crate fails to compile: that is the RED state.

```rust
//! The "What runs where" RESOLVED AI MAP — a pure, read-only projection of the config into the
//! per-job (engine, model, locality) rows the Settings AI page renders. Display-only: nothing
//! here steers dispatch ([`crate::summarize::roles::resolve`] stays the one resolver); this
//! module only MIRRORS it, so the table can never disagree with backend truth. No content, no
//! PII, no key material — config-derived metadata only.

use serde::Serialize;

use crate::reason::{brain_model_by_id, class_model_id, ModelClass};
use crate::settings::AppConfig;
use crate::summarize::roles::{self, Role, CONN_AFM, CONN_LOCAL, CONN_OFF};

/// One row of the resolved map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiMapRow {
    /// Stable job token the FE keys on: `notes` | `ask` | `live` | `reactions` |
    /// `transcription` | `embeddings` | `redaction`.
    pub job: String,
    /// Display title ("Notes & summaries").
    pub title: String,
    /// Display engine name ("Claude Code", the GGUF's registry name, "Whisper", …).
    pub engine: String,
    /// Resolved model id ("" = the engine's own default).
    pub model: String,
    /// True when this job cannot egress (on-device / loopback Ollama).
    pub on_device: bool,
    /// True when the job's text passes the redaction firewall before leaving (cloud engines).
    pub redacted: bool,
    /// False when the job is currently switched off (reactions without `brain_live`,
    /// embeddings with semantic search off).
    pub active: bool,
    /// True for the three routable roles (notes/ask/live) — the FE offers "Change".
    pub routable: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::postures::{apply_posture, Posture};

    fn row<'a>(rows: &'a [AiMapRow], job: &str) -> &'a AiMapRow {
        rows.iter().find(|r| r.job == job).unwrap()
    }

    #[test]
    fn default_config_is_cloud_claude_with_inactive_reactions() {
        let rows = ai_map_rows(&AppConfig::default());
        assert_eq!(rows.len(), 7);
        let notes = row(&rows, "notes");
        assert_eq!(notes.engine, "Claude Code");
        assert!(!notes.on_device);
        assert!(notes.redacted);
        assert!(notes.routable);
        let reactions = row(&rows, "reactions");
        assert!(reactions.on_device);
        assert!(!reactions.active, "brain_live defaults off ⇒ reactions row inactive");
        assert!(!reactions.routable);
        let tr = row(&rows, "transcription");
        assert_eq!(tr.engine, "Whisper");
        assert_eq!(tr.model, "small");
        assert!(tr.on_device);
    }

    #[test]
    fn fully_local_preset_maps_every_role_on_device() {
        let mut cfg = AppConfig::default();
        apply_posture(&mut cfg, Posture::FullyLocal);
        let rows = ai_map_rows(&cfg);
        for job in ["notes", "ask", "live"] {
            let r = row(&rows, job);
            assert!(r.on_device, "{job} must be on-device under Fully local");
            assert!(!r.redacted);
        }
        let heavy = crate::reason::brain_model_by_id("qwen3-4b-instruct-2507").unwrap();
        assert_eq!(row(&rows, "notes").engine, heavy.name);
        assert!(row(&rows, "reactions").active, "Fully local turns brain_live on");
    }

    #[test]
    fn ollama_default_resolves_its_own_model_and_loopback_is_on_device() {
        let cfg = AppConfig {
            provider_id: "ollama".to_string(),
            ..AppConfig::default()
        };
        let rows = ai_map_rows(&cfg);
        let notes = row(&rows, "notes");
        assert_eq!(notes.engine, "Ollama");
        assert_eq!(notes.model, cfg.ollama_model, "empty role model must fall back to ollama_model");
        assert!(notes.on_device, "loopback ollama must not classify as cloud");
        assert!(!notes.redacted);
    }

    #[test]
    fn semantic_off_renders_embeddings_inactive() {
        let cfg = AppConfig {
            semantic_search_enabled: false,
            ..AppConfig::default()
        };
        assert!(!row(&ai_map_rows(&cfg), "embeddings").active);
    }
}
```

Also add to `src-tauri/src/settings/mod.rs`, next to the existing `pub mod postures;`:

```rust
pub mod ai_map;
```

- [ ] **Step 3: Verify RED**

```bash
( cd src-tauri && source ~/.cargo/env && cargo test --lib settings::ai_map 2>&1 | tail -5 )
```

Expected: compile error `cannot find function ai_map_rows in this scope`. (First build after a cold checkout is slow — the ML tree; let it finish.)

- [ ] **Step 4: Implement the projection**

Add above the tests module in `ai_map.rs`:

```rust
/// Display name for an on-device brain model id — registry name, raw id when unknown,
/// generic label when empty.
fn brain_display(id: &str) -> String {
    brain_model_by_id(id).map(|m| m.name.to_string()).unwrap_or_else(|| {
        if id.is_empty() {
            "On-device brain".to_string()
        } else {
            id.to_string()
        }
    })
}

/// Build one routable role row by mirroring [`roles::resolve`].
fn role_row(job: &str, title: &str, role: Role, cfg: &AppConfig) -> AiMapRow {
    let t = roles::resolve(role, cfg);
    let conn = t.connection.as_str();
    let base = AiMapRow {
        job: job.to_string(),
        title: title.to_string(),
        engine: String::new(),
        model: String::new(),
        on_device: true,
        redacted: false,
        active: true,
        routable: true,
    };
    match conn {
        CONN_LOCAL => AiMapRow {
            engine: brain_display(&t.model),
            model: t.model.clone(),
            ..base
        },
        CONN_OFF => AiMapRow {
            engine: "Retrieval only (no model)".to_string(),
            ..base
        },
        CONN_AFM => AiMapRow {
            engine: "Apple Intelligence (on-device)".to_string(),
            ..base
        },
        _ => {
            let cloud = crate::summarize::egress_is_cloud(conn, cfg);
            // Show the model the connection will ACTUALLY use: an empty resolved model falls
            // back to the connection's own model key, mirroring the provider factory arms.
            let model = if !t.model.trim().is_empty() {
                t.model.clone()
            } else {
                match conn {
                    crate::summarize::PROVIDER_OLLAMA => cfg.ollama_model.clone(),
                    crate::summarize::PROVIDER_GATEWAY => cfg.gateway_model.clone(),
                    crate::summarize::PROVIDER_ANTHROPIC => cfg.anthropic_model.clone(),
                    _ => String::new(),
                }
            };
            AiMapRow {
                engine: roles::connection_display_name(conn).to_string(),
                model,
                on_device: !cloud,
                redacted: cloud,
                ..base
            }
        }
    }
}

/// The full resolved map in display order. Pure (config in, rows out).
pub fn ai_map_rows(cfg: &AppConfig) -> Vec<AiMapRow> {
    let light_id = class_model_id(cfg, ModelClass::Light).unwrap_or_default();
    let embed = cfg
        .embed_model_id
        .as_deref()
        .and_then(crate::embed::embed_model_by_id)
        .unwrap_or_else(crate::embed::default_embed_model);
    vec![
        role_row("notes", "Notes & summaries", Role::Notes, cfg),
        role_row("ask", "Ask & chat", Role::Ask, cfg),
        role_row("live", "Live @brain", Role::Live, cfg),
        AiMapRow {
            job: "reactions".to_string(),
            title: "Realtime reactions".to_string(),
            engine: brain_display(&light_id),
            model: light_id,
            on_device: true,
            redacted: false,
            active: cfg.brain_live,
            routable: false,
        },
        AiMapRow {
            job: "transcription".to_string(),
            title: "Transcription".to_string(),
            engine: "Whisper".to_string(),
            model: cfg.model_size.clone(),
            on_device: true,
            redacted: false,
            active: true,
            routable: false,
        },
        AiMapRow {
            job: "embeddings".to_string(),
            title: "Search index".to_string(),
            engine: embed.name.to_string(),
            model: embed.id.to_string(),
            on_device: true,
            redacted: false,
            active: cfg.semantic_search_enabled,
            routable: false,
        },
        AiMapRow {
            job: "redaction".to_string(),
            title: "Name redaction".to_string(),
            engine: "On-device NER".to_string(),
            model: String::new(),
            on_device: true,
            redacted: false,
            active: true,
            routable: false,
        },
    ]
}
```

NOTE: `embed_model_by_id`/`default_embed_model` resolve straight from `cfg.embed_model_id` on purpose — do NOT use `crate::embed::selected_embed_model()` (a process-global other tests mutate; using it would make these tests flaky under parallel `cargo test`). If any consumed symbol is private or differently named, fix the import per the real signature (`rg -n "pub fn brain_model_by_id|pub fn class_model_id|pub fn embed_model_by_id|pub fn default_embed_model" src-tauri/src/`) — do not weaken visibility beyond `pub(crate)` needs.

- [ ] **Step 5: Verify the module tests GREEN**

```bash
( cd src-tauri && source ~/.cargo/env && cargo test --lib settings::ai_map 2>&1 | tail -5 )
```

Expected: `test result: ok. 4 passed`.

- [ ] **Step 6: Add the Tauri command + register it**

In `src-tauri/src/commands.rs`, directly below `set_brain_posture` (same access pattern — `state.config.lock()`):

```rust
/// The RESOLVED "what runs where" map for the Settings AI page — one row per AI job with its
/// resolved engine/model/locality (mirrors `roles::resolve`; display-only, steers nothing).
/// Read-only config projection: no content, no PII, no keys — NOT a gated content read.
#[tauri::command]
pub fn resolved_ai_map(
    state: State<'_, AppState>,
) -> Result<Vec<crate::settings::ai_map::AiMapRow>, AppError> {
    let c = state
        .config
        .lock()
        .map_err(|_| AppError::Config("config mutex poisoned".into()))?;
    Ok(crate::settings::ai_map::ai_map_rows(&c))
}
```

In `src-tauri/src/lib.rs`, add to `generate_handler![…]` next to `commands::brain_posture,`:

```rust
            commands::resolved_ai_map,
```

- [ ] **Step 7: Full Rust test pass**

```bash
( cd src-tauri && source ~/.cargo/env && cargo test --lib 2>&1 | tail -3 )
```

Expected: `test result: ok.` (≈965+ tests, 0 failed).

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/settings/ai_map.rs src-tauri/src/settings/mod.rs src-tauri/src/commands.rs src-tauri/src/lib.rs
git commit -m "feat(settings): resolved AI map — ai_map_rows projection + resolved_ai_map command"
```

---

### Task 2: FE plumbing — type, IPC method, store signal

**Files:**
- Modify: `src/app/core/models.ts`
- Modify: `src/app/core/ipc.service.ts`
- Modify: `src/app/features/settings/settings.store.ts`

**Interfaces:**
- Consumes: the `resolved_ai_map` command from Task 1 (camelCase rows).
- Produces: `interface AiMapRow` in `models.ts`; `IpcService.resolvedAiMap(): Promise<AiMapRow[]>`; `SettingsStore.aiMap: Signal<AiMapRow[]>`, `SettingsStore.refreshAiMap(): Promise<void>`, `SettingsStore.advancedExpanded: WritableSignal<boolean>`, `SettingsStore.expandAdvanced(): void` — Task 3's contract.

- [ ] **Step 1: Add the type to `src/app/core/models.ts`** (near the existing `Posture` type — `rg -n "export type Posture" src/app/core/models.ts`):

```ts
/** One row of the Settings "What runs where" map (mirrors Rust `AiMapRow`, camelCase). */
export interface AiMapRow {
  job: string;
  title: string;
  engine: string;
  model: string;
  onDevice: boolean;
  redacted: boolean;
  active: boolean;
  routable: boolean;
}
```

- [ ] **Step 2: Add the IPC method to `src/app/core/ipc.service.ts`** (next to `brainPosture()` — mirror its exact invoke style, and add `AiMapRow` to the existing models import):

```ts
  /** The resolved "what runs where" rows for the Settings AI map (read-only config projection). */
  resolvedAiMap(): Promise<AiMapRow[]> {
    return this.invoke("resolved_ai_map");
  }
```

(If the file's one-shot methods call a different private wrapper than `this.invoke`, copy the adjacent `brainPosture` body verbatim and change only the command string + return type.)

- [ ] **Step 3: Add the store signal + refresh + hoisted Advanced expansion** to `src/app/features/settings/settings.store.ts` (place next to the posture signals — `rg -n "_posture = signal" settings.store.ts`; add `AiMapRow` to the models import):

```ts
  // ── "What runs where" resolved map ──────────────────────────────────────
  /** The backend-resolved per-job routing rows (resolved_ai_map). */
  private readonly _aiMap = signal<AiMapRow[]>([]);
  readonly aiMap = this._aiMap.asReadonly();

  /** Re-fetch the resolved map (load, after save, after a posture apply). Keeps last on failure. */
  async refreshAiMap(): Promise<void> {
    try {
      this._aiMap.set(await this.ipc.resolvedAiMap());
    } catch {
      // keep the last known map — the card renders its loading/emptiness state
    }
  }

  /**
   * Advanced-disclosure open state, HOISTED from AiAdvancedBlockComponent so the
   * map card's "Change" affordance can open it from outside.
   */
  readonly advancedExpanded = signal(false);
  expandAdvanced(): void {
    this.advancedExpanded.set(true);
  }
```

- [ ] **Step 4: Wire the refresh at the three freshness points** (grep `refreshPosture` in the store — it already marks exactly the right places):
  1. Inside `refreshPosture()`, after the posture fetch resolves, add `void this.refreshAiMap();` (covers `setPosture`, `setRoleConnection`, retirement apply — everything that already refreshes the posture).
  2. In `load()`, next to the existing `refreshPosture()`/`embedModelPresent` fetches, add `void this.refreshAiMap();`.
  3. At the end of a **successful** `save()`, add `void this.refreshAiMap();` (a saved role/provider edit changes dispatch — the map must follow; if `save()` already ends by calling `refreshPosture()`, point 1 covers it and this line is unnecessary — check first).

- [ ] **Step 5: Verify**

```bash
npx ng lint 2>&1 | tail -3 && npx ng build 2>&1 | tail -3
```

Expected: both clean (an unused-signal warning is acceptable until Task 3 consumes it; if lint errors on unused, fold this task's commit into Task 3's — preferred: commit both together only if lint blocks).

- [ ] **Step 6: Commit**

```bash
git add src/app/core/models.ts src/app/core/ipc.service.ts src/app/features/settings/settings.store.ts
git commit -m "feat(settings): AI map FE plumbing — AiMapRow type, IPC method, store signal"
```

---

### Task 3: The "What runs where" map card

**Files:**
- Create: `src/app/features/settings/sections/ai/ai-resolved-map.component.ts`
- Modify: `src/app/features/settings/sections/settings-ai-section.component.ts`
- Modify: `src/app/features/settings/sections/ai/ai-advanced-block.component.ts`

**Interfaces:**
- Consumes: `SettingsStore.aiMap`, `SettingsStore.expandAdvanced()`, `SettingsStore.advancedExpanded` (Task 2).
- Produces: `AiResolvedMapComponent` (selector `app-ai-resolved-map`) rendered between the posture block and Advanced.

- [ ] **Step 1: Create the component**

```ts
import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { SettingsStore } from "../../settings.store";

/**
 * AI & Models → "What runs where": the resolved-map card. A read-only,
 * always-visible mirror of the backend role resolver (`resolved_ai_map`) —
 * one row per AI job with the engine serving it RIGHT NOW. This is the
 * honesty layer of the posture redesign: the posture preset chooses, this
 * table shows the outcome, and a routable row's "Change" opens Advanced.
 * In-flow card (frosted .card is correct — not a floating overlay).
 */
@Component({
  selector: "app-ai-resolved-map",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="card map-card">
      <div class="map-head">
        <h3>What runs where</h3>
        <p class="text-secondary map-sub">
          Every AI job and the engine serving it right now.
        </p>
      </div>
      <div class="map-rows">
        @for (row of rows(); track row.job) {
          <div class="map-row" [class.is-inactive]="!row.active">
            <span class="map-title">{{ row.title }}</span>
            <span class="map-engine">
              {{ row.engine }}
              @if (row.model) {
                <span class="map-model text-muted">· {{ row.model }}</span>
              }
              @if (!row.active) {
                <span class="map-off text-muted">— off</span>
              }
            </span>
            <span class="pill map-loc" [class.is-success]="row.onDevice">
              <span class="pill-dot"></span>
              {{ row.onDevice ? "On this Mac" : "Cloud · redacted" }}
            </span>
            @if (row.routable) {
              <button
                type="button"
                class="btn btn-ghost btn-sm map-change"
                (click)="change()"
              >
                Change
              </button>
            } @else {
              <span class="map-change-spacer" aria-hidden="true"></span>
            }
          </div>
        } @empty {
          <p class="text-muted map-empty">Loading the routing map…</p>
        }
      </div>
    </div>
  `,
  styles: [
    `
      :host {
        display: contents;
      }
      .map-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .map-head {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .map-head h3 {
        margin: 0;
      }
      .map-sub {
        margin: 0;
        font-size: 0.8125rem;
      }
      .map-rows {
        display: flex;
        flex-direction: column;
      }
      .map-row {
        display: grid;
        grid-template-columns: minmax(150px, 0.55fr) 1fr auto auto;
        align-items: center;
        gap: var(--space-3);
        padding: var(--space-2) 0;
        border-top: 1px solid var(--border-subtle);
      }
      .map-row:first-of-type {
        border-top: none;
      }
      .map-row.is-inactive .map-title,
      .map-row.is-inactive .map-engine {
        opacity: 0.55;
      }
      .map-title {
        color: var(--text-secondary);
        font-size: 0.875rem;
        font-weight: 550;
      }
      .map-engine {
        display: flex;
        align-items: baseline;
        gap: var(--space-1);
        flex-wrap: wrap;
        font-size: 0.875rem;
        color: var(--text-primary);
        min-width: 0;
      }
      .map-model,
      .map-off,
      .map-empty {
        font-size: 0.8125rem;
      }
      .map-empty {
        margin: 0;
      }
      .map-loc {
        flex: none;
      }
      .map-change {
        flex: none;
        white-space: nowrap;
      }
      .map-change-spacer {
        width: 1px;
      }
    `,
  ],
})
export class AiResolvedMapComponent {
  private readonly store = inject(SettingsStore);
  readonly rows = this.store.aiMap;

  /** A routable row's Change → open the Advanced disclosure below. */
  change(): void {
    this.store.expandAdvanced();
  }
}
```

- [ ] **Step 2: Insert into the section** — `settings-ai-section.component.ts`: import `AiResolvedMapComponent`, add to `imports:`, and place in the template between the posture block and Advanced:

```html
    <app-brain-posture-block />
    <app-ai-resolved-map />
    <app-ai-advanced-block />
```

- [ ] **Step 3: Hoist the Advanced expansion to the store** — in `ai-advanced-block.component.ts` replace the local signal with the store's:

```ts
  /** Whether the Advanced disclosure is open — store-owned so the map's "Change" opens it. */
  readonly expanded = this.store.advancedExpanded;
```

(delete `readonly expanded = signal(false);` and the now-unused `signal` import if nothing else uses it), and change `toggle()` to:

```ts
  toggle(): void {
    this.store.advancedExpanded.update((v) => !v);
  }
```

The `_autoExpand` effect body stays but writes `this.store.advancedExpanded.set(true)` (it already has `allowSignalWrites: true`).

- [ ] **Step 4: Verify build + live smoke**

```bash
npx ng lint 2>&1 | tail -3 && npx ng build 2>&1 | tail -5
```

Expected: clean, style budget respected. Then a quick live check with the dev app or Playwright against `:1420` with mocked `window.__TAURI_INTERNALS__.invoke` (mock `resolved_ai_map` to return 7 rows shaped like Task 2's interface): the card renders 7 rows, inactive rows dim, "Change" expands Advanced.

- [ ] **Step 5: Commit**

```bash
git add src/app/features/settings/sections/ai/ai-resolved-map.component.ts src/app/features/settings/sections/settings-ai-section.component.ts src/app/features/settings/sections/ai/ai-advanced-block.component.ts
git commit -m "feat(settings): What-runs-where resolved map card + store-owned Advanced disclosure"
```

---

### Task 4: Naming + honest posture copy ("Default AI" → "Default engine")

**Files:**
- Modify: `src/app/features/settings/sections/ai/brain-posture-block.component.ts`
- Modify: `src/app/features/settings/sections/ai/ai-advanced-block.component.ts`

**Interfaces:** copy-only; no signals/IPC change. (The store's `defaultAiSummary`/`notesInheritSummary` strings in `settings.store.ts` also say "Default AI" — update those two strings to "Default engine" for consistency: `rg -n "Default AI" src/app`.)

- [ ] **Step 1: Posture block copy** — in `brain-posture-block.component.ts`:
  - Intro `<p class="text-secondary posture-intro">` becomes:
    `Pick how much runs on this Mac. The map below shows exactly what runs where — tune it under Advanced.`
  - Cloud option sub `Your Default AI does everything` becomes `Your Default engine does everything`.
  - Hybrid option sub `Cloud notes + on-device reactions` becomes `Cloud notes + realtime reactions on this Mac`.
  - (Fully local sub `Nothing leaves this Mac` stays.)

- [ ] **Step 2: Advanced block naming** — in `ai-advanced-block.component.ts`:
  - Toggle button text `⚙ Advanced — connections, models, per-feature` becomes `⚙ Advanced — engines & routing`.
  - Field label `Default AI` becomes `Default engine`.
  - Its `@else` help text becomes: `Runs Notes, Ask and @brain unless a per-feature override below says otherwise. Cloud engines are redacted first. Set engines up in the Engines block above.`
  - The fully-local note stays (`Not used — Fully local runs notes on-device.`).

- [ ] **Step 3: Sweep the remaining user-visible "Default AI" strings** found by `rg -n "Default AI" src/app` (expected: `defaultAiSummary`'s callers `notesInheritSummary`/`assistantInheritSummary` in `settings.store.ts` — change the literal `Follows Default AI:` to `Follows the Default engine:`). Do NOT rename identifiers (`defaultAiSummary` etc.) — strings only, zero-risk diff.

- [ ] **Step 4: Verify + commit**

```bash
npx ng lint 2>&1 | tail -3 && npx ng build 2>&1 | tail -3
git add -A src/app && git commit -m "refactor(settings): Default engine naming + honest posture copy"
```

---

### Task 5: Engines block — Murmur Brain card + GGUF registry relocation

**Files:**
- Create: `src/app/features/settings/sections/ai/brain-engine-card.component.ts`
- Modify: `src/app/features/settings/sections/ai/ai-connection-cards.component.ts`
- Modify: `src/app/features/settings/sections/ai/ai-connection-card.component.ts`
- Modify: `src/app/features/settings/sections/ai/ai-role-rows.component.ts`

**Interfaces:**
- Consumes: `SettingsStore.brainModels` (`BrainModelDto[]`, each with `downloaded: boolean`), `LocalModelsListComponent` (existing, store-driven — owns the `brainModelId` control).
- Produces: `BrainEngineCardComponent` (selector `app-brain-engine-card`) rendered first in the "On this Mac" group.

- [ ] **Step 1: Create `brain-engine-card.component.ts`**

```ts
import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  signal,
} from "@angular/core";
import { SettingsStore } from "../../settings.store";
import { LocalModelsListComponent } from "./local-models-list.component";

/**
 * Engines → "Murmur Brain" card: the BUILT-IN on-device engine (managed GGUF
 * downloads, light/heavy classes). Rendered first in the "On this Mac" group
 * so the built-in brain and Ollama (an external local server) stop being
 * conflated. The Configure disclosure hosts the shared GGUF registry
 * (LocalModelsListComponent — moved here from the role rows). In-flow
 * disclosure, not an overlay (T3).
 */
@Component({
  selector: "app-brain-engine-card",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [LocalModelsListComponent],
  template: `
    <div class="brain-card">
      <div class="brain-row">
        <div class="brain-main">
          <span class="brain-name">Murmur Brain</span>
          @if (ready()) {
            <span class="pill is-success">
              <span class="pill-dot"></span>
              Ready
            </span>
          } @else {
            <span class="pill">
              <span class="pill-dot"></span>
              No model downloaded
            </span>
          }
        </div>
        <button
          type="button"
          class="btn btn-sm"
          (click)="toggle()"
          [attr.aria-expanded]="expanded()"
        >
          Configure
          <svg
            class="brain-chevron"
            [class.is-open]="expanded()"
            viewBox="0 0 16 16"
            width="12"
            height="12"
            fill="none"
            stroke="currentColor"
            stroke-width="1.8"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <path d="M4 6.5 8 10.5 12 6.5" />
          </svg>
        </button>
      </div>
      <span class="brain-privacy text-muted">
        Built into Murmur — managed models, nothing leaves this Mac. Powers
        Realtime reactions and the Fully local posture.
      </span>
      @if (expanded()) {
        <div class="brain-config">
          <app-local-models-list />
        </div>
      }
    </div>
  `,
  styles: [
    `
      :host {
        display: contents;
      }
      .brain-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        padding: var(--space-3) var(--space-4);
        border: 1px solid var(--border-subtle);
        border-radius: var(--radius-md);
        background: var(--surface-input);
      }
      .brain-row {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-3);
      }
      .brain-main {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        min-width: 0;
      }
      .brain-name {
        font-size: 0.9375rem;
        font-weight: 600;
        color: var(--text-primary);
      }
      .brain-privacy {
        font-size: 0.8125rem;
      }
      .brain-chevron {
        margin-left: var(--space-1);
        transition: transform var(--transition);
      }
      .brain-chevron.is-open {
        transform: rotate(180deg);
      }
      .brain-config {
        padding-top: var(--space-2);
        border-top: 1px solid var(--border-subtle);
      }
    `,
  ],
})
export class BrainEngineCardComponent {
  private readonly store = inject(SettingsStore);

  /** Whether the Configure disclosure is open. */
  readonly expanded = signal(false);

  /** Ready = at least one registry GGUF is on disk. */
  readonly ready = computed(() =>
    this.store.brainModels().some((m) => m.downloaded),
  );

  toggle(): void {
    this.expanded.update((v) => !v);
  }
}
```

Match `.brain-card`'s visual to the existing `.conn-card` in `ai-connection-card.component.ts` (open it and copy its `padding`/`border`/`background` values verbatim if they differ from the above).

- [ ] **Step 2: Render it + rename the header** in `ai-connection-cards.component.ts`:
  - Import + add `BrainEngineCardComponent` to `imports:`.
  - `<h3>Providers</h3>` → `<h3>Engines</h3>`; the sub-copy becomes: `Where models can run. Set each engine up once — pick which one Murmur uses under Default engine below.`
  - Inside the `"On this Mac"` group, render `<app-brain-engine-card />` BEFORE the `@for (c of localCards(); …)`.
  - The `@empty` copy (now sitting under the always-present brain card) becomes: `Ollama appears here only while its base URL points at this Mac.`

- [ ] **Step 3: Differentiate Ollama** in `ai-connection-card.component.ts` — right after the `conn-privacy` span add:

```html
      @if (card().id === "ollama") {
        <span class="conn-reason text-muted">
          Your own local model server — separate from the built-in Murmur Brain.
        </span>
      }
```

- [ ] **Step 4: Relocate the GGUF registry pointer** in `ai-role-rows.component.ts`: replace the conditional `<app-local-models-list />` render (grep `app-local-models-list` in the file) with:

```html
          <p class="field-help text-muted">
            On-device models are managed under Engines above (Murmur Brain →
            Configure).
          </p>
```

Remove the `LocalModelsListComponent` import and its `imports:` entry. Keep the surrounding `@if` condition unchanged (the hint should appear exactly where the list used to).

- [ ] **Step 5: Ensure `brainModels` loads with the page** — `rg -n "listBrainModels|loadBrainModels" src/app/features/settings/settings.store.ts`. If the fetch happens in `load()` already (the posture pills need it, so it should), do nothing. If it is lazy-triggered by the role rows, move/add the call into `load()`.

- [ ] **Step 6: Verify + commit**

```bash
npx ng lint 2>&1 | tail -3 && npx ng build 2>&1 | tail -3
git add src/app/features/settings/sections/ai/
git commit -m "refactor(settings): Engines block — built-in Murmur Brain card, GGUF registry relocation, Ollama differentiation"
```

---

### Task 6: "On-device intelligence" → "Search index"

**Files:**
- Modify: `src/app/features/settings/sections/ai/on-device-intelligence-block.component.ts`

**Interfaces:** none new — the map card (Task 3) now owns the Embeddings/Name-redaction/Transcription honesty rows, so the badges here are redundant.

- [ ] **Step 1: Reduce the card**
  - Delete the `ondevice-badges` div (the three pills + the `ondevice-note` span) and its now-unused styles.
  - Change the group label `On-device intelligence` → `Search index`.
  - The semantic toggle/download/re-index content stays byte-identical.

- [ ] **Step 2: Verify + commit**

```bash
npx ng lint 2>&1 | tail -3 && npx ng build 2>&1 | tail -3
git add src/app/features/settings/sections/ai/on-device-intelligence-block.component.ts
git commit -m "refactor(settings): On-device intelligence card reduced to Search index (map owns the badges)"
```

---

### Task 7: Role rows — grouped engine options

**Files:**
- Modify: `src/app/features/settings/sections/ai/ai-role-rows.component.ts`

**Interfaces:** the `<select>` option VALUES are contractual (backend connection ids) — only grouping/labels change, never values.

- [ ] **Step 1: Group the per-role connection dropdown** — replace the flat option list with:

```html
              <select
                [formControlName]="row.connCtrl"
                (change)="onConnectionChange(row.role, $event)"
              >
                <option value="">Inherit default</option>
                @if (row.offersReasonerTargets) {
                  <optgroup label="Built-in (on this Mac)">
                    <option value="local">Murmur Brain — on-device</option>
                    <option value="off">Off — retrieval only</option>
                  </optgroup>
                }
                <optgroup label="Your engines">
                  <option value="claude_code">Claude Code</option>
                  <option value="anthropic">Anthropic API</option>
                  <option value="ollama">Ollama</option>
                  <option value="gateway">Kong AI Gateway (OpenAI-compatible)</option>
                </optgroup>
              </select>
```

(The Notes row still gets NO reasoner targets — the existing comment explains `provider_for` refuses them; keep that comment.)

- [ ] **Step 2: Sweep the old label** — `rg -n "Local model — on-device" src/app` and update any other user-visible instance (e.g. the Ask/Live inherit summary in `settings.store.ts` says `Follows the assistant fallback: Local model — on-device` → `Follows the assistant fallback: Murmur Brain — on-device`).

- [ ] **Step 3: Verify + commit**

```bash
npx ng lint 2>&1 | tail -3 && npx ng build 2>&1 | tail -3
git add src/app/features/settings/sections/ai/ai-role-rows.component.ts src/app/features/settings/settings.store.ts
git commit -m "refactor(settings): role rows — grouped engine options (built-in vs your engines)"
```

---

### Task 8: Gates, adversarial verification, PR

**Files:** none created — verification + PR only.

- [ ] **Step 1: Full local gates**

```bash
( cd src-tauri && source ~/.cargo/env && cargo test --lib 2>&1 | tail -3 )
npx ng lint 2>&1 | tail -3
npx ng build 2>&1 | tail -5
```

Expected: all green.

- [ ] **Step 2: Dispatch the adversarial-verifier** (subagent — the implementer does NOT own the verdict) with this brief:
  - Live-reproduce at `http://localhost:1420` (Playwright, mocked `window.__TAURI_INTERNALS__.invoke`); the mock MUST answer `resolved_ai_map` (7 rows per the `AiMapRow` shape) alongside the existing settings mocks, or the map card renders only its loading state and the run is a false-FAIL.
  - Assert: (1) the map card renders all 7 rows with correct on-device/cloud pills; (2) an inactive row (reactions with `brain_live=false` in the mocked config) renders dimmed with "— off"; (3) "Change" expands the Advanced disclosure; (4) posture switch triggers a `resolved_ai_map` re-fetch (mock call count); (5) the GGUF registry (`app-local-models-list`) renders under Engines → Murmur Brain → Configure and is GONE from the role rows; (6) role-row optgroup values unchanged (`local`/`off`/the four provider ids); (7) hunt NG0600 in the console across the whole flow; (8) no leftover "Default AI"/"Providers" strings on the page.
  - Rust: confirm `settings::ai_map` tests fail if `role_row`'s cloud classification is inverted (mutation spot-check — RED-before-GREEN evidence).
- [ ] **Step 3: Run the full CI gate ONCE** (background, it's long):

```bash
bash scripts/ci.sh
```

Expected: green (clippy `-D warnings` + tests + lint + build + headless E2E).

- [ ] **Step 4: PR to trunk** (never direct push; QueaT author, no trailers):

```bash
git push -u origin feat/ai-settings-reia
gh pr create -R murmur-io/murmur --base murmur --title "refactor(settings): AI & Models re-IA — What-runs-where map + Engines/Routing split" --body "<summary of blocks + note: resolved_ai_map returns config metadata only, no content reads — not a lock-gated path. Verifier verdict attached.>"
```

Merge only after the adversarial-verifier verdict is PASS.

---

## Self-Review Notes

- **Spec coverage:** analysis promised (a) resolved map ✅ T1–T3, (b) Engines/Routing axis split ✅ T4–T5, (c) two-local-stacks disambiguation ✅ T5, (d) copy honesty ✅ T4, (e) Search-index reduction ✅ T6, (f) grouped role options ✅ T7. Moving model/effort INTO the claude/anthropic cards was consciously dropped (shared `provider_model` control would mislabel across cards; needs per-connection keys → follow-up, noted in Out of scope).
- **Type consistency:** `AiMapRow` fields snake_case in Rust / camelCase over serde / camelCase in TS — matched everywhere (`onDevice` etc.). `expandAdvanced()`/`advancedExpanded` used identically in T2/T3. `resolved_ai_map` string identical in command, handler registration, IPC, and the verifier mock.
- **Known verify-before-use points (line numbers drift, symbols don't):** `state.config.lock()` pattern copied from `brain_posture`; config field names `ollama_model`/`gateway_model`/`anthropic_model`/`model_size`/`provider_id`/`semantic_search_enabled`/`embed_model_id` — each confirmed present in `config.rs`/`roles.rs` during planning, re-grep before editing.
