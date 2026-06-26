import {
  ChangeDetectionStrategy,
  Component,
  computed,
  forwardRef,
  inject,
  input,
  output,
  signal,
} from "@angular/core";
import {
  FoldersService,
  type FolderExposure,
} from "../../services/folders.service";
import type { FolderNode } from "../../core/models";
import { FolderTreeComponent } from "./folder-tree.component";
import { LockBadgeComponent } from "./lock-badge.component";

/**
 * One folder row in the tree: disclosure caret · name · note-count chip · inline
 * {@link LockBadgeComponent} · a hover lock/unlock affordance. Selecting the row
 * emits `select` (the folder id) so the parent screen can filter the meeting
 * list; the row itself owns no selection state.
 *
 * Recursion is by CHILD COMPONENT — a row renders its children through a nested
 * {@link FolderTreeComponent}, never a `@for`-of-rows inside a row. Each level's
 * `depth` only drives the indent.
 *
 * Lock affordances delegate straight to {@link FoldersService}; the service
 * reloads the tree, so this row re-renders with fresh flags (no local toggling
 * of the backend-owned `locked` / `unlocked`).
 */
@Component({
  selector: "app-folder-row",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  // `folder-row` ↔ `folder-tree` are mutually recursive standalone components
  // (a row renders its children through another tree). Their ES modules form a
  // cycle, so a direct reference here is `undefined` at metadata-evaluation time
  // — Angular would then hit `getComponentDef(undefined)` (reading 'ɵcmp') the
  // first time a row instantiates a child tree. `forwardRef` defers the lookup
  // until the def exists, breaking the cycle. (See folder-tree for the mirror.)
  imports: [LockBadgeComponent, forwardRef(() => FolderTreeComponent)],
  template: `
    <div class="row-line" [style.--depth]="depth()">
      <!-- Disclosure caret (only when the folder has children) -->
      @if (childCount()) {
        <button
          type="button"
          class="caret"
          [class.is-open]="expanded()"
          [attr.aria-expanded]="expanded()"
          [attr.aria-label]="
            (expanded() ? 'Collapse ' : 'Expand ') + node().name
          "
          (click)="toggleExpanded()"
        >
          <svg viewBox="0 0 12 12" width="11" height="11" aria-hidden="true">
            <path
              d="M4.5 2.5 8 6l-3.5 3.5"
              stroke="currentColor"
              stroke-width="1.5"
              stroke-linecap="round"
              stroke-linejoin="round"
              fill="none"
            />
          </svg>
        </button>
      } @else {
        <span class="caret caret--leaf" aria-hidden="true"></span>
      }

      <!-- The selectable folder button: name + lock badge + count chip -->
      <button
        type="button"
        class="folder"
        [class.is-selected]="selectedId() === node().id"
        [attr.aria-pressed]="selectedId() === node().id"
        (click)="select.emit(node().id)"
      >
        <span class="folder-icon" aria-hidden="true">
          <svg viewBox="0 0 16 16" width="15" height="15" fill="none">
            <path
              d="M1.75 4.25c0-.7.55-1.25 1.25-1.25h2.8c.4 0 .77.18 1 .5l.6.75h4.6c.7 0 1.25.55 1.25 1.25v5.5c0 .7-.55 1.25-1.25 1.25H3c-.7 0-1.25-.55-1.25-1.25z"
              stroke="currentColor"
              stroke-width="1.3"
              stroke-linejoin="round"
            />
          </svg>
        </span>
        <span class="folder-name">{{ node().name }}</span>
        <app-lock-badge [exposure]="exposure()" />
        @if (node().noteCount > 0) {
          <span class="count folder-count">{{ node().noteCount }}</span>
        }
      </button>

      <!-- Lock / unlock affordance (revealed on hover/focus of the row) -->
      <div class="row-actions">
        @switch (exposure()) {
          @case ("open") {
            <button
              type="button"
              class="act-btn"
              [disabled]="busy()"
              [attr.aria-label]="'Lock ' + node().name"
              title="Encrypt this folder"
              (click)="onLock()"
            >
              <svg
                viewBox="0 0 16 16"
                width="14"
                height="14"
                fill="none"
                aria-hidden="true"
              >
                <rect
                  x="3.5"
                  y="7"
                  width="9"
                  height="6"
                  rx="1.4"
                  stroke="currentColor"
                  stroke-width="1.3"
                />
                <path
                  d="M5.5 7V5.4a2.5 2.5 0 0 1 5 0V7"
                  stroke="currentColor"
                  stroke-width="1.3"
                  stroke-linecap="round"
                />
              </svg>
            </button>
          }
          @case ("locked") {
            <button
              type="button"
              class="act-btn"
              [disabled]="busy()"
              [attr.aria-label]="'Unlock ' + node().name + ' for this session'"
              title="Unlock for this session"
              (click)="onUnlock()"
            >
              <svg
                viewBox="0 0 16 16"
                width="14"
                height="14"
                fill="none"
                aria-hidden="true"
              >
                <rect
                  x="3.5"
                  y="7"
                  width="9"
                  height="6"
                  rx="1.4"
                  stroke="currentColor"
                  stroke-width="1.3"
                />
                <path
                  d="M5.5 7V5.4a2.5 2.5 0 0 1 4.9-0.65"
                  stroke="currentColor"
                  stroke-width="1.3"
                  stroke-linecap="round"
                />
              </svg>
            </button>
          }
          @case ("session") {
            <button
              type="button"
              class="act-btn is-session"
              [disabled]="busy()"
              [attr.aria-label]="'Re-lock ' + node().name"
              title="Re-seal now"
              (click)="onRelock()"
            >
              <svg
                viewBox="0 0 16 16"
                width="14"
                height="14"
                fill="none"
                aria-hidden="true"
              >
                <rect
                  x="3.5"
                  y="7"
                  width="9"
                  height="6"
                  rx="1.4"
                  stroke="currentColor"
                  stroke-width="1.3"
                />
                <path
                  d="M5.5 7V5.4a2.5 2.5 0 0 1 5 0V7"
                  stroke="currentColor"
                  stroke-width="1.3"
                  stroke-linecap="round"
                />
              </svg>
            </button>
          }
        }
      </div>
    </div>

    @if (lockError()) {
      <p class="row-error" role="alert" [style.--depth]="depth()">
        {{ lockError() }}
      </p>
    }

    <!-- Children: child-component recursion (NOT a nested @for of rows). -->
    @if (expanded() && childCount()) {
      <app-folder-tree
        [nodes]="children()"
        [selectedId]="selectedId()"
        [depth]="depth() + 1"
        (select)="select.emit($event)"
      />
    }
  `,
  styles: [
    `
      :host {
        display: block;
      }
      .row-line {
        display: flex;
        align-items: center;
        gap: var(--space-1);
        padding-left: calc(var(--depth, 0) * var(--space-4));
        border-radius: var(--radius-md);
        transition: background var(--transition);
      }
      .row-line:hover {
        background: var(--surface-hover);
      }

      .caret {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        flex: none;
        width: 20px;
        height: 20px;
        padding: 0;
        border: none;
        border-radius: var(--radius-sm);
        background: transparent;
        color: var(--text-muted);
        cursor: pointer;
        transition:
          transform var(--transition),
          color var(--transition),
          background var(--transition);
      }
      .caret:hover {
        color: var(--text-primary);
        background: var(--surface-input);
      }
      .caret.is-open {
        transform: rotate(90deg);
      }
      .caret:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .caret--leaf {
        cursor: default;
      }

      .folder {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        flex: 1 1 auto;
        min-width: 0;
        padding: var(--space-2) var(--space-2);
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
          background var(--transition),
          border-color var(--transition);
      }
      .folder:hover {
        color: var(--text-primary);
      }
      .folder:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .folder.is-selected {
        color: var(--accent-hover);
        background: var(--accent-soft);
      }
      .folder-icon {
        display: inline-flex;
        flex: none;
        color: var(--text-muted);
      }
      .folder.is-selected .folder-icon {
        color: var(--accent-hover);
      }
      .folder-name {
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .folder-count {
        margin-left: auto;
        min-width: 22px;
        height: 20px;
      }

      .row-actions {
        display: inline-flex;
        align-items: center;
        flex: none;
        opacity: 0;
        transition: opacity var(--transition);
      }
      .row-line:hover .row-actions,
      .row-line:focus-within .row-actions {
        opacity: 1;
      }
      .act-btn {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 28px;
        height: 28px;
        padding: 0;
        border: 1px solid transparent;
        border-radius: var(--radius-sm);
        background: var(--surface-input);
        color: var(--text-secondary);
        cursor: pointer;
        transition:
          color var(--transition),
          background var(--transition),
          border-color var(--transition),
          transform var(--transition-fast);
      }
      .act-btn:hover {
        color: var(--text-primary);
        border-color: var(--border-strong);
      }
      .act-btn.is-session {
        color: var(--accent-hover);
      }
      .act-btn:active {
        transform: scale(0.92);
      }
      .act-btn:focus-visible {
        outline: none;
        opacity: 1;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .act-btn:disabled {
        opacity: 0.4;
        cursor: not-allowed;
      }

      .row-error {
        margin: var(--space-1) 0 var(--space-2);
        padding-left: calc(var(--depth, 0) * var(--space-4) + var(--space-5));
        color: var(--danger);
        font-size: 0.78rem;
      }
    `,
  ],
})
export class FolderRowComponent {
  private readonly folders = inject(FoldersService);

