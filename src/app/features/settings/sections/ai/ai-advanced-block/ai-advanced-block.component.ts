import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../../../settings.store";
import { AiConnectionCardsComponent } from "../ai-connection-cards/ai-connection-cards.component";
import { AiRoleRowsComponent } from "../ai-role-rows/ai-role-rows.component";

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
  templateUrl: "./ai-advanced-block.component.html",
  styleUrl: "./ai-advanced-block.component.scss",
})
export class AiAdvancedBlockComponent {
  private readonly store = inject(SettingsStore);

  readonly form = this.store.form;

  /** Whether the Advanced disclosure region is open. Collapsed by default. */
  readonly expanded = signal(false);

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
        this.expanded.set(true);
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
    this.expanded.update((v) => !v);
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
