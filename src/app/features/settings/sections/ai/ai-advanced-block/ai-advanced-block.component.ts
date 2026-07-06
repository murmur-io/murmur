import {
  ChangeDetectionStrategy,
  Component,
  effect,
  inject,
} from "@angular/core";
import { SettingsStore } from "../../../settings.store";
import { AiConnectionCardsComponent } from "../ai-connection-cards/ai-connection-cards.component";
import { AiRoleRowsComponent } from "../ai-role-rows/ai-role-rows.component";

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
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [AiConnectionCardsComponent, AiRoleRowsComponent],
  templateUrl: "./ai-advanced-block.component.html",
  styleUrl: "./ai-advanced-block.component.scss",
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
  );

  toggle(): void {
    this.store.advancedExpanded.update((v) => !v);
  }
}
