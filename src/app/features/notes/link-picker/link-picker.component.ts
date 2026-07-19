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
import { TeleportToBodyDirective } from "../../../design-system/teleport-to-body.directive";

/** How long to wait after the last keystroke before re-querying the backend. */
const QUERY_DEBOUNCE_MS = 150;
/** The debounce-service key — only one picker is ever open at a time. */
const DEBOUNCE_KEY = "link-picker-query";
/**
 * One backend page of the infinite scroll. A page that comes back exactly this
 * size means more may exist; a shorter page means the list ran dry. The backend
 * clamps whatever it is asked for to its own ceiling (100).
 */
const PAGE_SIZE = 40;
/** Load the next page once the scroll position is within this many px of the bottom. */
const SCROLL_LOAD_THRESHOLD_PX = 56;
/** Keyboard ↑/↓: pull the next page once the active row is within this many rows of the end. */
const KEYBOARD_LOAD_AHEAD_ROWS = 3;

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
 * INFINITE SCROLL (2026-07-17): the backend paginates (`offset`/`limit`), so the
 * popover walks the WHOLE visible vault instead of a fixed top-8 — the query
 * fetch loads page 0 and each scroll-near-bottom (or ↑/↓ reaching the tail of
 * what's loaded) appends the next page, deduped by `kind+id` (the `@for` track
 * key must stay unique even if a row shifts pages mid-scroll). A query change
 * resets the accumulation via the same `requestSeq` stale-guard.
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
  imports: [RepositionOnScrollDirective, TeleportToBodyDirective],
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
  /**
   * The ANCHOR item this picker links FROM — excluded from its own candidate list so a user can
   * never pick the item they're already on (`list_link_candidates` is anchor-agnostic and would
   * otherwise surface the current meeting/note in its own list, where picking it hit the self-link
   * guard with a misleading toast). `null` (the default) excludes nothing.
   */
  readonly excludeKind = input<string | null>(null);
  readonly excludeId = input<string | null>(null);

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
  /**
   * True when the last page came back full, so another page may exist. Plain
   * private fields (same precedent as {@link requestSeq}) — neither is read by
   * the template; they only guard the fetch orchestration.
   */
  private hasMore = false;
  /** Re-entrancy guard: one append fetch at a time. */
  private loadingMore = false;

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
    // Keyboard nav over a paginated list: keep the active row scrolled into
    // view (the host moves `activeIndex` without focus ever entering the
    // popover), and pull the next page once ↑/↓ nears the tail of what's
    // loaded. `afterNextRender` so the `.is-active` class from THIS change-
    // detection pass is painted before we scroll to it.
    effect(() => {
      const idx = this.activeIndex();
      const total = this.candidates().length;
      if (total > 0 && idx >= total - KEYBOARD_LOAD_AHEAD_ROWS) {
        void this.loadMore();
      }
      afterNextRender(
        () => {
          this.popoverEl()
            ?.nativeElement.querySelector(".link-pop-row.is-active")
            ?.scrollIntoView({ block: "nearest" });
        },
        { injector: this.injector },
      );
    });
    this.destroyRef.onDestroy(() => this.debounce.cancel(DEBOUNCE_KEY));
  }

  /** Drop the anchor item (the thing this picker links FROM) so it can't be picked as its own target. */
  private withoutAnchor(rows: NoteCitation[]): NoteCitation[] {
    const k = this.excludeKind();
    const i = this.excludeId();
    if (!k || !i) {
      return rows;
    }
    return rows.filter((r) => !(r.kind === k && r.id === i));
  }

  private async fetch(q: string): Promise<void> {
    const seq = ++this.requestSeq;
    this._loading.set(true);
    try {
      const raw = await this.ipc.listLinkCandidates(q, 0, PAGE_SIZE);
      if (seq !== this.requestSeq) {
        return; // superseded by a newer query.
      }
      // `hasMore` keys on the RAW page length (the backend paginates the unfiltered list); the
      // anchor is dropped only from what we display, so a full raw page still means "more to load".
      this.hasMore = raw.length === PAGE_SIZE;
      const rows = this.withoutAnchor(raw);
      this.candidates.set(rows);
      this.candidatesChange.emit(rows);
      // The list height changed (fresh page) — re-fit around the caret so a
      // flipped-above popover never grows down over the line being typed.
      this.reposition();
    } catch {
      if (seq === this.requestSeq) {
        this.hasMore = false;
        this.candidates.set([]);
        this.candidatesChange.emit([]);
      }
    } finally {
      if (seq === this.requestSeq) {
        this._loading.set(false);
      }
    }
  }

  /**
   * Append the next backend page (scroll neared the bottom, or ↑/↓ neared the
   * tail of the loaded rows). Guarded by the SAME `requestSeq` as {@link fetch}:
   * a query change mid-flight bumps it and the stale append is dropped whole.
   */
  private async loadMore(): Promise<void> {
    if (!this.hasMore || this.loadingMore || this._loading()) {
      return;
    }
    const seq = this.requestSeq;
    this.loadingMore = true;
    try {
      const rawPage = await this.ipc.listLinkCandidates(
        this.query(),
        this.candidates().length,
        PAGE_SIZE,
      );
      if (seq !== this.requestSeq) {
        return; // a newer query reset the list while this page was in flight.
      }
      this.hasMore = rawPage.length === PAGE_SIZE;
      const page = this.withoutAnchor(rawPage);
      // Dedupe on append: a row that shifted pages mid-scroll (e.g. a title
      // edit reordered recency) must not repeat — the template's
      // `track c.kind + c.id` requires unique keys.
      const seen = new Set(this.candidates().map((c) => c.kind + c.id));
      const fresh = page.filter((c) => !seen.has(c.kind + c.id));
      if (fresh.length > 0) {
        const next = [...this.candidates(), ...fresh];
        this.candidates.set(next);
        this.candidatesChange.emit(next);
        this.reposition();
      }
    } catch {
      if (seq === this.requestSeq) {
        // Keep what's loaded; stop probing so a failing backend isn't hammered
        // on every scroll tick. The next query change resets the flow.
        this.hasMore = false;
      }
    } finally {
      this.loadingMore = false;
    }
  }

  /** Infinite scroll: fetch the next page when nearing the bottom of the popover. */
  onScroll(): void {
    const el = this.popoverEl()?.nativeElement;
    if (!el) {
      return;
    }
    const nearBottom =
      el.scrollTop + el.clientHeight >= el.scrollHeight - SCROLL_LOAD_THRESHOLD_PX;
    if (nearBottom) {
      void this.loadMore();
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
