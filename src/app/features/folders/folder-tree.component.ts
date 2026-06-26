import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  forwardRef,
  inject,
  input,
  output,
  signal,
  viewChild,
} from "@angular/core";
import { FoldersService } from "../../services/folders.service";
import { ToastService } from "../../services/toast.service";
import type { FolderNode } from "../../core/models";
import { FolderRowComponent } from "./folder-row.component";
import { FolderDropDirective } from "./folder-drop.directive";

/**
 * The recursive folder tree. Renders `nodes` as {@link FolderRowComponent}s; the
 * recursion is by child component — a row renders its children through ANOTHER
 * `app-folder-tree`, never a nested `@for` of rows. This component is reused at
 * every depth (the row passes `depth + 1`).
 *
 * At the ROOT (depth 0) it also owns:
 *  - a "Vault root" pseudo-row so notes can be selected/moved to no folder, and
 *  - an inline "New folder" create affordance (text field + confirm). Focus is
 *    moved into the field with `afterNextRender` (no `setTimeout`); creation
 *    delegates to {@link FoldersService.create}, which reloads the tree.
 *
 * Selection bubbles up via the `select` output (folder id, or null for root).
 */
@Component({
  selector: "app-folder-tree",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  // `folder-tree` ↔ `folder-row` are mutually recursive standalone components,
  // so their ES modules form an import cycle. Referencing `FolderRowComponent`
  // directly here is `undefined` at metadata-evaluation time and Angular then
  // throws `getComponentDef(undefined)` (reading 'ɵcmp') the moment the `@for`
  // below instantiates an `app-folder-row` — exactly the "view breaks after
  // adding the first folder" bug. `forwardRef` defers the lookup, breaking the
  // cycle. (See folder-row for the mirror.)
  imports: [FolderDropDirective, forwardRef(() => FolderRowComponent)],
  template: `
    <div class="tree" [class.is-root]="isRoot()">
      @if (isRoot()) {
        <!-- "Vault root" — selecting it filters to notes outside any folder,
             and it's a DROP TARGET so a note can be dragged out to the root. -->
        <button
          type="button"
          class="root-row"
          [class.is-selected]="selectedId() === null"
          [attr.aria-pressed]="selectedId() === null"
          appFolderDrop
          [dropFolderId]="null"
          (dropNote)="dropNote.emit({ meetingId: $event, folderId: null })"
          (click)="select.emit(null)"
        >
          <span class="root-icon" aria-hidden="true">
            <svg viewBox="0 0 16 16" width="15" height="15" fill="none">
              <path
                d="M2 13V5.5C2 4.7 2.7 4 3.5 4h3l1.5 1.7H12.5c.8 0 1.5.7 1.5 1.5V13"
                stroke="currentColor"
                stroke-width="1.3"
                stroke-linejoin="round"
              />
            </svg>
          </span>
          <span class="root-name">All notes</span>
        </button>
      }

      @for (node of nodes(); track node.id) {
        <app-folder-row
          [node]="node"
          [selectedId]="selectedId()"
          [depth]="depth()"
          (select)="select.emit($event)"
          (dropNote)="dropNote.emit($event)"
        />
      } @empty {
        @if (isRoot()) {
          <p class="tree-empty">No folders yet.</p>
        }
      }

      @if (isRoot()) {
        <!-- Inline "New folder" create (afterNextRender focus; not setTimeout) -->
        @if (creating()) {
          <form
            class="new-folder"
            [class.is-saving]="saving()"
            (submit)="confirmCreate($event)"
          >
            <span class="new-folder-icon" aria-hidden="true">
              @if (saving()) {
                <span class="new-folder-spinner"></span>
              } @else {
                <svg viewBox="0 0 16 16" width="14" height="14" fill="none">
                  <path
                    d="M1.75 4.25c0-.7.55-1.25 1.25-1.25h2.8c.4 0 .77.18 1 .5l.6.75h4.6c.7 0 1.25.55 1.25 1.25v5.5c0 .7-.55 1.25-1.25 1.25H3c-.7 0-1.25-.55-1.25-1.25z"
                    stroke="currentColor"
                    stroke-width="1.3"
                    stroke-linejoin="round"
                  />
                </svg>
              }
            </span>
            <input
              #nameInput
              type="text"
              class="new-folder-input"
              placeholder="Folder name…"
              autocomplete="off"
              spellcheck="false"
              aria-label="New folder name"
              [value]="draftName()"
              [disabled]="saving()"
              (input)="onNameInput($event)"
              (keydown.escape)="cancelCreate()"
            />
            <button
              type="submit"
              class="btn btn-primary new-folder-add"
              [disabled]="saving() || !draftName().trim()"
            >
              {{ saving() ? "Adding…" : "Add" }}
            </button>
            <button
              type="button"
              class="new-folder-cancel"
              aria-label="Cancel new folder"
              [disabled]="saving()"
              (click)="cancelCreate()"
            >
              <svg
                viewBox="0 0 16 16"
                width="13"
                height="13"
                aria-hidden="true"
              >
                <path
                  d="M4 4l8 8M12 4l-8 8"
                  stroke="currentColor"
                  stroke-width="1.6"
                  stroke-linecap="round"
                />
              </svg>
            </button>
          </form>
          <p class="new-folder-hint" aria-hidden="true">
            Enter to create · Esc to cancel
          </p>
        } @else {
          <button
            type="button"
            class="new-folder-trigger"
            (click)="openCreate()"
          >
            <svg
              viewBox="0 0 16 16"
              width="14"
              height="14"
              fill="none"
              aria-hidden="true"
            >
              <path
                d="M8 3.5v9M3.5 8h9"
                stroke="currentColor"
                stroke-width="1.5"
                stroke-linecap="round"
              />
            </svg>
            New folder
          </button>
        }
      }
    </div>
  `,
  styles: [
    `
      :host {
        display: block;
      }
      .tree {
        display: flex;
        flex-direction: column;
        gap: 2px;
      }

      .root-row {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        padding: var(--space-2) var(--space-2) var(--space-2) var(--space-3);
        border: 1px solid transparent;
        border-radius: var(--radius-md);
        background: transparent;
        color: var(--text-secondary);
        font-family: inherit;
        font-size: 0.9rem;
        font-weight: 550;
        letter-spacing: -0.01em;
        text-align: left;
        cursor: pointer;
        transition:
          color var(--transition),
          background var(--transition);
      }
      .root-row:hover {
        color: var(--text-primary);
        background: var(--surface-hover);
      }
      .root-row:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .root-row.is-selected {
        color: var(--accent-hover);
        background: var(--accent-soft);
      }
      /* Drop target — armed (faint) while any note drags; lit under the pointer. */
      .root-row {
        border: 1px solid transparent;
        transition:
          color var(--transition),
          background var(--transition),
          border-color var(--transition),
          box-shadow var(--transition);
      }
      .root-row.is-drop-armed {
        border-color: var(--border-strong);
        border-style: dashed;
      }
      .root-row.is-drop-target {
        border-style: solid;
        border-color: var(--accent);
        background: var(--accent-soft);
        color: var(--accent-hover);
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .root-icon {
        display: inline-flex;
        color: var(--text-muted);
      }
      .root-row.is-selected .root-icon {
        color: var(--accent-hover);
      }

      .tree-empty {
        margin: var(--space-1) 0 0 var(--space-3);
        color: var(--text-muted);
        font-size: 0.8125rem;
      }

      /* --- Inline create --- */
      .new-folder-trigger {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        align-self: flex-start;
        margin-top: var(--space-2);
        padding: var(--space-2) var(--space-3);
        border: 1px dashed var(--border-strong);
        border-radius: var(--radius-md);
        background: transparent;
        color: var(--text-secondary);
        font-family: inherit;
        font-size: 0.85rem;
        font-weight: 550;
        cursor: pointer;
        transition:
          color var(--transition),
          border-color var(--transition),
          background var(--transition);
      }
      .new-folder-trigger:hover {
        color: var(--text-primary);
        border-color: var(--accent);
        background: var(--accent-soft);
      }
      .new-folder-trigger:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }

      /* The inline create reads as one frosted field: an icon, the name input,
         a primary "Add" and a quiet ✕ cancel — it slides in under the list. */
      .new-folder {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        margin-top: var(--space-2);
        padding: var(--space-1) var(--space-2);
        border: 1px solid var(--accent);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        box-shadow: 0 0 0 3px var(--accent-ring);
        animation: rise 200ms var(--ease-spring, var(--transition)) both;
        transition:
          border-color var(--transition),
          box-shadow var(--transition),
          opacity var(--transition);
      }
      .new-folder.is-saving {
        opacity: 0.75;
        border-color: var(--border-strong);
        box-shadow: none;
      }
      .new-folder-icon {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        flex: none;
        width: 20px;
        height: 20px;
        color: var(--accent-hover);
      }
      .new-folder-spinner {
        width: 14px;
        height: 14px;
        border-radius: 50%;
        border: 2px solid var(--border-strong);
        border-top-color: var(--accent-hover);
        animation: nf-spin 700ms linear infinite;
      }
      @keyframes nf-spin {
        to {
          transform: rotate(360deg);
        }
      }
      .new-folder-input {
        flex: 1 1 auto;
        min-width: 0;
        height: 32px;
        padding: 0 var(--space-1);
        border: none;
        background: transparent;
        color: var(--text-primary);
        font-family: inherit;
        font-size: 0.9rem;
        letter-spacing: -0.01em;
      }
      .new-folder-input:hover,
      .new-folder-input:focus {
        border: none;
        background: transparent;
        box-shadow: none;
        outline: none;
      }
      .new-folder-input::placeholder {
        color: var(--text-muted);
      }
      .new-folder-add {
        height: 28px;
        flex: none;
        padding: 0 var(--space-3);
        font-size: 0.8rem;
      }
      .new-folder-cancel {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        flex: none;
        width: 28px;
        height: 28px;
        padding: 0;
        border: none;
        border-radius: var(--radius-sm);
        background: transparent;
        color: var(--text-muted);
        cursor: pointer;
        transition:
          color var(--transition),
          background var(--transition);
      }
      .new-folder-cancel:hover:not(:disabled) {
        color: var(--text-primary);
        background: var(--surface-hover);
      }
      .new-folder-cancel:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .new-folder-cancel:disabled {
        opacity: 0.4;
        cursor: not-allowed;
      }
      .new-folder-hint {
        margin: var(--space-1) 0 0 var(--space-2);
        color: var(--text-muted);
        font-size: 0.72rem;
        letter-spacing: 0.01em;
      }
      @media (prefers-reduced-motion: reduce) {
        .new-folder {
          animation: none;
        }
        .new-folder-spinner {
          animation-duration: 1400ms;
        }
      }
    `,
  ],
})
export class FolderTreeComponent {
  private readonly folders = inject(FoldersService);
  private readonly toast = inject(ToastService);
  private readonly injector = inject(Injector);

