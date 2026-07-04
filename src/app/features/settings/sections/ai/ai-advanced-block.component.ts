import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
} from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../../settings.store";
import { AiConnectionCardsComponent } from "./ai-connection-cards.component";
import { AiRoleRowsComponent } from "./ai-role-rows.component";

/**
 * AI & Models → Collapsed "Advanced" disclosure block (Task 4).
 *
 * A plain in-flow toggle button ("⚙ Advanced — connections, models, per-feature")
 * expands a region that shows, in order:
 *   1. `<app-ai-connection-cards />` — the provider Connections block (moved here
 *      from the top-level `settings-ai-section`).
 *   2. Default AI `<select>` + Default model / reasoning-effort (`brain-tuning`)
 *      — moved verbatim from `AiDefaultsBlockComponent`.
 *   3. `<app-ai-role-rows />` — Customize per feature (moved from AiDefaultsBlock).
 *
 * When `posture() === "fully_local"` the Default-AI select is rendered disabled
 * (the on-device pipeline runs notes itself) with a "Not used — Fully local" note.
 *
 * NOT an overlay — the expanded content is IN-FLOW (no `--surface-overlay`, no
 * `backdrop-filter`). See angular-zoneless.md T3.
 */
@Component({
  selector: "app-ai-advanced-block",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule, AiConnectionCardsComponent, AiRoleRowsComponent],
  template: `
    <div class="adv-wrap">
      <button
        type="button"
        class="adv-toggle"
        (click)="toggle()"
        [attr.aria-expanded]="expanded()"
      >
        <svg
          class="adv-chevron"
          [class.is-open]="expanded()"
          width="12"
          height="12"
          viewBox="0 0 12 12"
          fill="none"
          aria-hidden="true"
        >
          <path
            d="M4 2l4 4-4 4"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            stroke-linejoin="round"
          />
        </svg>
        ⚙ Advanced — connections, models, per-feature
      </button>

      @if (expanded()) {
        <div class="adv-body">
          <!-- Block 1: Provider Connections (moved from settings-ai-section top) -->
          <app-ai-connection-cards />

          <!-- Block 2: Default AI + Default model / reasoning effort -->
          <div class="adv-defaults card" [formGroup]="form">
            <label class="field">
              <span class="field-label">Default AI</span>
              <!--
                Disabled when posture is "fully_local" — the on-device pipeline
                runs notes on the selected GGUF, so this picker has no effect.
                Reactive-forms controls can't be disabled via [attr.disabled]
                (the ControlValueAccessor strips it), so _syncProviderDisable
                toggles the FormControl's own .disable()/.enable() — see its
                comment below. save() uses getRawValue(), so a disabled
                providerId still persists.
              -->
              <select
                formControlName="providerId"
                (change)="onDefaultAiChanged($event)"
              >
                <option value="claude_code">Claude Code (default)</option>
                <option value="anthropic">Anthropic API</option>
                <option value="ollama">Ollama</option>
                <option value="gateway">Kong AI Gateway (OpenAI-compatible)</option>
              </select>
              @if (fullyLocal()) {
                <span class="field-help text-muted">
                  Not used — Fully local runs notes on-device.
                </span>
              } @else {
                <span class="field-help text-muted">
                  Used for everything Murmur writes: notes, answers, digests,
                  briefs. Set the connection up in the Providers block above.
                </span>
              }
            </label>

            <!--
              Model + reasoning-effort overrides. providerModel steers ONLY the
              claude_code/anthropic arms (gateway/ollama read gateway_model /
              ollama_model instead), so the dropdown renders only for those two —
              for gateway/ollama we point at the connection card that actually
              holds the model. The old "Anthropic model" free-text is intentionally
              UNRENDERED (its FormControl still round-trips in the store).
            -->
            <div class="brain-tuning">
              @switch (form.controls.providerId.value) {
                @case ("gateway") {
                  <p class="brain-note text-muted">
                    The model for Kong AI Gateway is set in its connection card above.
                  </p>
                }
                @case ("ollama") {
                  <p class="brain-note text-muted">
                    The model for Ollama is set in its connection card above.
                  </p>
                }
                @default {
                  <!-- div.field (not label) — the control sits in a nested row div,
                       same as the gateway card's Model field. -->
                  <div class="field">
                    <span class="field-label">Default model</span>
                    <!--
                      Options come from list_models (the backend Claude-id constant —
                      single source of truth, no hardcoded ids here). Empty catalog
                      (fetch failed / older backend) → free-text fallback; a saved
                      model missing from the catalog stays selectable as "(custom)"
                      — the gateway picker's keep-manually-typed pattern.
                    -->
                    <div class="default-model-row">
                      @if (defaultModelCatalog().length > 0) {
                        <select
                          formControlName="providerModel"
                          class="default-model-select"
                        >
                          <option value="">Default (provider's pick)</option>
                          @for (id of defaultModelCatalog(); track id) {
                            <option [value]="id">{{ id }}</option>
                          }
                          @if (defaultModelIsCustom()) {
                            <option [value]="form.controls.providerModel.value">
                              {{ form.controls.providerModel.value }} (custom)
                            </option>
                          }
                        </select>
                      } @else {
                        <input
                          formControlName="providerModel"
                          placeholder="Model id (blank = provider's pick)"
                          autocomplete="off"
                          spellcheck="false"
                          class="default-model-input"
                        />
                      }
                      <button
                        type="button"
                        class="btn btn-ghost default-model-refresh"
                        (click)="refreshDefaultModels()"
                        [disabled]="defaultModelsLoading()"
                        title="Fetch this provider's model list"
                      >
                        @if (defaultModelsLoading()) {
                          Loading…
                        } @else {
                          ↻ Refresh
                        }
                      </button>
                    </div>
                    <span class="field-help text-muted">
                      Used for everything Murmur writes with AI: meeting notes,
                      answers, digests and briefs. Default lets the provider choose.
                    </span>
                  </div>
                }
              }

              @if (form.controls.providerId.value === "anthropic") {
                <label class="field">
                  <span class="field-label">Reasoning effort</span>
                  <select formControlName="providerEffort">
                    <option value="">Default</option>
                    <option value="low">Low</option>
                    <option value="medium">Medium</option>
                    <option value="high">High</option>
                  </select>
                  <span class="field-help text-muted">
                    Applies to the Anthropic provider — higher effort spends more
                    thinking on harder questions.
                  </span>
                </label>
              }
            </div>
          </div>

          <!-- Block 3: Customize per feature (Ask / Notes / Live override rows) -->
          <app-ai-role-rows />
        </div>
      }
    </div>
  `,
  styles: [
    `
      :host {
        display: contents;
      }

      /* ── Disclosure container — in-flow, not an overlay (T3) ── */
      .adv-wrap {
        display: flex;
        flex-direction: column;
      }

      /* ── Toggle button ── */
      .adv-toggle {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        width: 100%;
        text-align: left;
        padding: var(--space-3) var(--space-4);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
        border-radius: var(--radius-md);
        color: var(--text-secondary);
        font-size: 0.875rem;
        font-weight: 550;
        cursor: pointer;
        transition:
          border-color var(--transition),
          background var(--transition);
      }
      .adv-toggle:hover {
        border-color: var(--border-strong);
        color: var(--text-primary);
      }
      .adv-toggle:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .adv-toggle[aria-expanded="true"] {
        border-bottom-left-radius: 0;
        border-bottom-right-radius: 0;
        border-bottom-color: transparent;
      }

      .adv-chevron {
        flex: none;
        transition: transform var(--transition);
      }
      .adv-chevron.is-open {
        transform: rotate(90deg);
      }

      /* ── Expanded body — stacks sub-sections vertically ── */
      .adv-body {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
        padding: var(--space-4);
        border: 1px solid var(--border-subtle);
        border-top: none;
        border-bottom-left-radius: var(--radius-md);
        border-bottom-right-radius: var(--radius-md);
        background: var(--surface-input);
      }

      /* ── Default AI + model block ── */
      .adv-defaults {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }

      .brain-tuning {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .brain-note {
        margin: 0;
        font-size: 0.8125rem;
        line-height: 1.5;
      }

      /* Default-model picker — select-or-input + the catalog refresh. */
      .default-model-row {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        flex-wrap: wrap;
      }
      .default-model-select,
      .default-model-input {
        flex: 1 1 220px;
        min-width: 0;
      }
      .default-model-refresh {
        flex: none;
        white-space: nowrap;
      }

      /* ── Stacked label + control ── */
      .field {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .field-label {
        color: var(--text-secondary);
        font-size: 0.9rem;
        font-weight: 550;
      }
      .field-help {
        font-size: 0.8125rem;
        line-height: 1.5;
      }
    `,
  ],
})
export class AiAdvancedBlockComponent {
  private readonly store = inject(SettingsStore);

