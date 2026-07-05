import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  input,
  output,
} from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { MurToggleComponent } from "../../../../../design-system/toggle/toggle.component";
import type { ProviderStatus } from "../../../../../core/models";
import { SettingsStore } from "../../../settings.store";

/** Render-ready view-model for one connection card (built by the parent). */
export interface ConnectionCardVm {
  readonly id: string;
  readonly name: string;
  readonly status: ProviderStatus | null;
  readonly expanded: boolean;
  readonly cloud: boolean;
}

/**
 * ONE provider connection card: name + status pill (from the availability
 * fan-out), a privacy line, Test, and a Configure DISCLOSURE (in-flow expand,
 * default collapsed — not a floating overlay) holding exactly the controls
 * the old Providers section had for this connection. The gateway disclosure
 * is the ENTIRE old gateway card, now always reachable (no
 * `providerId === 'gateway'` gate). Expand/Test state lives in the parent;
 * the form + key controls come from the shell-provided SettingsStore.
 */
@Component({
  selector: "app-ai-connection-card",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    MurToggleComponent,ReactiveFormsModule],
  templateUrl: "./ai-connection-card.component.html",
  styleUrl: "./ai-connection-card.component.scss",
})
export class AiConnectionCardComponent {
  private readonly store = inject(SettingsStore);

  /** The render-ready card (status + group + disclosure state), parent-built. */
  readonly card = input.required<ConnectionCardVm>();
  /** True while the parent has a Test probe in flight (disables the button). */
  readonly testing = input(false);

  /** Open/close this card's Configure disclosure (state lives in the parent). */
  readonly toggleConfigure = output<void>();
  /** Run the availability probe for this connection (parent orchestrates). */
  readonly probe = output<void>();

  readonly form = this.store.form;
  readonly keyControl = this.store.keyControl;
  readonly gatewayKeyControl = this.store.gatewayKeyControl;
  readonly hasKey = this.store.hasKey;
  readonly gatewayUrlWarning = this.store.gatewayUrlWarning;
  readonly gatewayModels = this.store.gatewayModels;
  readonly gatewayModelsLoading = this.store.gatewayModelsLoading;
  readonly gatewayModelError = this.store.gatewayModelError;
  readonly gatewayModelIsCustom = this.store.gatewayModelIsCustom;
  readonly gatewayHealth = this.store.gatewayHealth;
  readonly gatewayHealthChecking = this.store.gatewayHealthChecking;
  readonly hasGatewayKey = this.store.hasGatewayKey;
  readonly gatewayKeyError = this.store.gatewayKeyError;
  readonly gatewayDestination = this.store.gatewayDestination;

  /** The reason line, shown only for a probed-but-unavailable connection. */
  readonly unavailableReason = computed(() => {
    const c = this.card();
    return c.status && !c.status.available && c.status.reason
      ? c.status.reason
      : null;
  });

  saveKey(): void {
    void this.store.saveKey();
  }

  refreshGatewayModels(): void {
    void this.store.refreshGatewayModels();
  }

  checkGatewayHealth(): void {
    void this.store.checkGatewayHealth();
  }

  saveGatewayKey(): void {
    void this.store.saveGatewayKey();
  }

  removeGatewayKey(): void {
    void this.store.removeGatewayKey();
  }
}