  /** Nodes to render at this level. */
  readonly nodes = input.required<FolderNode[]>();
  /** Currently-selected folder id (null = vault root). */
  readonly selectedId = input<string | null>(null);
  /** Indent depth (0 at the roots; the inline create only shows at the root). */
  readonly depth = input<number>(0);

  /** Bubbles the chosen folder id (or null for the vault root) to the screen. */
  readonly select = output<string | null>();

  /** Bubbles a note dropped onto a folder (or the root) up to the screen. */
  readonly dropNote = output<{ meetingId: string; folderId: string | null }>();

  /** Whether THIS level is the tree root (owns "All notes" + create UI). */
  readonly isRoot = computed(() => this.depth() === 0);

  // --- Inline "new folder" create -----------------------------------------
  /** True when the create field is open. */
  readonly creating = signal(false);
  /** Draft folder name bound to the field. */
  readonly draftName = signal("");
  /** True while the create IPC is in flight. */
  readonly saving = signal(false);

  /** The name field — focused after it renders (afterNextRender; no setTimeout). */
  private readonly nameInput =
    viewChild<ElementRef<HTMLInputElement>>("nameInput");

  /** Open the inline create field and move focus into it once it has rendered. */
  openCreate(): void {
    this.draftName.set("");
    this.creating.set(true);
    afterNextRender(() => this.nameInput()?.nativeElement.focus(), {
      injector: this.injector,
    });
  }

  /** Close the create field without creating (ignored mid-save). */
  cancelCreate(): void {
    if (this.saving()) {
      return;
    }
    this.creating.set(false);
    this.draftName.set("");
  }

  onNameInput(event: Event): void {
    this.draftName.set((event.target as HTMLInputElement).value);
  }

  /**
   * Submit the new folder at the vault root. Awaits the service (which reloads
   * the tree), then closes the field and SELECTS the freshly-created folder so
   * it's highlighted and its (empty) note list is shown — a clear success
   * signal. On failure we keep the field open (so the typed name isn't lost)
   * and surface a single danger toast; no inline error noise.
   */
  async confirmCreate(event: Event): Promise<void> {
    event.preventDefault();
    const name = this.draftName().trim();
    if (!name || this.saving()) {
      return;
    }
    this.saving.set(true);
    try {
      const folder = await this.folders.create(name, null);
      this.creating.set(false);
      this.draftName.set("");
      // Surface the new folder: select + highlight it (its row now exists in the
      // reloaded tree). The parent screen filters the meeting list to it.
      this.select.emit(folder.id);
    } catch {
      this.toast.danger("Couldn’t create that folder. Try another name.");
    } finally {
      this.saving.set(false);
    }
  }
}