  readonly form = this.store.form;

  /** Whether the Advanced disclosure is open — store-owned so the map's "Change" opens it. */
  readonly expanded = this.store.advancedExpanded;

  /** True when the committed posture is "fully_local" — disables the Default-AI select. */
  readonly fullyLocal = computed(() => this.store.posture() === "fully_local");

  /**
   * Auto-open once when any per-feature role override is active, or the legacy
   * `brainBackend=local` fallback is in effect. Mirrors AiRoleRowsComponent._autoExpand
   * so that an active override is never hidden behind the collapsed disclosure.
   * A manual collapse by the user is NOT re-overridden (the effect only sets true).
   */
  private readonly _autoExpand = effect(
    () => {
      if (
        this.store.roleNotesConnValue() ||
        this.store.roleAskConnValue() ||
        this.store.roleLiveConnValue() ||
        this.store.brainBackendValue() === "local"
      ) {
        this.store.advancedExpanded.set(true);
      }
    },
    { allowSignalWrites: true },
  );

  readonly defaultModelCatalog = this.store.defaultModelCatalog;
  readonly defaultModelsLoading = this.store.defaultModelsLoading;
  readonly defaultModelIsCustom = this.store.defaultModelIsCustom;

  /**
   * Keep the native `<select disabled>` state in sync with the posture signal.
   * Angular reactive forms owns the disabled property — we must call
   * `.disable()` / `.enable()` on the FormControl itself, not `[attr.disabled]`
   * (which Angular's control-value-accessor removes). `{ emitEvent: false }`
   * avoids a spurious `valueChanges` emission. No signal write → no NG0600.
   * `getRawValue()` in the store still includes disabled controls for save. ✓
   */
  private readonly _syncProviderDisable = effect(() => {
    if (this.fullyLocal()) {
      this.form.controls.providerId.disable({ emitEvent: false });
    } else {
      this.form.controls.providerId.enable({ emitEvent: false });
    }
  });

  toggle(): void {
    this.store.advancedExpanded.update((v) => !v);
  }

  /** Prefetch the newly-picked Default AI's model catalog (claude_code/anthropic only). */
  onDefaultAiChanged(e: Event): void {
    const id = (e.target as HTMLSelectElement).value;
    if (id === "claude_code" || id === "anthropic") {
      void this.store.ensureModels(id);
    }
  }

  /** Re-fetch the Default-model catalog for the current provider. */
  refreshDefaultModels(): void {
    void this.store.refreshModels(this.form.controls.providerId.value);
  }
}
