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
import { FoldersService } from "../../../services/folders.service";
import { ToastService } from "../../../services/toast.service";
import type { FolderNode } from "../../../core/models";
import { FolderRowComponent } from "../folder-row/folder-row.component";

/**
 * The recursive folder tree. Renders `nodes` as {@link FolderRowComponent}s; the
 * recursion is by child component — a row renders its children through ANOTHER
 * `app-folder-tree`, never a nested `@for` of rows. This component is reused at
 * every depth (the row passes `depth + 1`).
 *
 * At the ROOT (depth 0) it also owns an inline "New folder" create affordance
 * (text field + confirm — the entry TRIGGER is `<mur-sidebar-section>`'s "+"
 * icon now, forwarded via `openCreate()`). Focus is moved into the field with
 * `afterNextRender` (no `setTimeout`); creation delegates to
 * {@link FoldersService.create}, which reloads the tree. The "Vault root"
 * pseudo-row (2026-07-12: MOVED to `<mur-sidebar-section>`'s "All meetings"
 * root row, alongside Notes' identical "All notes" — see that component)
 * used to live here too.
 *
 * Selection bubbles up via the `select` output (a real folder id only now —
 * the root/`null` case is handled by `<mur-sidebar-section>`'s own
 * `rootSelect` output instead).
 */
@Component({
  selector: "app-folder-tree",
  changeDetection: ChangeDetectionStrategy.OnPush,
  // `folder-tree` ↔ `folder-row` are mutually recursive standalone components,
  // so their ES modules form an import cycle. Referencing `FolderRowComponent`
  // directly here is `undefined` at metadata-evaluation time and Angular then
  // throws `getComponentDef(undefined)` (reading 'ɵcmp') the moment the `@for`
  // below instantiates an `app-folder-row` — exactly the "view breaks after
  // adding the first folder" bug. `forwardRef` defers the lookup, breaking the
  // cycle. (See folder-row for the mirror.)
  imports: [forwardRef(() => FolderRowComponent)],
  templateUrl: "./folder-tree.component.html",
  styleUrl: "./folder-tree.component.scss",
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
  /**
   * Whether `selectedId` should render as visually SELECTED (the pill) —
   * default `true` (unchanged for any other consumer). `MeetingsSidebarTree
   * Component` passes `false` while `/library`/`/meeting/:id` isn't the
   * current route (2026-07-12 fix), so a remembered last-selected folder
   * doesn't misleadingly show as "active" while looking at a different page.
   * Threaded through recursively to `FolderRowComponent` → nested
   * `app-folder-tree` levels, same as `depth`.
   */
  readonly selectionActive = input<boolean>(true);

  /** Bubbles the chosen folder id (or null for the vault root) to the screen. */
  readonly selected = output<string | null>();

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

  /**
   * Open the inline create field and move focus into it once it has rendered.
   * Public so `MeetingsSidebarTreeComponent` can forward the main sidebar's
   * compact "+" icon (next to the "Meetings" section header,
   * `AppShellComponent`, 2026-07-12) into this call — the tree no longer
   * renders its own entry-trigger button (was a full-width dashed row that
   * read as duplicate UI next to Notes' identical one).
   */
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
      this.selected.emit(folder.id);
    } catch {
      this.toast.danger("Couldn’t create that folder. Try another name.");
    } finally {
      this.saving.set(false);
    }
  }
}
