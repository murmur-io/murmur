import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  effect,
  inject,
  input,
  output,
  viewChild,
} from "@angular/core";
import type { PopoverSelection } from "../note-brain-popover/note-brain-popover.component";
import { RepositionOnScrollDirective } from "../note-brain-popover/reposition-on-scroll.directive";
import { TeleportToBodyDirective } from "../../../design-system/teleport-to-body.directive";
import type { FormatOp } from "../note-editor/note-editor.component";

/** Viewport gap kept between the bubble and the selection rect. */
const BAR_GAP = 8;
/** Fallback width used before the bar has laid out (for the first clamp). */
const BAR_WIDTH = 480;

/**
 * The floating selection toolbar (bubble menu). On a non-empty body selection the
 * editor floats this ABOVE the selection with the formatting controls + an accent
 * "Ask Brain" button — a ClickUp/Notion-style bubble that REPLACES the old
 * always-modal behavior (selecting text no longer pops the Brain modal; the modal
 * opens only when the AI button is pressed).
 *
 * It only formats: every op is emitted as a {@link FormatOp} back to the editor,
 * which owns the textarea + the actual markdown transform (so the bubble stays a
 * dumb presentational overlay). The AI button emits `askBrain`; the host then
 * mounts the {@link import('../note-brain-popover/note-brain-popover.component').NoteBrainPopoverComponent}.
 *
 * OPAQUE overlay (T3): `--surface-overlay`, `--border-strong`, `--shadow-lg`,
 * `backdrop-filter:none` — never the frosted `.card`. Positioned via
 * `afterNextRender({injector})` (no `setTimeout`/`rAF`), re-positioned on any
 * scroll/resize via {@link RepositionOnScrollDirective} (DestroyRef-cleaned).
 * `mousedown` is prevented on the bar so clicking a button never blurs the
 * textarea / collapses the selection the ops act on.
 */
@Component({
  selector: "app-note-selection-toolbar",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RepositionOnScrollDirective, TeleportToBodyDirective],
  templateUrl: "./note-selection-toolbar.component.html",
  styleUrl: "./note-selection-toolbar.component.scss",
})
export class NoteSelectionToolbarComponent {
  private readonly injector = inject(Injector);

  /** The live selection + anchor rect. A new object re-positions the bar. */
  readonly selection = input.required<PopoverSelection>();
  /**
   * Whether to show the accent "Ask Brain" button — true when at least one
   * note-assistant action is enabled in Settings. Formatting is ALWAYS available;
   * only the AI entry point is gated.
   */
  readonly showAi = input<boolean>(true);

  /** A formatting op the editor applies to the textarea selection. */
  readonly format = output<FormatOp>();
  /** The AI button — the host opens the Brain popover for the current selection. */
  readonly askBrain = output<void>();

  private readonly barEl = viewChild<ElementRef<HTMLDivElement>>("bar");

  constructor() {
    // Re-position whenever a NEW selection arrives (a fresh rect). Positioning is
    // DOM-after-render work — afterNextRender({injector}), mirroring the popover.
    effect(() => {
      this.selection(); // track
      this.reposition();
    });
  }

  /** Emit a formatting op (template helper — keeps the markup terse). */
  emit(op: FormatOp): void {
    this.format.emit(op);
  }

  /** Position the bar ABOVE the selection rect (flips below when there's no room). */
  reposition(): void {
    afterNextRender(
      () => {
        const el = this.barEl()?.nativeElement;
        if (!el) {
          return;
        }
        const rect = this.selection().rect;
        const width = el.offsetWidth || BAR_WIDTH;
        const height = el.offsetHeight;
        const vw = window.innerWidth;
        const vh = window.innerHeight;

        // Horizontally center on the selection, clamped to the viewport.
        const anchorCenter = (rect.left + rect.right) / 2;
        let left = anchorCenter - width / 2;
        left = Math.max(BAR_GAP, Math.min(left, vw - width - BAR_GAP));

        // Prefer ABOVE; flip below when there isn't room above the selection.
        let top = rect.top - height - BAR_GAP;
        if (top < BAR_GAP) {
          top = Math.min(rect.bottom + BAR_GAP, vh - height - BAR_GAP);
        }

        el.style.left = `${Math.round(left)}px`;
        el.style.top = `${Math.round(Math.max(BAR_GAP, top))}px`;
      },
      { injector: this.injector },
    );
  }
}
