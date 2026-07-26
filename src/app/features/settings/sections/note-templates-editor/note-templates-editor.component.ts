import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  signal,
} from "@angular/core";
import { SettingsStore } from "../../settings.store";
import type { NoteTemplate } from "../../../../core/models";

/** A draft section carries a client-only `id` so `@for … track s.id` keeps each input's DOM node
 * (and focus) stable across keystrokes, even though every edit replaces the object in the signal. */
interface DraftSection {
  id: number;
  heading: string;
  instruction: string;
}

/**
 * Minimal editor for user-authored NOTE TEMPLATES (Granola-style named sections). Lists the saved
 * templates and hosts a create/edit form (name + tone + ordered sections + optional extra
 * front-matter keys). All persistence goes through {@link SettingsStore} → IpcService; the backend
 * rejects scripting tokens (`<%`, `tp.`, `require(`, `process.`) and surfaces the error in
 * `noteTemplateError`. A saved template becomes selectable in the "Summary style" selector by id.
 */
@Component({
  selector: "app-note-templates-editor",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./note-templates-editor.component.html",
  styleUrl: "./note-templates-editor.component.scss",
})
export class NoteTemplatesEditorComponent {
  private readonly store = inject(SettingsStore);

  readonly templates = this.store.noteTemplates;
  readonly busy = this.store.noteTemplateBusy;
  readonly error = this.store.noteTemplateError;

  /** True while the create/edit form is open (else the list is shown). */
  readonly editing = signal(false);
  /** The id being edited, or null when creating a new template. */
  private readonly editingId = signal<string | null>(null);
  private nextSectionId = 0;

  readonly name = signal("");
  readonly tone = signal("");
  readonly sections = signal<DraftSection[]>([]);
  /** Comma-separated extra front-matter keys (parsed on save). */
  readonly extraKeys = signal("");

  /** A template needs a name and at least one non-blank section heading before it can be saved. */
  readonly canSave = computed(
    () =>
      this.name().trim().length > 0 &&
      this.sections().some((s) => s.heading.trim().length > 0),
  );

  private draftSection(heading = "", instruction = ""): DraftSection {
    return { id: this.nextSectionId++, heading, instruction };
  }

  startNew(): void {
    this.editingId.set(null);
    this.name.set("");
    this.tone.set("");
    this.sections.set([this.draftSection("Summary")]);
    this.extraKeys.set("");
    this.editing.set(true);
  }

  edit(t: NoteTemplate): void {
    this.editingId.set(t.id);
    this.name.set(t.name);
    this.tone.set(t.tone);
    this.sections.set(
      t.sections.map((s) => this.draftSection(s.heading, s.instruction)),
    );
    this.extraKeys.set(t.extraFrontmatterKeys.join(", "));
    this.editing.set(true);
  }

  cancel(): void {
    this.editing.set(false);
  }

  addSection(): void {
    this.sections.set([...this.sections(), this.draftSection()]);
  }

  removeSection(id: number): void {
    this.sections.set(this.sections().filter((s) => s.id !== id));
  }

  setHeading(id: number, value: string): void {
    this.sections.set(
      this.sections().map((s) => (s.id === id ? { ...s, heading: value } : s)),
    );
  }

  setInstruction(id: number, value: string): void {
    this.sections.set(
      this.sections().map((s) =>
        s.id === id ? { ...s, instruction: value } : s,
      ),
    );
  }

  async save(): Promise<void> {
    const keys = this.extraKeys()
      .split(",")
      .map((k) => k.trim())
      .filter((k) => k.length > 0);
    const saved = await this.store.saveNoteTemplate({
      id: this.editingId(),
      name: this.name(),
      tone: this.tone(),
      sections: this.sections().map((s) => ({
        heading: s.heading,
        instruction: s.instruction,
      })),
      extraFrontmatterKeys: keys,
    });
    // Only close on success — a rejection (e.g. a scripting token) keeps the draft so the user can fix it.
    if (saved) this.editing.set(false);
  }

  async remove(t: NoteTemplate): Promise<void> {
    await this.store.deleteNoteTemplate(t.id);
  }

  /** Handler helper so the template can read `$event.target.value` without an inline cast. */
  inputValue(event: Event): string {
    return (event.target as HTMLInputElement | HTMLTextAreaElement).value;
  }
}
