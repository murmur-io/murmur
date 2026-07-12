import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../../../settings.store";
import { MurToggleComponent } from "../../../../../design-system/toggle/toggle.component";
import {
  NOTE_ASSIST_CATALOG,
  NOTE_ASSIST_GROUPS,
  type NoteAssistCatalogEntry,
  type NoteAssistGroup,
} from "../../../../notes/note-brain-popover/note-assist-catalog";

/**
 * AI & Models → "Note assistant" block.
 *
 * Owns every in-note selection-assistant action toggle, grouped under the same
 * quiet section labels the editor's command menu uses (EDIT / STRUCTURE / FROM
 * YOUR BRAIN / EXTRACT / CREATE). The three LEGACY actions (Refine · Shorten ·
 * Enhance context) bind to their own AppConfig booleans (`noteAssistRefine` /
 * `-Shorten` / `-Enhance`); every other action binds to an id-named boolean
 * control the store converts to/from the single `noteAssistActionsOff` config
 * list. All default ON. Every toggle rides the shared SettingsStore auto-save —
 * the Notes editor's popover simply hides a disabled action.
 *
 * The clarifying line notes these run on the user's Notes model (Role::Notes), so
 * they follow the same local/cloud connection set for Notes above — surfacing the
 * backend-resolved Notes model label when the "what runs where" map has loaded.
 * All copy is competitor-name-free.
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

  /** Group order for the section headers (same as the command menu). */
  readonly groups = NOTE_ASSIST_GROUPS;

  /** The catalog actions in one group, in catalog order. */
  actionsInGroup(group: NoteAssistGroup): readonly NoteAssistCatalogEntry[] {
    return NOTE_ASSIST_CATALOG.filter((a) => a.group === group);
  }

  /**
   * The form-control name for an action's toggle: the legacy trio map to their
   * dedicated `noteAssist*` bools; every other action's control name IS its id.
   */
  controlName(entry: NoteAssistCatalogEntry): string {
    switch (entry.id) {
      case "refine":
        return "noteAssistRefine";
      case "shorten":
        return "noteAssistShorten";
      case "enhance":
        return "noteAssistEnhance";
      default:
        return entry.id;
    }
  }
}
