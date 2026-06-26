import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  input,
  output,
  signal,
} from "@angular/core";
import { FoldersService } from "../../services/folders.service";
import { ToastService } from "../../services/toast.service";
import type { FolderNode } from "../../core/models";
import { LockBadgeComponent } from "./lock-badge.component";

/** A flattened, indented folder option for the picker list. */
interface FolderOption {
  node: FolderNode;
  depth: number;
}

/**
 * "Move to…" popover — a folder picker (NOT drag-and-drop). The host renders the
 * trigger; this popover lists every folder (plus "All notes" / vault root) and,
 * on pick, moves `meetingId` via {@link FoldersService.moveNote}.
 *
 * Load-bearing confirm: a move that crosses an encryption boundary is destructive
 * to the on-disk plaintext, so it MUST be confirmed first:
 *  - INTO a locked folder  → "encrypts + removes the Markdown from your vault".
 *  - OUT OF a locked folder → "re-exports the plaintext Markdown to your vault".
 * A move between two open folders (or open↔root) needs no confirm and applies
 * immediately. The confirm copy names the exact consequence — never a bare
 * "Are you sure?".
 */
@Component({
  selector: "app-move-to-menu",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [LockBadgeComponent],
  template: `
    <div class="menu card" role="menu" aria-label="Move note to folder">
      <header class="menu-head">
        <h4 class="menu-title">Move to…</h4>
        <button
          type="button"
          class="menu-close"
          aria-label="Close move menu"
          (click)="close.emit()"
        >
          <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
            <path
              d="M4 4l8 8M12 4l-8 8"
              stroke="currentColor"
              stroke-width="1.6"
              stroke-linecap="round"
            />
          </svg>
        </button>
      </header>

      @if (pending(); as target) {
        <!-- Load-bearing confirm for a cross-encryption-boundary move -->
        <div class="confirm" role="alertdialog" aria-modal="true">
          <p class="confirm-title">{{ confirmTitle() }}</p>
          <p class="confirm-body">{{ confirmBody() }}</p>
          @if (moveError()) {
            <p class="confirm-error" role="alert">{{ moveError() }}</p>
          }
          <div class="confirm-actions">
            <button
              type="button"
              class="btn btn-ghost"
              [disabled]="moving()"
              (click)="cancelConfirm()"
            >
              Cancel
            </button>
            <button
              type="button"
              class="btn btn-primary"
              [disabled]="moving()"
              (click)="applyMove(target.id)"
            >
              {{ moving() ? "Moving…" : confirmCta() }}
            </button>
          </div>
        </div>
      } @else {
        <ul class="opts">
          <!-- Vault root -->
          <li>
            <button
              type="button"
              class="opt"
              role="menuitem"
              [class.is-current]="currentFolderId() === null"
              [disabled]="moving() || currentFolderId() === null"
              (click)="pick(null)"
            >
              <span class="opt-name">All notes (vault root)</span>
              @if (currentFolderId() === null) {
                <span class="opt-here">Here</span>
              }
            </button>
          </li>

          @for (opt of options(); track opt.node.id) {
            <li>
              <button
                type="button"
                class="opt"
                role="menuitem"
                [style.--depth]="opt.depth"
                [class.is-current]="currentFolderId() === opt.node.id"
                [disabled]="moving() || currentFolderId() === opt.node.id"
                (click)="pick(opt.node.id)"
              >
                <span class="opt-name">{{ opt.node.name }}</span>
                <app-lock-badge [exposure]="folders.exposureOf(opt.node)" />
                @if (currentFolderId() === opt.node.id) {
                  <span class="opt-here">Here</span>
                }
              </button>
            </li>
          } @empty {
            <li><p class="opts-empty">No folders to move into.</p></li>
          }
        </ul>
      }
    </div>
  `,
  styles: [
    `
      :host {
        display: block;
      }
      .menu {
        padding: var(--space-3);
        min-width: 260px;
        max-width: 320px;
      }
      .menu-head {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-3);
        margin-bottom: var(--space-2);
      }
      .menu-title {
        margin: 0;
        font-size: 0.9rem;
        font-weight: 600;
      }
      .menu-close {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 26px;
        height: 26px;
        padding: 0;
        border: none;
        border-radius: var(--radius-sm);
        background: var(--surface-input);
        color: var(--text-muted);
        cursor: pointer;
        transition:
          color var(--transition),
          background var(--transition);
      }
      .menu-close:hover {
        color: var(--text-primary);
        background: var(--surface-hover);
      }
      .menu-close:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }

      .opts {
        list-style: none;
        margin: 0;
        padding: 0;
        max-height: 280px;
        overflow-y: auto;
      }
      .opt {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        width: 100%;
        padding: var(--space-2) var(--space-2);
        padding-left: calc(var(--space-2) + var(--depth, 0) * var(--space-4));
        border: 1px solid transparent;
        border-radius: var(--radius-md);
        background: transparent;
        color: var(--text-secondary);
        font-family: inherit;
        font-size: 0.875rem;
        font-weight: 550;
        text-align: left;
        cursor: pointer;
        transition:
          color var(--transition),
          background var(--transition);
      }
      .opt:hover:not(:disabled) {
        color: var(--text-primary);
        background: var(--surface-hover);
      }
      .opt:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .opt:disabled {
        cursor: default;
      }
      .opt.is-current {
        color: var(--text-muted);
      }
      .opt-name {
        flex: 1 1 auto;
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .opt-here {
        flex: none;
        padding: 0 var(--space-2);
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        color: var(--text-muted);
        font-size: 0.7rem;
        font-weight: 600;
        text-transform: uppercase;
        letter-spacing: 0.03em;
      }
      .opts-empty {
        margin: var(--space-2);
        color: var(--text-muted);
        font-size: 0.8125rem;
      }

      /* --- Load-bearing confirm (names the exact destructive consequence) --- */
      .confirm {
        padding: var(--space-3);
        border: 1px solid var(--warning);
        border-radius: var(--radius-md);
        background: var(--warning-soft);
      }
      .confirm-title {
        margin: 0 0 var(--space-1);
        color: var(--text-primary);
        font-weight: 600;
      }
      .confirm-body {
        margin: 0;
        color: var(--text-secondary);
        font-size: 0.85rem;
        line-height: 1.5;
      }
      .confirm-error {
        margin: var(--space-2) 0 0;
        color: var(--danger);
        font-size: 0.8rem;
      }
      .confirm-actions {
        display: flex;
        justify-content: flex-end;
        gap: var(--space-2);
        margin-top: var(--space-4);
      }
    `,
  ],
})
export class MoveToMenuComponent {
  readonly folders = inject(FoldersService);
  private readonly toast = inject(ToastService);

