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
import { FoldersService } from "../../services/folders.service";
import type { FolderNode } from "../../core/models";
import { FolderRowComponent } from "./folder-row.component";

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
  imports: [FolderRowComponent],
  template: `
    <div class="tree" [class.is-root]="isRoot()">
      @if (isRoot()) {
        <!-- "Vault root" — selecting it filters to notes outside any folder. -->
        <button
          type="button"
          class="root-row"
          [class.is-selected]="selectedId() === null"
          [attr.aria-pressed]="selectedId() === null"
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
        />
      } @empty {
        @if (isRoot()) {
          <p class="tree-empty">No folders yet.</p>
        }
      }

      @if (isRoot()) {
        <!-- Inline "New folder" create (afterNextRender focus; not setTimeout) -->
        @if (creating()) {
          <form class="new-folder" (submit)="confirmCreate($event)">
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
              class="btn btn-ghost"
              [disabled]="saving()"
              (click)="cancelCreate()"
            >
              Cancel
            </button>
          </form>
          @if (createError()) {
            <p class="tree-error" role="alert">{{ createError() }}</p>
          }
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

      .new-folder {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        margin-top: var(--space-2);
      }
      .new-folder-input {
        flex: 1 1 auto;
        min-width: 0;
        height: 36px;
      }
      .new-folder-add {
        height: 36px;
        flex: none;
      }
      .new-folder .btn-ghost {
        height: 36px;
        flex: none;
      }

      .tree-error {
        margin: var(--space-1) 0 0;
        color: var(--danger);
        font-size: 0.78rem;
      }
    `,
  ],
})
export class FolderTreeComponent {
  private readonly folders = inject(FoldersService);
  private readonly injector = inject(Injector);

  /** Nodes to render at this level. */
  readonly nodes = input.required<FolderNode[]>();
  /** Currently-selected folder id (null = vault root). */
  readonly selectedId = input<string | null>(null);
  /** Indent depth (0 at the roots; the inline create only shows at the root). */
  readonly depth = input<number>(0);

  /** Bubbles the chosen folder id (or null for the vault root) to the screen. */
  readonly select = output<string | null>();

  /** Whether THIS level is the tree root (owns "All notes" + create UI). */
  readonly isRoot = computed(() => this.depth() === 0);

  // --- Inline "new folder" create -----------------------------------------
  /** True when the create field is open. */
  readonly creating = signal(false);
  /** Draft folder name bound to the field. */
  readonly draftName = signal("");
  /** True while the create IPC is in flight. */
  readonly saving = signal(false);
  /** Create error (cleared when re-opening / re-submitting). */
  readonly createError = signal<string | null>(null);

  /** The name field — focused after it renders (afterNextRender; no setTimeout). */
  private readonly nameInput =
    viewChild<ElementRef<HTMLInputElement>>("nameInput");

  /** Open the inline create field and move focus into it once it has rendered. */
  openCreate(): void {
    this.createError.set(null);
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
    this.createError.set(null);
  }

  onNameInput(event: Event): void {
    this.draftName.set((event.target as HTMLInputElement).value);
  }

  /**
   * Submit the new folder at the vault root. Awaits the service (which reloads
   * the tree), then closes the field. On failure keeps the field open with an
   * inline error so the user can retry.
   */
  async confirmCreate(event: Event): Promise<void> {
    event.preventDefault();
    const name = this.draftName().trim();
    if (!name || this.saving()) {
      return;
    }
    this.saving.set(true);
    this.createError.set(null);
    try {
      await this.folders.create(name, null);
      this.creating.set(false);
      this.draftName.set("");
    } catch {
      this.createError.set("Couldn’t create that folder. Try another name.");
    } finally {
      this.saving.set(false);
    }
  }
}
