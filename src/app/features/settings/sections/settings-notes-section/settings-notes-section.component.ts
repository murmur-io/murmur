import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { MurToggleComponent } from "../../../../design-system/toggle/toggle.component";
import { SettingsStore } from "../../settings.store";

/**
 * Settings → notes section (Stage-1 split): the `@case ("notes")` block of the
 * former settings.component.ts monolith, moved VERBATIM. State/actions live in
 * the shell-provided SettingsStore so section switches never drop them.
 */
@Component({
  selector: "app-settings-notes-section",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    MurToggleComponent,ReactiveFormsModule],
  templateUrl: "./settings-notes-section.component.html",
  styleUrl: "./settings-notes-section.component.scss",
})
export class SettingsNotesSectionComponent {
  private readonly store = inject(SettingsStore);

  readonly form = this.store.form;
}
