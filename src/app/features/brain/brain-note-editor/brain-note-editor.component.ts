import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  inject,
  input,
  output,
  signal,
  viewChild,
} from "@angular/core";

/**
 * The "+ Add note" editor — a name field + a `<textarea>` that ingests typed
 * text into the brain as a `kind='note'` document (via the parent's
 * `importText`). Presented as an OPAQUE modal (T3): a full-viewport dim backdrop
 * + an opaque `var(--surface-overlay)` panel — NOT the frosted `.card`, which
 * would bleed the sources list through (a broken-looking modal).
 *
 * Pure/presentational: it owns the draft (name + body) and emits {@link save}
 * `{ name, text }` on submit + {@link dismiss} on close. The parent owns the
 * IPC call, the in-flight `saving` flag (input), the toast, and the close (it
 * flips the open flag on a successful save). Escape / backdrop click / Cancel
 * all emit `dismiss`.
 */
@Component({
  selector: "app-brain-note-editor",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./brain-note-editor.component.html",
  styleUrl: "./brain-note-editor.component.scss",
})
export class BrainNoteEditorComponent {
  private readonly injector = inject(Injector);

  /** True while the parent's importText is in flight (locks the buttons). */
  readonly saving = input(false);

  readonly save = output<{ name: string; text: string }>();
  readonly dismiss = output<void>();

  protected readonly name = signal("");
  protected readonly text = signal("");

  private readonly nameInput =
    viewChild<ElementRef<HTMLInputElement>>("nameInput");

  /** Only the body is required (a blank title falls back to "note" server-side). */
  protected readonly canSave = computed(() => this.text().trim().length > 0);

  constructor() {
    // Focus the title field once the modal has rendered (afterNextRender, never
    // setTimeout). This runs in the field-init injection context, so the
    // explicit injector is belt-and-braces consistent with the rest of the tree.
    afterNextRender(() => this.nameInput()?.nativeElement.focus(), {
      injector: this.injector,
    });
  }

  protected asValue(event: Event): string {
    return (event.target as HTMLInputElement | HTMLTextAreaElement).value;
  }

  protected submit(): void {
    if (this.saving() || !this.canSave()) {
      return;
    }
    this.save.emit({ name: this.name().trim(), text: this.text().trim() });
  }
}
