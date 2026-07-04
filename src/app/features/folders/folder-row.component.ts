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
import {
  FoldersService,
  type FolderExposure,
} from "../../services/folders.service";
import { ToastService } from "../../services/toast.service";
import type { FolderNode } from "../../core/models";
import { FolderTreeComponent } from "./folder-tree.component";
import { FolderDropDirective } from "./folder-drop.directive";

/**
 * One folder row in the tree: disclosure caret · folder glyph · name · note-count
 * chip · ONE unambiguous lock control. Selecting the row emits `select` (the
 * folder id) so the parent screen can filter the meeting list; the row itself
 * owns no selection state.
 *
 * SINGLE lock control (was a confusing double padlock — a passive badge AND a
 * toggle). Now exactly one `lock-toggle` button per row carries BOTH the state
 * and the action:
 *   - open    → a faint open-padlock, revealed on hover; click = "Encrypt".
 *   - locked  → a solid closed padlock, ALWAYS visible; click = "Unlock for
 *               this session".
 *   - session → an accent open-padlock, ALWAYS visible; click = "Re-seal now".
 * State is read straight from the lock control, so there is no second glyph.
 *
 * The row is also a DROP TARGET (`appFolderDrop`): a meeting dragged from the
 * list can be dropped here to file it into this folder.
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
  imports: [FolderDropDirective, forwardRef(() => FolderTreeComponent)],
  template: `
    <div
      class="row-line"
      [class.is-exposure-locked]="exposure() === 'locked'"
      [class.is-exposure-session]="exposure() === 'session'"
      [style.--depth]="depth()"
      appFolderDrop
      [dropFolderId]="node().id"
      (dropNote)="dropNote.emit({ meetingId: $event, folderId: node().id })"
    >
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

      <!-- The selectable folder button: glyph + name + count chip.
           NO lock badge here — the single lock control (below) owns lock state.
           When renaming, the name is REPLACED by an inline edit field (Enter=save,
           Esc=cancel; focused via afterNextRender). -->
      @if (renaming()) {
        <form class="rename-form" (submit)="confirmRename($event)">
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
          <input
            #renameInput
            type="text"
            class="rename-input"
            autocomplete="off"
            spellcheck="false"
            aria-label="Rename folder"
            [value]="renameDraft()"
            [disabled]="busy()"
            (input)="onRenameInput($event)"
            (keydown.escape)="cancelRename()"
            (blur)="cancelRename()"
          />
        </form>
      } @else {
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
          @if (node().noteCount > 0) {
            <span class="count folder-count">{{ node().noteCount }}</span>
          }
        </button>
      }

      <!-- ONE lock control. It is the state AND the action: a single padlock per
           row. Always visible while locked/session (so the privacy state always
           reads); revealed on hover for an open folder (the "encrypt" action). -->
      <div class="row-actions">
        @switch (exposure()) {
          @case ("open") {
            <button
              type="button"
              class="lock-toggle"
              [disabled]="busy()"
              [attr.aria-label]="'Lock ' + node().name"
              title="Encrypt this folder"
              (click)="onLock()"
            >
              <!-- open padlock (shackle up & off the body) -->
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
          @case ("locked") {
            <button
              type="button"
              class="lock-toggle is-locked"
              [disabled]="busy()"
              [attr.aria-label]="'Unlock ' + node().name + ' for this session'"
              title="Locked — click to unlock for this session"
              (click)="onUnlock()"
            >
              <!-- closed padlock, filled body -->
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
                  fill="currentColor"
                  stroke="currentColor"
                  stroke-width="1.3"
                />
                <path
                  d="M5.5 7V5.4a2.5 2.5 0 0 1 5 0V7"
                  stroke="currentColor"
                  stroke-width="1.3"
                  stroke-linecap="round"
                />
                <circle cx="8" cy="10" r="1" fill="var(--surface-base)" />
              </svg>
            </button>
          }
          @case ("session") {
            <button
              type="button"
              class="lock-toggle is-session"
              [disabled]="busy()"
              [attr.aria-label]="'Re-lock ' + node().name"
              title="Unlocked this session — click to re-seal now"
              (click)="onRelock()"
            >
              <!-- open padlock, accent — plaintext exposed this session -->
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
                  stroke-width="1.4"
                />
                <path
                  d="M5.5 7V5.4a2.5 2.5 0 0 1 4.9-0.65"
                  stroke="currentColor"
                  stroke-width="1.4"
                  stroke-linecap="round"
                />
                <circle cx="8" cy="10" r="1.05" fill="currentColor" />
              </svg>
            </button>
          }
        }

        <!-- The ⋯ folder-actions menu: Rename · Make private/Unlock · Delete. -->
        <div class="menu-wrap">
          <button
            type="button"
            class="menu-trigger"
            [class.is-open]="menuOpen()"
            [disabled]="busy()"
            [attr.aria-haspopup]="true"
            [attr.aria-expanded]="menuOpen()"
            [attr.aria-label]="'Folder actions for ' + node().name"
            title="Folder actions"
            (click)="toggleMenu()"
          >
            <svg viewBox="0 0 16 16" width="15" height="15" aria-hidden="true">
              <circle cx="8" cy="3.2" r="1.35" fill="currentColor" />
              <circle cx="8" cy="8" r="1.35" fill="currentColor" />
              <circle cx="8" cy="12.8" r="1.35" fill="currentColor" />
            </svg>
          </button>

          @if (menuOpen()) {
            <div
              class="menu"
              role="menu"
              [attr.aria-label]="node().name + ' actions'"
            >
              @if (confirmingDelete()) {
                <!-- Load-bearing confirm: names the exact consequence (notes move, or unlock first). -->
                <div class="confirm" role="alertdialog" aria-modal="true">
                  <p class="confirm-body">{{ deleteConfirmText() }}</p>
                  @if (actionError()) {
                    <p class="confirm-error" role="alert">
                      {{ actionError() }}
                    </p>
                  }
                  <div class="confirm-actions">
                    <button
                      type="button"
                      class="btn btn-ghost"
                      [disabled]="busy()"
                      (click)="cancelDelete()"
                    >
                      Cancel
                    </button>
                    @if (canDelete()) {
                      <button
                        type="button"
                        class="btn btn-danger"
                        [disabled]="busy()"
                        (click)="onDelete()"
                      >
                        {{ busy() ? "Deleting…" : "Delete folder" }}
                      </button>
                    }
                  </div>
                </div>
              } @else {
                <button
                  type="button"
                  class="menu-item"
                  role="menuitem"
                  (click)="startRename()"
                >
                  <span class="menu-icon" aria-hidden="true">
                    <svg viewBox="0 0 16 16" width="14" height="14" fill="none">
                      <path
                        d="M11.3 2.2 13.8 4.7 5.5 13H3v-2.5z"
                        stroke="currentColor"
                        stroke-width="1.3"
                        stroke-linejoin="round"
                      />
                    </svg>
                  </span>
                  Rename
                </button>

                <!-- "Make private" (lock) when open; "Unlock" via Touch ID when locked;
                     "Re-seal now" when session-unlocked. Reuses the row's lock affordances. -->
                @switch (exposure()) {
                  @case ("open") {
                    <button
                      type="button"
                      class="menu-item"
                      role="menuitem"
                      (click)="onLockFromMenu()"
                    >
                      <span class="menu-icon" aria-hidden="true">🔒</span>
                      Make private
                    </button>
                  }
                  @case ("locked") {
                    <button
                      type="button"
                      class="menu-item"
                      role="menuitem"
                      (click)="onUnlockFromMenu()"
                    >
                      <span class="menu-icon" aria-hidden="true">🔑</span>
                      Locked — unlock
                    </button>
                  }
                  @case ("session") {
                    <button
                      type="button"
                      class="menu-item"
                      role="menuitem"
                      (click)="onRelockFromMenu()"
                    >
                      <span class="menu-icon" aria-hidden="true">🔒</span>
                      Re-seal now
                    </button>
                  }
                }

                <button
                  type="button"
                  class="menu-item is-danger"
                  role="menuitem"
                  (click)="startDelete()"
                >
                  <span class="menu-icon" aria-hidden="true">
                    <svg viewBox="0 0 16 16" width="14" height="14" fill="none">
                      <path
                        d="M3.5 4.5h9M6.5 4.5V3.2c0-.4.3-.7.7-.7h1.6c.4 0 .7.3.7.7v1.3M5 4.5l.5 8c0 .4.3.7.7.7h3.6c.4 0 .7-.3.7-.7l.5-8"
                        stroke="currentColor"
                        stroke-width="1.3"
                        stroke-linecap="round"
                        stroke-linejoin="round"
                      />
                    </svg>
                  </span>
                  Delete folder
                </button>
              }
            </div>
          }
        </div>
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
        (dropNote)="dropNote.emit($event)"
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
        border: 1px solid transparent;
        border-radius: var(--radius-md);
        transition:
          background var(--transition),
          border-color var(--transition),
          box-shadow var(--transition);
      }
      .row-line:hover {
        background: var(--surface-hover);
      }

      /* --- Drop target (a note dragged from the list) --------------------- */
      /* Armed: every folder shows a faint dashed hint the instant a drag starts. */
      .row-line.is-drop-armed {
        border-color: var(--border-strong);
        border-style: dashed;
      }
      /* Active: the folder under the pointer lights up with the accent. */
      .row-line.is-drop-target {
        border-style: solid;
        border-color: var(--accent);
        background: var(--accent-soft);
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .row-line.is-drop-target .folder,
      .row-line.is-drop-target .folder-icon {
        color: var(--accent-hover);
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
      /* Selected: the shell's neutral glass-on-glass pill (accent on the
         glyph/label only — matches every other rail in the app). */
      .folder.is-selected {
        color: var(--shell-active-text);
        background: var(--shell-active-bg);
        box-shadow: var(--shell-active-shadow);
      }
      .folder-icon {
        display: inline-flex;
        flex: none;
        color: var(--text-muted);
      }
      .folder.is-selected .folder-icon {
        color: var(--shell-active-text);
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

      /* --- ONE lock control: state + action in a single button ----------- */
      .row-actions {
        display: inline-flex;
        align-items: center;
        flex: none;
        margin-right: 2px;
      }
      .lock-toggle {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 28px;
        height: 28px;
        padding: 0;
        border: 1px solid transparent;
        border-radius: var(--radius-sm);
        background: transparent;
        color: var(--text-muted);
        cursor: pointer;
        /* OPEN folders: the lock action is quiet until the row is hovered. */
        opacity: 0;
        transition:
          color var(--transition),
          background var(--transition),
          border-color var(--transition),
          opacity var(--transition),
          transform var(--transition-fast);
      }
      .row-line:hover .lock-toggle,
      .row-line:focus-within .lock-toggle {
        opacity: 1;
      }
      /* LOCKED / SESSION: always visible — the control IS the state badge. */
      .lock-toggle.is-locked,
      .lock-toggle.is-session {
        opacity: 1;
        background: var(--surface-input);
      }
      .lock-toggle.is-locked {
        color: var(--text-secondary);
      }
      .lock-toggle.is-session {
        color: var(--accent-hover);
        background: var(--accent-soft);
        /* a gentle one-shot pop when a folder unseals for the session */
        animation: lock-pop 220ms var(--ease-spring) both;
      }
      .lock-toggle:hover:not(:disabled) {
        color: var(--text-primary);
        background: var(--surface-hover);
        border-color: var(--border-strong);
      }
      .lock-toggle.is-session:hover:not(:disabled) {
        color: var(--accent-hover);
        border-color: var(--accent);
      }
      .lock-toggle:active:not(:disabled) {
        transform: scale(0.92);
      }
      .lock-toggle:focus-visible {
        outline: none;
        opacity: 1;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .lock-toggle:disabled {
        opacity: 0.4;
        cursor: not-allowed;
      }
      @keyframes lock-pop {
        from {
          transform: scale(0.55);
          opacity: 0;
        }
        to {
          transform: scale(1);
          opacity: 1;
        }
      }
      @media (prefers-reduced-motion: reduce) {
        .lock-toggle.is-session {
          animation: none;
        }
      }

      /* --- Inline rename field (replaces the folder name button) ---------- */
      .rename-form {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        flex: 1 1 auto;
        min-width: 0;
        padding: var(--space-2) var(--space-2);
        border: 1px solid var(--accent);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .rename-input {
        flex: 1 1 auto;
        min-width: 0;
        height: 22px;
        padding: 0;
        border: none;
        background: transparent;
        color: var(--text-primary);
        font-family: inherit;
        font-size: 0.9rem;
        font-weight: 550;
        letter-spacing: -0.01em;
      }
      .rename-input:hover,
      .rename-input:focus {
        border: none;
        background: transparent;
        box-shadow: none;
        outline: none;
      }

      /* --- ⋯ folder-actions menu ----------------------------------------- */
      .menu-wrap {
        position: relative;
        display: inline-flex;
      }
      .menu-trigger {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 28px;
        height: 28px;
        padding: 0;
        border: 1px solid transparent;
        border-radius: var(--radius-sm);
        background: transparent;
        color: var(--text-muted);
        cursor: pointer;
        /* Quiet until the row is hovered/focused (like the lock toggle). */
        opacity: 0;
        transition:
          color var(--transition),
          background var(--transition),
          border-color var(--transition),
          opacity var(--transition);
      }
      .row-line:hover .menu-trigger,
      .row-line:focus-within .menu-trigger,
      .menu-trigger.is-open {
        opacity: 1;
      }
      .menu-trigger:hover:not(:disabled),
      .menu-trigger.is-open {
        color: var(--text-primary);
        background: var(--surface-hover);
        border-color: var(--border-strong);
      }
      .menu-trigger:focus-visible {
        outline: none;
        opacity: 1;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .menu-trigger:disabled {
        opacity: 0.4;
        cursor: not-allowed;
      }

      /* Floating popover OVER the tree → OPAQUE surface (never the frosted .card). */
      .menu {
        position: absolute;
        top: calc(100% + 4px);
        right: 0;
        z-index: 30;
        min-width: 196px;
        padding: var(--space-1);
        background: var(--surface-overlay);
        border: 1px solid var(--border-strong);
        border-radius: var(--radius-md);
        box-shadow: var(--shadow-lg);
        -webkit-backdrop-filter: none;
        backdrop-filter: none;
      }
      .menu-item {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        width: 100%;
        padding: var(--space-2) var(--space-2);
        border: 1px solid transparent;
        border-radius: var(--radius-sm);
        background: transparent;
        color: var(--text-secondary);
        font-family: inherit;
        font-size: 0.85rem;
        font-weight: 550;
        text-align: left;
        cursor: pointer;
        transition:
          color var(--transition),
          background var(--transition);
      }
      .menu-item:hover {
        color: var(--text-primary);
        background: var(--surface-hover);
      }
      .menu-item:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .menu-item.is-danger {
        color: var(--danger);
      }
      .menu-item.is-danger:hover {
        color: var(--danger);
        background: var(--danger-soft);
      }
      .menu-icon {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        flex: none;
        width: 18px;
        height: 18px;
        font-size: 0.8rem;
        color: var(--text-muted);
      }
      .menu-item.is-danger .menu-icon {
        color: var(--danger);
      }

      /* Load-bearing delete confirm (names the consequence). */
      .confirm {
        padding: var(--space-2);
      }
      .confirm-body {
        margin: 0 0 var(--space-2);
        color: var(--text-secondary);
        font-size: 0.8125rem;
        line-height: 1.4;
      }
      .confirm-error {
        margin: 0 0 var(--space-2);
        color: var(--danger);
        font-size: 0.78rem;
      }
      .confirm-actions {
        display: flex;
        justify-content: flex-end;
        gap: var(--space-2);
      }
      .confirm-actions .btn {
        height: 30px;
        padding: 0 var(--space-3);
        font-size: 0.8rem;
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
  private readonly toast = inject(ToastService);
  private readonly injector = inject(Injector);

  /** This row's folder node. */
  readonly node = input.required<FolderNode>();
  /** The currently-selected folder id (null = vault root) — drives the highlight. */
  readonly selectedId = input<string | null>(null);
  /** Indent depth (0 at the roots). */
  readonly depth = input<number>(0);

  /** Emits the folder id when this row (or a descendant) is chosen. */
  readonly select = output<string | null>();

  /** Emits when a dragged meeting is dropped onto this row (or a descendant). */
  readonly dropNote = output<{ meetingId: string; folderId: string | null }>();

  /** Whether this row's children subtree is shown. Roots start expanded. */
  readonly expanded = signal(true);
  /** True while a lock/rename/delete op for THIS row is in flight (guards the affordances). */
  readonly busy = signal(false);
  /** Per-row lock/action error (cleared on the next attempt). */
  readonly lockError = signal<string | null>(null);

  // --- ⋯ folder-actions menu + inline rename + delete confirm ---------------
  /** Whether the ⋯ actions menu popover is open. */
  readonly menuOpen = signal(false);
  /** Whether the inline rename field is showing (replaces the name button). */
  readonly renaming = signal(false);
  /** Draft folder name bound to the rename field. */
  readonly renameDraft = signal("");
  /** Whether the delete-confirm step is showing inside the menu. */
  readonly confirmingDelete = signal(false);
  /** Error surfaced inside the delete confirm (e.g. a backend reject). */
  readonly actionError = signal<string | null>(null);

  /** The rename input — focused after it renders (afterNextRender; no setTimeout). */
  private readonly renameInput =
    viewChild<ElementRef<HTMLInputElement>>("renameInput");

  /** This folder's privacy exposure, derived from its lock flags. */
  readonly exposure = computed<FolderExposure>(() =>
    this.folders.exposureOf(this.node()),
  );

  /**
   * Whether DELETE may proceed right now. A sealed folder that is NOT session-unlocked cannot be
   * deleted (its notes are encrypted and the backend refuses) — the confirm shows the "unlock first"
   * message and hides the destructive button. An open or session-unlocked folder can delete.
   */
  readonly canDelete = computed(() => this.exposure() !== "locked");

  /**
   * The delete-confirm copy — names the exact consequence:
   *  - sealed + not unlocked → "Unlock this folder first to delete it."
   *  - otherwise             → "Delete 'X'? Its N notes move to All notes." (or no-notes variant).
   */
  readonly deleteConfirmText = computed(() => {
    const f = this.node();
    if (!this.canDelete()) {
      return `“${f.name}” is locked. Unlock this folder first to delete it.`;
    }
    const n = f.noteCount;
    if (n <= 0) {
      return `Delete “${f.name}”? This empty folder will be removed.`;
    }
    const notes = n === 1 ? "note" : "notes";
    return `Delete “${f.name}”? Its ${n} ${notes} move to All notes — nothing is lost.`;
  });

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

  // --- ⋯ menu --------------------------------------------------------------
  /** Open/close the actions menu. Closing resets any in-menu delete-confirm. */
  toggleMenu(): void {
    const next = !this.menuOpen();
    this.menuOpen.set(next);
    if (!next) {
      this.confirmingDelete.set(false);
      this.actionError.set(null);
    }
  }

  /** Close the menu (and any confirm) — used after an action resolves. */
  private closeMenu(): void {
    this.menuOpen.set(false);
    this.confirmingDelete.set(false);
    this.actionError.set(null);
  }

  // --- Lock actions from the menu (reuse the existing run() lock affordances) ---
  /** "Make private" — seal the whole folder. */
  async onLockFromMenu(): Promise<void> {
    this.closeMenu();
    await this.onLock();
  }

  /** "Locked — unlock" — session-unlock via Touch ID. */
  async onUnlockFromMenu(): Promise<void> {
    this.closeMenu();
    await this.onUnlock();
  }

  /** "Re-seal now" — re-lock a session-unlocked folder. */
  async onRelockFromMenu(): Promise<void> {
    this.closeMenu();
    await this.onRelock();
  }

  // --- Inline rename -------------------------------------------------------
  /** Open the inline rename field (seeded with the current name) + focus it once rendered. */
  startRename(): void {
    this.renameDraft.set(this.node().name);
    this.renaming.set(true);
    this.menuOpen.set(false);
    afterNextRender(
      () => {
        const el = this.renameInput()?.nativeElement;
        el?.focus();
        el?.select();
      },
      { injector: this.injector },
    );
  }

  onRenameInput(event: Event): void {
    this.renameDraft.set((event.target as HTMLInputElement).value);
  }

  /** Close the rename field without saving. Ignored mid-save. */
  cancelRename(): void {
    if (this.busy()) {
      return;
    }
    this.renaming.set(false);
    this.renameDraft.set("");
  }

  /**
   * Save the rename. A no-op when the name is unchanged/empty. On success the field closes and the
   * tree reloads (the row re-renders with the new name). On failure we keep the field open (so the
   * typed name isn't lost) and surface a single danger toast.
   */
  async confirmRename(event: Event): Promise<void> {
    event.preventDefault();
    const name = this.renameDraft().trim();
    if (this.busy()) {
      return;
    }
    if (!name || name === this.node().name) {
      this.cancelRename();
      return;
    }
    this.busy.set(true);
    try {
      await this.folders.rename(this.node().id, name);
      this.renaming.set(false);
      this.renameDraft.set("");
    } catch {
      this.toast.danger("Couldn’t rename that folder. Try another name.");
    } finally {
      this.busy.set(false);
    }
  }

  // --- Delete --------------------------------------------------------------
  /** Switch the menu into its delete-confirm step. */
  startDelete(): void {
    this.actionError.set(null);
    this.confirmingDelete.set(true);
  }

  /** Back out of the delete confirm (keeps the menu open on the action list). */
  cancelDelete(): void {
    if (this.busy()) {
      return;
    }
    this.confirmingDelete.set(false);
    this.actionError.set(null);
  }

  /**
   * Delete the folder. Its notes move to the vault root (the backend never loses a note). On a
   * backend reject (e.g. still has subfolders) we show the message inline in the confirm. The "unlock
   * first" case is handled BEFORE this is reachable (the destructive button is hidden when locked).
   */
  async onDelete(): Promise<void> {
    if (this.busy() || !this.canDelete()) {
      return;
    }
    this.busy.set(true);
    this.actionError.set(null);
    try {
      await this.folders.delete(this.node().id);
      this.closeMenu();
      this.toast.success("Folder deleted. Its notes are in All notes.");
      // If this folder was selected, fall back to the vault root.
      if (this.selectedId() === this.node().id) {
        this.select.emit(null);
      }
    } catch {
      this.actionError.set("Couldn’t delete this folder. Please try again.");
    } finally {
      this.busy.set(false);
    }
  }
}
