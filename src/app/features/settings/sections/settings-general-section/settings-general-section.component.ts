import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../../settings.store";

/**
 * Settings → general section (Stage-1 split): the `@case ("general")` block of the
 * former settings.component.ts monolith, moved VERBATIM. State/actions live in
 * the shell-provided SettingsStore so section switches never drop them.
 */
@Component({
  selector: "app-settings-general-section",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
  templateUrl: "./settings-general-section.component.html",
  styleUrl: "./settings-general-section.component.scss",
})
export class SettingsGeneralSectionComponent {
  private readonly store = inject(SettingsStore);

  /** The shared settings form — single source of truth, owned by the store. */
  readonly form = this.store.form;

  pickVault(): void {
    void this.store.pickVault();
  }

  pickModel(): void {
    void this.store.pickModel();
  }

  rerunOnboarding(): void {
    this.store.rerunOnboarding();
  }
}
