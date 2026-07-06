import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  input,
  output,
  signal,
} from "@angular/core";
import { FoldersService } from "../../../services/folders.service";
import { ToastService } from "../../../services/toast.service";
import type { FolderNode } from "../../../core/models";
import { LockBadgeComponent } from "../lock-badge/lock-badge.component";

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
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [LockBadgeComponent],
  templateUrl: "./move-to-menu.component.html",
  styleUrl: "./move-to-menu.component.scss",
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
  readonly closed = output<void>();

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
        // Defensive: tolerate a node without a `children` array.
        if (node.children?.length) {
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
      this.closed.emit();
    } catch {
      this.moveError.set("Couldn’t move this note. Please try again.");
    } finally {
      this.moving.set(false);
    }
  }
}
