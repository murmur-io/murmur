import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
} from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import { ReactiveFormsModule } from "@angular/forms";
import { startWith } from "rxjs";
import { MurToggleComponent } from "../../../../design-system/toggle/toggle.component";
import { SettingsStore } from "../../settings.store";
import { NoteTemplatesEditorComponent } from "../note-templates-editor/note-templates-editor.component";

/** The four fixed built-in summary styles (everything else is a saved template id). */
const BUILTIN_STYLES = ["standard", "brief", "detailed", "action"];

/**
 * Settings → notes section (Stage-1 split): the `@case ("notes")` block of the
 * former settings.component.ts monolith. State/actions live in the shell-provided
 * SettingsStore so section switches never drop them. Extended with the
 * user-authored note-template layer (saved templates list in the style selector +
 * the {@link NoteTemplatesEditorComponent}).
 */
@Component({
  selector: "app-settings-notes-section",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurToggleComponent, ReactiveFormsModule, NoteTemplatesEditorComponent],
  templateUrl: "./settings-notes-section.component.html",
  styleUrl: "./settings-notes-section.component.scss",
})
export class SettingsNotesSectionComponent {
  private readonly store = inject(SettingsStore);

  readonly form = this.store.form;
  readonly noteTemplates = this.store.noteTemplates;

  /** Live mirror of the noteStyle control so the computed below is reactive under zoneless. */
  private readonly _noteStyleValue = toSignal(
    this.form.controls.noteStyle.valueChanges.pipe(
      startWith(this.form.controls.noteStyle.value),
    ),
    { initialValue: this.form.controls.noteStyle.value },
  );

  /**
   * Whether to render the stored value as its own "(custom)" option — only when it matches
   * NEITHER a built-in style NOR a known saved template (a hand-edited / legacy value). A saved
   * template id renders through the `@for` options instead, so it must not also appear here.
   */
  readonly showCustomOption = computed(() => {
    const v = this._noteStyleValue();
    if (BUILTIN_STYLES.includes(v)) return false;
    return !this.noteTemplates().some((t) => t.id === v);
  });

  /** The saved template the style selector currently points at, or null (a built-in / unknown). */
  readonly selectedTemplate = computed(
    () => this.noteTemplates().find((t) => t.id === this._noteStyleValue()) ?? null,
  );
}