  /** This row's folder node. */
  readonly node = input.required<FolderNode>();
  /** The currently-selected folder id (null = vault root) — drives the highlight. */
  readonly selectedId = input<string | null>(null);
  /** Indent depth (0 at the roots). */
  readonly depth = input<number>(0);

  /** Emits the folder id when this row (or a descendant) is chosen. */
  readonly select = output<string | null>();

  /** Whether this row's children subtree is shown. Roots start expanded. */
  readonly expanded = signal(true);
  /** True while a lock op for THIS row is in flight (guards the affordance). */
  readonly busy = signal(false);
  /** Per-row lock error (cleared on the next attempt). */
  readonly lockError = signal<string | null>(null);

  /** This folder's privacy exposure, derived from its lock flags. */
  readonly exposure = computed<FolderExposure>(() =>
    this.folders.exposureOf(this.node()),
  );

  /**
   * This folder's children — always an array, even if the backend node omits
   * the field. Keeps the disclosure caret + child recursion from ever reading
   * `.length` off `undefined` (which would throw and blank the tree).
   */
  readonly children = computed<FolderNode[]>(() => this.node().children ?? []);
  /** Number of children (drives the caret + recursion guards). */
  readonly childCount = computed(() => this.children().length);

  toggleExpanded(): void {
    this.expanded.update((v) => !v);
  }

  /** Seal this folder (delegates to the service; tree reload re-renders the row). */
  async onLock(): Promise<void> {
    await this.run(() => this.folders.lock(this.node().id));
  }

  /** Session-unlock this folder. */
  async onUnlock(): Promise<void> {
    await this.run(() => this.folders.unlock(this.node().id));
  }

  /** Re-seal this session-unlocked folder. */
  async onRelock(): Promise<void> {
    await this.run(() => this.folders.relock(this.node().id));
  }

  /** Shared lock-op runner: guard, await, surface a per-row error on failure. */
  private async run(op: () => Promise<void>): Promise<void> {
    if (this.busy()) {
      return;
    }
    this.busy.set(true);
    this.lockError.set(null);
    try {
      await op();
    } catch {
      // Leave the message visible until the next op (no component timer).
      this.lockError.set("Couldn’t change this folder’s lock. Try again.");
    } finally {
      this.busy.set(false);
    }
  }
}
