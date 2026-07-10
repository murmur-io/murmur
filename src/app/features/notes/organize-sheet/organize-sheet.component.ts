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
import type { OrganizeMove, OrganizePlan } from "../../../core/models";

/**
 * Auto-organize REVIEW SHEET — the confirm-before-apply step for the Notes
 * "Auto-organize" action. The backend proposes an {@link OrganizePlan} (per-note
 * target folder + a reason); this sheet presents it, lets the user include /
 * exclude individual moves, and emits the SELECTED moves to apply. Nothing moves
 * until the parent runs `apply_organize_plan` with what `apply` emits.
 *
 * A FLOATING modal (trap T3): it floats OVER the Notes content, so the panel is
 * OPAQUE `var(--surface-overlay)` + `--border-strong` + `--shadow-lg` +
 * `backdrop-filter: none` — never the frosted `.card` (which would bleed the
 * note list through). Mirrors {@link ShareVerifySheetComponent}.
 *
 * Presentational: the parent owns the async `plan_organize_notes` /
 * `apply_organize_plan` calls + the `busy` flag; this sheet only renders the plan
 * and emits `apply` / `cancel`. Selection lives in a signal Set of EXCLUDED
 * noteIds (default: every move included), so `selectedMoves` derives the list.
 */
@Component({
  selector: "app-organize-sheet",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./organize-sheet.component.html",
  styleUrl: "./organize-sheet.component.scss",
})
export class OrganizeSheetComponent {
  private readonly injector = inject(Injector);

  /** The proposed plan (null while the parent is still fetching it). */
  readonly plan = input<OrganizePlan | null>(null);
  /** True while the parent's `apply_organize_plan` call is in flight. */
  readonly busy = input(false);

  /** Apply the SELECTED moves (excluded ones dropped). */
  readonly apply = output<OrganizeMove[]>();
  /**
   * Dismiss the sheet without applying. Named `cancelled` (not `cancel`) because
   * `cancel` is a native DOM event name → the `@angular-eslint/no-output-native`
   * rule bans it as an output name (mirrors `ShareVerifySheetComponent.cancelled`).
   */
  readonly cancelled = output<void>();

  private readonly panel = viewChild<ElementRef<HTMLDivElement>>("panel");

  /** noteIds the user has UN-checked (default all-included → empty set). */
  private readonly excluded = signal<ReadonlySet<string>>(new Set());

  /** The plan's moves (empty when null / empty plan). */
  readonly moves = computed<OrganizeMove[]>(() => this.plan()?.moves ?? []);

  /** True when there are no moves to review (null plan or an empty plan). */
  readonly isEmpty = computed(() => this.moves().length === 0);

  /** The moves currently included (not in the excluded set). */
  readonly selectedMoves = computed(() => {
    const ex = this.excluded();
    return this.moves().filter((m) => !ex.has(m.noteId));
  });

  /** How many moves are selected right now. */
  readonly selectedCount = computed(() => this.selectedMoves().length);

  /**
   * Distinct target folders across the SELECTED moves — drives the
   * "N notes → M folders" summary line so it reflects the live selection.
   */
  readonly targetFolderCount = computed(
    () => new Set(this.selectedMoves().map((m) => m.toFolder)).size,
  );

  /** True when every move is selected (drives the select-all/none control label). */
  readonly allSelected = computed(
    () => !this.isEmpty() && this.excluded().size === 0,
  );

  constructor() {
    // Land focus in the dialog so Escape works + screen readers announce it.
    afterNextRender(() => this.panel()?.nativeElement.focus(), {
      injector: this.injector,
    });
  }

  /** Whether a move is currently included (checkbox state). */
  isIncluded(noteId: string): boolean {
    return !this.excluded().has(noteId);
  }

  /** Toggle one move's inclusion. */
  toggleMove(noteId: string): void {
    this.excluded.update((prev) => {
      const next = new Set(prev);
      if (next.has(noteId)) {
        next.delete(noteId);
      } else {
        next.add(noteId);
      }
      return next;
    });
  }

  /** Select all moves (clear the excluded set) / select none (exclude all). */
  toggleSelectAll(): void {
    if (this.allSelected()) {
      this.excluded.set(new Set(this.moves().map((m) => m.noteId)));
    } else {
      this.excluded.set(new Set());
    }
  }

  /** Emit the selected moves (guarded: no-op when none selected or busy). */
  onApply(): void {
    if (this.busy() || this.selectedCount() === 0) {
      return;
    }
    this.apply.emit(this.selectedMoves());
  }

  /**
   * Backdrop click → cancel, but ONLY when the scrim itself was clicked (not a
   * click that bubbled up from inside the panel). Compares the event target to
   * its currentTarget so an interaction with the sheet never dismisses it.
   */
  onScrimClick(event: MouseEvent): void {
    if (event.target === event.currentTarget) {
      this.cancelled.emit();
    }
  }
}
