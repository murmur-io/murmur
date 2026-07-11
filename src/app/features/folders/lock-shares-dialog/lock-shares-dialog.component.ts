import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  afterNextRender,
  computed,
  input,
  output,
  viewChild,
} from "@angular/core";
import type { ActiveSharesReport } from "../../../core/models";

/**
 * The lock×shares blocking dialog (Shared Brain v1). Shown BEFORE `lock_folder`
 * when the folder still has active shares (link / user / org). A FLOATING overlay
 * over the tree → OPAQUE `var(--surface-overlay)` + `backdrop-filter: none` +
 * strong border + `--shadow-lg` (trap T3), never the frosted `.card`.
 *
 * Three choices:
 *  - Revoke & lock — revoke EVERY share, then lock (the DEFAULT when org shares
 *    exist, since org copies are the loudest egress).
 *  - Lock anyway — lock while leaving the shares live (the ciphertext stays
 *    reachable by anyone who already has a link / synced the org item).
 *  - Cancel — do nothing.
 *
 * Presentational: the parent owns the async revoke/lock calls + `busy`; this
 * dialog only renders the report and emits `revokeAndLock` / `lockAnyway` /
 * `cancelled`. It states honestly that org copies already synced by colleagues
 * may persist even after a revoke.
 */
@Component({
  selector: "app-lock-shares-dialog",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./lock-shares-dialog.component.html",
  styleUrl: "./lock-shares-dialog.component.scss",
})
export class LockSharesDialogComponent {
  /** The folder name (shown in the copy). */
  readonly folderName = input.required<string>();
  /** The active-shares report gathered before the lock. */
  readonly report = input.required<ActiveSharesReport>();
  /** True while the parent's revoke/lock call is in flight. */
  readonly busy = input(false);
  /**
   * True when the active-shares PROBE itself failed (fail-closed, F5): the report is
   * all-zero but we can't be sure the folder has no shares, so the dialog warns the
   * user to decide explicitly instead of the app silently locking a possibly-shared
   * folder. Swaps the copy to the "couldn't check shares" message.
   */
  readonly probeFailed = input(false);

  /** Revoke every share, then lock (default when org shares exist). */
  readonly revokeAndLock = output<void>();
  /** Lock while leaving the shares live. */
  readonly lockAnyway = output<void>();
  /** Dismiss — do nothing. */
  readonly cancelled = output<void>();

  private readonly panel = viewChild<ElementRef<HTMLDivElement>>("panel");

  /** Total 1:1 (link + user) shares. */
  readonly oneToOneCount = computed(
    () => this.report().links + this.report().users,
  );
  /** Org-brain share count. */
  readonly orgCount = computed(() => this.report().org.length);
  /** Grand total of active shares. */
  readonly totalCount = computed(() => this.oneToOneCount() + this.orgCount());

  /** Org shares are the loudest egress → Revoke & lock is the default when > 0. */
  readonly revokeIsDefault = computed(() => this.orgCount() > 0);

  constructor() {
    // Land focus in the dialog so Escape works + it's announced.
    afterNextRender(() => this.panel()?.nativeElement.focus());
  }
}
