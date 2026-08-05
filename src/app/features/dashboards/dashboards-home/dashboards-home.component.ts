import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  effect,
  inject,
  signal,
  untracked,
  viewChild,
} from "@angular/core";
import { Router } from "@angular/router";
import {
  DashboardsService,
  splitLeadingEmoji,
} from "../../../services/dashboards.service";
import { MurEmptyStateComponent } from "../../../design-system/empty-state/empty-state.component";
import { MurIconComponent } from "../../../design-system/icon/icon.component";
import { BoardCardComponent } from "../board-card/board-card.component";
import type { DashboardSummary } from "../../../core/models";

/**
 * `/dashboards` — the boards list.
 *
 * A sibling of Notes / Meetings / Reminders, and a LIST route, so it follows
 * angular-zoneless.md §8 to the letter: the rows live in the root
 * {@link DashboardsService} (they survive this component's destroy+recreate),
 * and the "Loading…" branch is gated on `listEmpty() && loading()` so a return
 * visit paints the cached boards instantly instead of flashing a spinner.
 */
@Component({
  selector: "app-dashboards-home",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurEmptyStateComponent, MurIconComponent, BoardCardComponent],
  templateUrl: "./dashboards-home.component.html",
  styleUrl: "./dashboards-home.component.scss",
})
export class DashboardsHomeComponent {
  private readonly service = inject(DashboardsService);
  private readonly router = inject(Router);
  private readonly injector = inject(Injector);

  readonly boards = this.service.boards;
  readonly loading = this.service.loading;
  readonly error = this.service.error;
  readonly pinned = this.service.pinned;
  readonly unpinned = this.service.unpinned;
  readonly listEmpty = computed(() => this.boards().length === 0);

  /** The inline "new board" composer, so creating never leaves the page. */
  readonly composing = signal(false);
  readonly draftTitle = signal("");
  readonly busy = signal(false);

  private readonly titleField =
    viewChild<ElementRef<HTMLInputElement>>("titleField");

  /**
   * Reload on every mount. The cached rows stay on screen while it runs (§8),
   * and the destroy+recreate cycle is what guarantees a board created elsewhere
   * shows up on return without a bespoke "detect reactivation" hook.
   */
  private readonly _load = effect(() => {
    // untracked: `load()` runs synchronously up to its first await; keeping the
    // effect free of any dependency it also writes is what stops a reload loop
    // (see the sibling comment in DashboardViewComponent).
    untracked(() => void this.service.load());
  });

  open(board: DashboardSummary): void {
    void this.router.navigate(["/dashboards", board.id]);
  }

  startCompose(): void {
    this.draftTitle.set("");
    this.composing.set(true);
    // Zoneless-safe focus: afterNextRender with an explicit injector, because
    // this runs from a click handler (outside the field-init injection context).
    afterNextRender(() => this.titleField()?.nativeElement.focus(), {
      injector: this.injector,
    });
  }

  cancelCompose(): void {
    this.composing.set(false);
  }

  onDraftInput(event: Event): void {
    this.draftTitle.set((event.target as HTMLInputElement).value);
  }

  async createBoard(): Promise<void> {
    const typed = this.draftTitle().trim();
    if (!typed || this.busy()) return;
    this.busy.set(true);
    // "🚀 Atlas GA" names the board AND gives it its emoji — see `splitLeadingEmoji`.
    const { emoji, title } = splitLeadingEmoji(typed);
    const created = await this.service.create(title, emoji);
    this.busy.set(false);
    this.composing.set(false);
    if (created) void this.router.navigate(["/dashboards", created.id]);
  }

  async togglePin(board: DashboardSummary): Promise<void> {
    await this.service.update(board.id, { pinned: !board.pinned });
  }

  async remove(board: DashboardSummary): Promise<void> {
    await this.service.remove(board.id);
  }

  dismissError(): void {
    this.service.clearError();
  }
}
