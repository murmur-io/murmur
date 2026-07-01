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
  template: `
    <div class="ne-backdrop">
      <!-- Full-viewport dismiss target: a real <button> so it's natively
           focusable + keyboard-operable (satisfies a11y lint) — click or Escape
           closes. The panel sits ABOVE it and stops propagation. -->
      <button
        type="button"
        class="ne-scrim"
        aria-label="Close"
        (click)="dismiss.emit()"
        (keydown.escape)="dismiss.emit()"
      ></button>
      <div
        class="ne-panel"
        role="dialog"
        aria-modal="true"
        aria-label="Add a note to the brain"
      >
        <header class="ne-head">
          <h3 class="ne-title">Add a note to the brain</h3>
          <button
            type="button"
            class="ne-close btn btn-ghost"
            aria-label="Close"
            (click)="dismiss.emit()"
          >
            <svg viewBox="0 0 16 16" width="15" height="15" fill="none" aria-hidden="true">
              <path
                d="M4 4l8 8M12 4l-8 8"
                stroke="currentColor"
                stroke-width="1.6"
                stroke-linecap="round"
              />
            </svg>
          </button>
        </header>

        <p class="ne-sub">
          Type or paste anything — it’s indexed alongside your meeting notes so
          the brain can reason over it.
        </p>

        <label class="ne-field">
          <span class="ne-label">Title</span>
          <input
            #nameInput
            class="ne-input"
            type="text"
            placeholder="e.g. Q3 planning notes"
            [value]="name()"
            (input)="name.set(asValue($event))"
          />
        </label>

        <label class="ne-field">
          <span class="ne-label">Note</span>
          <textarea
            class="ne-textarea"
            rows="8"
            placeholder="Write or paste the note text…"
            [value]="text()"
            (input)="text.set(asValue($event))"
          ></textarea>
        </label>

        <footer class="ne-actions">
          <button
            type="button"
            class="btn btn-ghost"
            [disabled]="saving()"
            (click)="dismiss.emit()"
          >
            Cancel
          </button>
          <button
            type="button"
            class="btn btn-primary"
            [disabled]="saving() || !canSave()"
            (click)="submit()"
          >
            {{ saving() ? "Adding…" : "Add to brain" }}
          </button>
        </footer>
      </div>
    </div>
  `,
  styles: [
    `
      :host {
        display: block;
      }
      .ne-backdrop {
        position: fixed;
        inset: 0;
        z-index: 60;
        display: flex;
        align-items: center;
        justify-content: center;
        padding: var(--space-5);
      }
      .ne-scrim {
        position: fixed;
        inset: 0;
        border: none;
        margin: 0;
        padding: 0;
        cursor: default;
        background: rgba(0, 0, 0, 0.55);
        animation: ne-fade 160ms var(--transition) both;
      }
      .ne-panel {
        /* Floating modal OVER the page → OPAQUE (T3), never the frosted .card. */
        position: relative;
        z-index: 1;
        width: min(560px, 100%);
        max-height: 88vh;
        overflow: auto;
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
        padding: var(--space-5);
        background: var(--surface-overlay);
        border: 1px solid var(--border-strong);
        border-radius: var(--radius-lg);
        box-shadow: var(--shadow-lg);
        -webkit-backdrop-filter: none;
        backdrop-filter: none;
        animation: ne-rise 200ms var(--transition) both;
      }
      .ne-head {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-3);
      }
      .ne-title {
        margin: 0;
        font-size: 1.0625rem;
      }
      .ne-close {
        flex: none;
        width: 30px;
        height: 30px;
        padding: 0;
        color: var(--text-muted);
      }
      .ne-close:hover {
        color: var(--text-primary);
      }
      .ne-sub {
        margin: 0;
        color: var(--text-secondary);
        font-size: 0.875rem;
      }
      .ne-field {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .ne-label {
        color: var(--text-muted);
        font-size: 0.75rem;
        font-weight: 600;
        letter-spacing: 0.04em;
        text-transform: uppercase;
      }
      .ne-input {
        width: 100%;
        height: 40px;
      }
      .ne-textarea {
        width: 100%;
        min-height: 160px;
        resize: vertical;
        font-family: inherit;
        line-height: 1.55;
      }
      .ne-actions {
        display: flex;
        justify-content: flex-end;
        gap: var(--space-2);
        margin-top: var(--space-1);
      }

      @keyframes ne-fade {
        from {
          opacity: 0;
        }
      }
      @keyframes ne-rise {
        from {
          opacity: 0;
          transform: translateY(8px) scale(0.98);
        }
      }
      @media (prefers-reduced-motion: reduce) {
        .ne-scrim,
        .ne-panel {
          animation: none;
        }
      }
    `,
  ],
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
