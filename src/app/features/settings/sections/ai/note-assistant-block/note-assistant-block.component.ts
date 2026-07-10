import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../../../settings.store";
import { MurToggleComponent } from "../../../../../design-system/toggle/toggle.component";

/**
 * AI & Models → "Note assistant" block.
 *
 * Owns the three in-note selection-assistant action toggles (Refine · Shorten ·
 * Enhance context). Each is an independent AppConfig boolean, all default ON,
 * persisted through the shared SettingsStore auto-save (same path as the
 * live-during-meetings toggles) — the Notes editor's popover simply hides a
 * disabled action.
 *
 * The clarifying line notes that these in-note actions run on the user's Notes
 * model (Role::Notes), so they follow the same local/cloud connection set for
 * Notes above — surfacing the backend-resolved Notes model label when the "what
 * runs where" map has loaded. All copy is competitor-name-free.
 */
@Component({
  selector: "app-note-assistant-block",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurToggleComponent, ReactiveFormsModule],
  templateUrl: "./note-assistant-block.component.html",
  styleUrl: "./note-assistant-block.component.scss",
})
export class NoteAssistantBlockComponent {
  private readonly store = inject(SettingsStore);

  readonly form = this.store.form;
  /** The backend-resolved Notes model label (null until the map loads). */
  readonly noteAssistModelLabel = this.store.noteAssistModelLabel;
}