  /** The note being moved. */
  readonly meetingId = input.required<string>();
  /** The note's current owning folder id (null = vault root) — drives "Here". */
  readonly currentFolderId = input<string | null>(null);

  /** Fired after a successful move, with the new folder id (null = root). */
  readonly moved = output<string | null>();
  /** Fired when the menu should be dismissed. */
  readonly close = output<void>();

  /** True while a move IPC is in flight. */
  readonly moving = signal(false);
  /** Non-null when the last move failed (kept until cancel / retry). */
  readonly moveError = signal<string | null>(null);
  /**
   * The pending target awaiting a load-bearing confirm. Holds the target node
   * (or null for the vault root, modelled as a sentinel id). Null = no confirm
   * open and the option list is shown.
   */
  readonly pending = signal<{
    id: string | null;
    node: FolderNode | null;
  } | null>(null);

  /** Flattened, depth-indented folder options (depth-first). */
  readonly options = computed<FolderOption[]>(() => {
    const out: FolderOption[] = [];
    const walk = (nodes: FolderNode[], depth: number): void => {
      for (const node of nodes) {
        out.push({ node, depth });
        if (node.children.length) {
          walk(node.children, depth + 1);
        }
      }
    };
    walk(this.folders.tree(), 0);
    return out;
  });

  /** Find a node by id across the whole forest (for confirm copy / exposure). */
  private nodeById(id: string | null): FolderNode | null {
    if (id === null) {
      return null;
    }
    return this.options().find((o) => o.node.id === id)?.node ?? null;
  }

  /** Whether the note currently lives in a sealed (locked) folder. */
  private readonly sourceLocked = computed(() => {
    const src = this.nodeById(this.currentFolderId());
    return src?.locked ?? false;
  });

  /**
   * Pick a target. If the move crosses an encryption boundary (into OR out of a
   * locked folder) we open the load-bearing confirm; otherwise apply at once.
   */
  pick(targetId: string | null): void {
    if (targetId === this.currentFolderId()) {
      return; // already here
    }
    const targetNode = this.nodeById(targetId);
    const intoLocked = targetNode?.locked ?? false;
    const outOfLocked = this.sourceLocked();

    if (intoLocked || outOfLocked) {
      this.moveError.set(null);
      this.pending.set({ id: targetId, node: targetNode });
      return;
    }
    void this.applyMove(targetId);
  }

  /** Whether the pending move is INTO a locked folder (vs out of one). */
  private readonly pendingIntoLocked = computed(
    () => this.pending()?.node?.locked ?? false,
  );

  /** Confirm headline — names which boundary is being crossed. */
  readonly confirmTitle = computed(() =>
    this.pendingIntoLocked()
      ? "Move into a locked folder?"
      : "Move out of a locked folder?",
  );

  /** Confirm body — spells out the exact, irreversible-on-disk consequence. */
  readonly confirmBody = computed(() => {
    const name = this.pending()?.node?.name ?? "the vault root";
    return this.pendingIntoLocked()
      ? `This encrypts the note into “${name}” and removes its plaintext Markdown from your vault. It’ll only be readable while the folder is unlocked.`
      : `This decrypts the note and re-exports its plaintext Markdown back into your vault at “${name}”.`;
  });

  /** Confirm primary-button label. */
  readonly confirmCta = computed(() =>
    this.pendingIntoLocked() ? "Encrypt & move" : "Re-export & move",
  );

  /** Dismiss the confirm and return to the option list. */
  cancelConfirm(): void {
    if (this.moving()) {
      return;
    }
    this.pending.set(null);
    this.moveError.set(null);
  }

  /**
   * Perform the move. Awaits {@link FoldersService.moveNote} (which reloads the
   * tree), toasts the outcome, emits `moved`, and closes. On failure the inline
   * error is shown; the confirm/list stays open for a retry.
   */
  async applyMove(targetId: string | null): Promise<void> {
    if (this.moving()) {
      return;
    }
    this.moving.set(true);
    this.moveError.set(null);
    try {
      await this.folders.moveNote(this.meetingId(), targetId);
      const targetName = this.nodeById(targetId)?.name ?? "All notes";
      this.toast.success(`Moved to ${targetName}`);
      this.moved.emit(targetId);
      this.close.emit();
    } catch {
      this.moveError.set("Couldn’t move this note. Please try again.");
    } finally {
      this.moving.set(false);
    }
  }
}
