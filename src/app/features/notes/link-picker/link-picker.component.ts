import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  effect,
  inject,
  input,
  output,
  signal,
  viewChild,
} from "@angular/core";
import { IpcService } from "../../../core/ipc.service";
import type { NoteCitation } from "../../../core/models";
import { DebounceService } from "../../../services/debounce.service";
import { RepositionOnScrollDirective } from "../note-brain-popover/reposition-on-scroll.directive";

/** How long to wait after the last keystroke before re-querying the backend. */
const QUERY_DEBOUNCE_MS = 150;
/** The debounce-service key — only one picker is ever open at a time. */
const DEBOUNCE_KEY = "link-picker-query";

/**
 * The inline `[[` / slash-menu "Link to note" autocomplete (note-editor Fix 2) —
 * an Obsidian-style fuzzy-filterable popover of candidate note/meeting/org titles,
 * anchored at the caret. Both trigger paths (typing `[[` and picking the slash-menu
 * "Link to note" entry) share this ONE component, per the task's parity requirement.
 *
 * PURE PRESENTATIONAL + CONTROLLED — like Obsidian, the caret STAYS in the editor
 * textarea while this is open (no focus steal): the HOST owns `query()` (derived
 * from the text typed since the trigger) and `activeIndex()` (driven by the SAME
 * textarea keydown handler that already runs the slash menu's ↑/↓/Enter/Esc), and
 * this component only renders the live candidate list + does the debounced fetch.
 * `NoteEditorComponent` owns the actual textarea splice on `picked` (this stays a
 * dumb overlay, mirroring {@link import('../note-selection-toolbar/note-selection-toolbar.component').NoteSelectionToolbarComponent}).
 *
 * Live-filters via {@link IpcService.listLinkCandidates} (debounced through the
 * sanctioned {@link DebounceService} — rule §5, never a bare `setTimeout`), with a
 * stale-result guard (T1 discipline: an effect-orchestrated async fetch keyed on
 * `query()`, dropping a late reply for a superseded query). The resolved
 * `candidates()` is re-exposed via `candidatesChange` so the host's keyboard
 * handler can navigate/pick without duplicating the fetch.
 *
 * OPAQUE overlay (T3): `--surface-overlay`, `--border-strong`, `--shadow-lg`,
 * `backdrop-filter:none` — never the frosted `.card`, since this floats over the
 * editor body. Positioned via `afterNextRender({injector})` at the caret rect the
 * host supplies (no `setTimeout`/`rAF`), re-positioned on scroll/resize via the
 * existing {@link RepositionOnScrollDirective}.
 */
@Component({
  selector: "app-link-picker",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RepositionOnScrollDirective],
  templateUrl: "./link-picker.component.html",
  styleUrl: "./link-picker.component.scss",
})
export class LinkPickerComponent {
  private readonly ipc = inject(IpcService);
  private readonly debounce = inject(DebounceService);
  private readonly injector = inject(Injector);
  private readonly destroyRef = inject(DestroyRef);

  /** The caret's viewport anchor rect — a NEW object re-positions the popover. */
  readonly anchorRect = input.required<{
    top: number;
    left: number;
    right: number;
    bottom: number;
  }>();
  /** The live filter text, owned by the host (the text typed since the trigger). */
  readonly query = input<string>("");
  /** Which row is keyboard-highlighted, owned by the host. */
  readonly activeIndex = input<number>(0);

  /** A candidate was picked (click) — the host inserts `[[title]]` at the trigger position. */
  readonly picked = output<NoteCitation>();
  /** The resolved candidate list for the CURRENT query — the host's ↑/↓/Enter act on this. */
  readonly candidatesChange = output<NoteCitation[]>();

  /** The live candidate rows for the current query. */
  readonly candidates = signal<NoteCitation[]>([]);
  /** True while the (debounced) fetch for the CURRENT query is in flight. */
  readonly loading = computed(() => this._loading());
  private readonly _loading = signal(false);

  private readonly popoverEl = viewChild<ElementRef<HTMLDivElement>>("popover");

  /** Monotonic request token — a late reply for a superseded query is dropped (T1 stale-guard). */
  private requestSeq = 0;

  constructor() {
    // Fetch on every query change (debounced) — a legitimate signal-writing effect
    // (T1): async IPC orchestration keyed on the `query` INPUT, with a stale guard.
    effect(() => {
      const q = this.query();
      this.debounce.schedule(
        DEBOUNCE_KEY,
        () => void this.fetch(q),
        QUERY_DEBOUNCE_MS,
      );
    });
    // Re-position whenever the anchor rect changes (a fresh caret position).
    effect(() => {
      this.anchorRect(); // track
      this.reposition();
    });
    this.destroyRef.onDestroy(() => this.debounce.cancel(DEBOUNCE_KEY));
  }

  private async fetch(q: string): Promise<void> {
    const seq = ++this.requestSeq;
    this._loading.set(true);
    try {
      const rows = await this.ipc.listLinkCandidates(q);
      if (seq !== this.requestSeq) {
        return; // superseded by a newer query.
      }
      this.candidates.set(rows);
      this.candidatesChange.emit(rows);
    } catch {
      if (seq === this.requestSeq) {
        this.candidates.set([]);
        this.candidatesChange.emit([]);
      }
    } finally {
      if (seq === this.requestSeq) {
        this._loading.set(false);
      }
    }
  }

  pick(candidate: NoteCitation): void {
    this.picked.emit(candidate);
  }

  reposition(): void {
    afterNextRender(
      () => {
        const el = this.popoverEl()?.nativeElement;
        if (!el) {
          return;
        }
        const rect = this.anchorRect();
        const width = el.offsetWidth || 300;
        const height = el.offsetHeight;
        const vw = window.innerWidth;
        const vh = window.innerHeight;

        let left = rect.left;
        left = Math.max(8, Math.min(left, vw - width - 8));

        // Prefer BELOW the caret; flip above when there isn't room below.
        let top = rect.bottom + 4;
        if (top + height > vh - 8) {
          top = Math.max(8, rect.top - height - 4);
        }

        el.style.left = `${Math.round(left)}px`;
        el.style.top = `${Math.round(top)}px`;
      },
      { injector: this.injector },
    );
  }
}
