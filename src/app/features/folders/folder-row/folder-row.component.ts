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
} from "../../../services/folders.service";
import { FolderLockFlowService } from "../../../services/folder-lock-flow.service";
import { ToastService } from "../../../services/toast.service";
import type { FolderNode } from "../../../core/models";
import { MurTreeRowComponent } from "../../../design-system/tree-row/tree-row.component";
import { MurRowMenuComponent } from "../../../design-system/row-menu/row-menu.component";
import { FolderTreeComponent } from "../folder-tree/folder-tree.component";
import { FolderDropDirective } from "../folder-drop.directive";

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
 *
 * TRAILING ACTIONS (2026-07-12, unification round 2 — see
 * `design-system/row-menu/row-menu.component.ts`'s class doc for the full
 * history): rename + delete live behind ONE shared `<mur-row-menu>` gear
 * dropdown — the exact same component instance type `NotesSidebarTreeComponent`
 * renders, not a hand-copied icon row. (Round 1 tried matching two SEPARATE
 * flat-icon implementations pixel-for-pixel by measured box size; they still
 * read as visibly different in color/weight between the two trees — a real
 * "looks the same on paper, isn't" bug, correctly caught and rejected. A
 * shared component makes that class of drift structurally impossible: there
 * is only one file that can render this dropdown's chrome.) The lock control
 * (open/locked/session `.lock-toggle`) stays its OWN always-visible control,
 * outside the dropdown — unlike rename/delete it is a security-relevant
 * glanceable state badge (see its doc below), and hiding it behind a menu
 * would regress a folder's sealed-state from "visible at a glance" to
 * "visible after two clicks." Deleting still opens the SAME
 * consequence-naming confirm as before ({@link deleteConfirmText}/
 * {@link canDelete} — notes move, or "unlock first" when sealed), now
 * replacing the row in place (mirrors the inline rename field) rather than
 * living inside the dropdown panel.
 */
@Component({
  selector: "app-folder-row",
  changeDetection: ChangeDetectionStrategy.OnPush,
  // `folder-row` ↔ `folder-tree` are mutually recursive standalone components
  // (a row renders its children through another tree). Their ES modules form a
  // cycle, so a direct reference here is `undefined` at metadata-evaluation time
  // — Angular would then hit `getComponentDef(undefined)` (reading 'ɵcmp') the
  // first time a row instantiates a child tree. `forwardRef` defers the lookup
  // until the def exists, breaking the cycle. (See folder-tree for the mirror.)
  imports: [
    FolderDropDirective,
    MurTreeRowComponent,
    MurRowMenuComponent,
    forwardRef(() => FolderTreeComponent),
  ],
  templateUrl: "./folder-row.component.html",
  styleUrl: "./folder-row.component.scss",
})
export class FolderRowComponent {
  private readonly folders = inject(FoldersService);
  private readonly toast = inject(ToastService);
  private readonly injector = inject(Injector);
  /**
   * Shared lock×shares flow (probe → warn/revoke dialog → lock) — a root
   * singleton, also driven by `NotesSidebarTreeComponent`'s folder rows. The
   * DIALOG itself is rendered exactly ONCE, by `AppShellComponent` (2026-07-12
   * fix: both this component and the Notes tree used to each render their own
   * `<app-lock-shares-dialog>` bound to the same singleton `pending()` — since
   * the main sidebar now ALWAYS mounts both trees simultaneously, that meant
   * TWO dialogs rendered for one lock request; caught by
   * `e2e/org/org-surfaces.spec.ts`'s strict-mode-violation failure).
   */
  readonly lockFlow = inject(FolderLockFlowService);

  /** This row's folder node. */
  readonly node = input.required<FolderNode>();
  /** The currently-selected folder id (null = vault root) — drives the highlight. */
  readonly selectedId = input<string | null>(null);
  /** Indent depth (0 at the roots). */
  readonly depth = input<number>(0);
  /** Whether `selectedId` should render as visually selected — see `FolderTreeComponent`'s doc. */
  readonly selectionActive = input<boolean>(true);

  /** Emits the folder id when this row (or a descendant) is chosen. */
  readonly selected = output<string | null>();

  /** Emits when a dragged meeting is dropped onto this row (or a descendant). */
  readonly dropNote = output<{ meetingId: string; folderId: string | null }>();

  /** Whether this row's children subtree is shown. Roots start expanded. */
  readonly expanded = signal(true);
  /** True while a lock/rename/delete op for THIS row is in flight (guards the affordances). */
  readonly busy = signal(false);
  /** Per-row lock/action error (cleared on the next attempt). */
  readonly lockError = signal<string | null>(null);

  // --- inline rename + delete confirm (flat icons — 2026-07-12: the ⋯
  // dropdown these used to live behind is gone; see the class doc's
  // unification note) --------------------------------------------------------
  /** Whether the inline rename field is showing (replaces the name button). */
  readonly renaming = signal(false);
  /** Draft folder name bound to the rename field. */
  readonly renameDraft = signal("");
  /** Whether the delete-confirm step is showing (replaces the row — see the template). */
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

  /**
   * Seal this folder. Delegates to the shared {@link FolderLockFlowService}, which
   * FIRST probes active shares (link / user / org) via `folder_active_shares`. If any
   * exist — OR the probe itself fails (FAIL-CLOSED, F5) — it opens the blocking
   * lock×shares dialog instead of locking straight away; the dialog's actions
   * (Revoke & lock / Lock anyway / Cancel) drive the real lock. With no shares and a
   * clean probe it locks directly. The tree reloads on lock, so this row re-renders
   * reactively — no host refresh needed.
   */
  async onLock(): Promise<void> {
    if (this.busy() || this.lockFlow.busy()) {
      return;
    }
    this.lockError.set(null);
    // Direct-lock (no shares) rejections surface as this row's inline lock error, matching the
    // previous per-row behavior; the dialog path reports its own error via the shared service.
    try {
      await this.lockFlow.requestLock(this.node().id, this.node().name, () => {
        /* tree reload (inside FoldersService.lock) re-renders this row reactively */
      });
    } catch {
      this.lockError.set("Couldn’t change this folder’s lock. Try again.");
    }
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

  // --- Inline rename -------------------------------------------------------
  /** Open the inline rename field (seeded with the current name) + focus it once rendered. */
  startRename(): void {
    this.renameDraft.set(this.node().name);
    this.renaming.set(true);
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
  /** Open the delete-confirm step (triggered by the flat trash icon). */
  startDelete(): void {
    this.actionError.set(null);
    this.confirmingDelete.set(true);
  }

  /** Back out of the delete confirm, back to the normal row. */
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
      this.confirmingDelete.set(false);
      this.actionError.set(null);
      this.toast.success("Folder deleted. Its notes are in All notes.");
      // If this folder was selected, fall back to the vault root.
      if (this.selectedId() === this.node().id) {
        this.selected.emit(null);
      }
    } catch {
      this.actionError.set("Couldn’t delete this folder. Please try again.");
    } finally {
      this.busy.set(false);
    }
  }
}
