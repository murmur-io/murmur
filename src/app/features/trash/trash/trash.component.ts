import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  OnInit,
  computed,
  inject,
  signal,
} from "@angular/core";
import {
  MurIconComponent,
  type ShellIcon,
} from "../../../design-system/icon/icon.component";
import { MurEmptyStateComponent } from "../../../design-system/empty-state/empty-state.component";
import { ToastService } from "../../../services/toast.service";
import { TrashService } from "../../../services/trash.service";
import type { TrashEntry, TrashKind } from "../../../core/models";

/** One row, with everything the template needs already derived. */
interface TrashRowVm {
  entry: TrashEntry;
  /** Icon name for the entry's kind. */
  icon: ShellIcon;
  /** "Recording" / "Note" / "Folder" / "Note folder". */
  kindLabel: string;
  /** "Deleted 2 days ago". */
  deletedLabel: string;
  /** "29 days left" / "Purges today". */
  expiryLabel: string;
  /** True in the final 3 days — the row gets the urgent accent. */
  expiringSoon: boolean;
}

const KIND_META: Record<TrashKind, { icon: ShellIcon; label: string }> = {
  meeting: { icon: "meetings", label: "Recording" },
  note: { icon: "notes", label: "Note" },
  folder: { icon: "spaces", label: "Folder" },
  noteFolder: { icon: "spaces", label: "Note folder" },
};

/** Rows within this many days of purge are flagged. */
const EXPIRING_SOON_DAYS = 3;

const RETENTION_CHOICES = [7, 14, 30, 60, 90] as const;

/**
 * The Trash view — deleted recordings, notes and folders, restorable until their
 * retention window runs out.
 *
 * Masking is the BACKEND's call: a sealed entry arrives `locked: true` with no
 * label or detail, and both Restore and Delete-forever are refused for it. This
 * component renders that state and disables the actions, but never decides it —
 * and never shows a locked row's title, because it does not have one.
 */
@Component({
  selector: "app-trash",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./trash.component.html",
  styleUrl: "./trash.component.scss",
  imports: [MurIconComponent, MurEmptyStateComponent],
})
export class TrashComponent implements OnInit {
  readonly store = inject(TrashService);
  private readonly destroyRef = inject(DestroyRef);
  private readonly toast = inject(ToastService);

  readonly retentionChoices = RETENTION_CHOICES;
  /** Which entry the user is confirming a permanent delete for. */
  readonly confirmingDelete = signal<string | null>(null);
  readonly confirmingEmpty = signal(false);
  readonly retentionOpen = signal(false);

  readonly rows = computed<TrashRowVm[]>(() =>
    this.store.entries().map((entry) => {
      // A `kind` the FE does not know (an older/newer backend) must still render a row rather
      // than throw — T6's lesson: one bad field must not take the whole view down.
      const meta: { icon: ShellIcon; label: string } = KIND_META[entry.kind] ?? {
        icon: "notes",
        label: "Item",
      };
      return {
        entry,
        icon: meta.icon,
        kindLabel: meta.label,
        deletedLabel: this.relativeDeleted(entry.deletedAt),
        expiryLabel: this.expiryLabel(entry.daysLeft),
        expiringSoon: entry.daysLeft <= EXPIRING_SOON_DAYS,
      };
    }),
  );

  /**
   * Gate the spinner on BOTH, per `angular-zoneless.md` §8: a return visit must
   * show the cached rows immediately instead of flashing a spinner over them.
   */
  readonly showSpinner = computed(
    () => this.store.isEmpty() && this.store.loading(),
  );

  readonly retentionLabel = computed(
    () => `Items are kept for ${this.store.retentionDays()} days`,
  );

  ngOnInit(): void {
    // Tell the store a view is mounted, so trash events refresh the ROWS (not just
    // the badge count) while this screen is open — and stop doing so once it is not.
    this.destroyRef.onDestroy(this.store.watch());
    void this.store.reload();
    // Reconcile expired entries on open rather than waiting up to an hour for the
    // background tick — otherwise the view can show a row that is already past its
    // date. Best-effort: the list reload above already happened.
    void this.store.purgeExpiredOnOpen();
  }

  isBusy(entryId: string): boolean {
    return this.store.busyIds().has(entryId);
  }

  async restore(entry: TrashEntry): Promise<void> {
    const error = await this.store.restore(entry.id);
    if (error) {
      this.toast.danger(error);
      return;
    }
    this.toast.success(`Restored “${entry.label}”.`);
  }

  askDelete(entryId: string): void {
    this.confirmingDelete.set(entryId);
  }

  cancelDelete(): void {
    this.confirmingDelete.set(null);
  }

  async confirmDelete(entry: TrashEntry): Promise<void> {
    const error = await this.store.deleteForever(entry.id);
    this.confirmingDelete.set(null);
    if (error) {
      this.toast.danger(error);
      return;
    }
    this.toast.success("Deleted permanently.");
  }

  askEmpty(): void {
    this.confirmingEmpty.set(true);
  }

  cancelEmpty(): void {
    this.confirmingEmpty.set(false);
  }

  async confirmEmpty(): Promise<void> {
    const result = await this.store.emptyAll();
    this.confirmingEmpty.set(false);
    if ("error" in result) {
      this.toast.danger(result.error);
      return;
    }
    const locked = this.store.lockedCount();
    if (locked > 0) {
      // Say what was NOT done. Reporting only the purged count would read as
      // "the trash is empty" while locked entries are still sitting in it.
      this.toast.info(
        `Deleted ${result.purged} item${result.purged === 1 ? "" : "s"}. ` +
          `${locked} locked item${locked === 1 ? "" : "s"} kept — unlock the folder to delete them.`,
      );
      return;
    }
    this.toast.success(
      `Deleted ${result.purged} item${result.purged === 1 ? "" : "s"} permanently.`,
    );
  }

  toggleRetention(): void {
    this.retentionOpen.update((open) => !open);
  }

  async chooseRetention(days: number): Promise<void> {
    this.retentionOpen.set(false);
    const error = await this.store.setRetentionDays(days);
    if (error) {
      this.toast.danger(error);
      return;
    }
    this.toast.success(`Items are now kept for ${days} days.`);
  }

  /** "Deleted today" / "Deleted 3 days ago". */
  private relativeDeleted(iso: string): string {
    const then = new Date(iso).getTime();
    if (Number.isNaN(then)) {
      return "Deleted recently";
    }
    const days = Math.floor((Date.now() - then) / 86_400_000);
    if (days <= 0) {
      return "Deleted today";
    }
    if (days === 1) {
      return "Deleted yesterday";
    }
    return `Deleted ${days} days ago`;
  }

  private expiryLabel(daysLeft: number): string {
    if (daysLeft <= 0) {
      return "Purges today";
    }
    if (daysLeft === 1) {
      return "1 day left";
    }
    return `${daysLeft} days left`;
  }
}
