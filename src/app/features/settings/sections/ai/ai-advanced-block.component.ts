import {
  ChangeDetectionStrategy,
  Component,
  effect,
  inject,
} from "@angular/core";
import { SettingsStore } from "../../settings.store";
import { AiConnectionCardsComponent } from "./ai-connection-cards.component";
import { AiRoleRowsComponent } from "./ai-role-rows.component";

/**
 * AI & Models → Collapsed "Advanced" disclosure block (Task 4, posture redesign).
 *
 * A plain in-flow toggle button expands the power path, in order:
 *   1. `<app-ai-connection-cards />` — every engine's connection card (keys,
 *      base URLs, Test), split On-this-Mac vs Cloud.
 *   2. `<app-ai-role-rows />` — per-feature overrides (Notes / Ask / Live).
 *
 * The Default AI engine + Default model controls used to live here; they moved
 * UP into "Your setup" (`<app-ai-setup-block />`), which owns the same
 * `providerId` / `providerModel` / `providerEffort` FormControls — one source
 * of truth, no duplicate writer. Advanced no longer touches them.
 *
 * NOT an overlay — the expanded content is IN-FLOW (no `--surface-overlay`, no
 * `backdrop-filter`). See angular-zoneless.md T3.
 */
@Component({
  selector: "app-ai-advanced-block",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [AiConnectionCardsComponent, AiRoleRowsComponent],
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
        ⚙ Advanced — per-feature overrides &amp; all engines
      </button>

      @if (expanded()) {
        <div class="adv-body">
          <!-- Block 1: every engine's connection card (keys, URLs, Test). -->
          <app-ai-connection-cards />

          <!-- Block 2: per-feature overrides (Ask / Notes / Live). -->
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
    `,
  ],
})
export class AiAdvancedBlockComponent {
  private readonly store = inject(SettingsStore);

  /** Whether the Advanced disclosure is open — store-owned so the map's "Change" opens it. */
  readonly expanded = this.store.advancedExpanded;

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

  toggle(): void {
    this.store.advancedExpanded.update((v) => !v);
  }
}
