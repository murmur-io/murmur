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
  model,
  signal,
  viewChild,
} from "@angular/core";
import { IpcService } from "../../core/ipc.service";
import type { LinkKind, NoteCitation, SourceRef } from "../../core/models";
import { DebounceService } from "../../services/debounce.service";
import { RepositionOnScrollDirective } from "../../features/notes/note-brain-popover/reposition-on-scroll.directive";
import { TeleportToBodyDirective } from "../teleport-to-body.directive";

/** How long to wait after the last keystroke before re-querying the backend. */
const QUERY_DEBOUNCE_MS = 150;
/** The debounce-service key — one source-picker popover open at a time per instance. */
const DEBOUNCE_KEY = "source-picker-query";
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

/** The `NoteCitation.kind`s the picker offers — the Brain scopes to these only. */
const ALLOWED_KINDS: ReadonlySet<string> = new Set<LinkKind>([
  "meeting",
  "note",
  "document",
]);

/**
 * Design System — `<mur-source-picker>`: the "scope the Brain to these sources"
 * chip multiselect. A small trigger opens an OPAQUE popover (T3) with a
 * self-focused search over {@link IpcService.listLinkCandidates} — the SAME
 * paginated candidate feed the `[[` link picker walks — filtered to
 * note/meeting/document (person/entity/org rows are dropped, since a Brain scope
 * is a document scope). Picking a row ADDS a {@link SourceRef} to `selected()`
 * (deduped by `kind + id`) and keeps the popover open (multiselect); the picked
 * sources render as removable chips.
 *
 * REUSES the link-picker mechanics wholesale — the debounced fetch (sanctioned
 * {@link DebounceService}, never a bare `setTimeout`), the monotonic `requestSeq`
 * stale-guard (a late reply for a superseded query is dropped), the `hasMore`/
 * `loadingMore` infinite-scroll paging deduped by `kind + id`, the ↑/↓ tail
 * look-ahead, and {@link RepositionOnScrollDirective} for scroll/resize
 * re-anchoring. The one shape difference: this component OWNS its `query` (its
 * own `<input>`, unlike the link picker whose caret stays in the editor), so the
 * fetch effect keys on an internal `_query` signal instead of a host input.
 *
 * The popover is positioned in JS via `afterNextRender({injector})` under the
 * trigger — never a `setTimeout`/`rAF` (rule §5).
 */
@Component({
  selector: "mur-source-picker",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RepositionOnScrollDirective, TeleportToBodyDirective],
  templateUrl: "./source-picker.component.html",
  styleUrl: "./source-picker.component.scss",
})
export class SourcePickerComponent {
  private readonly ipc = inject(IpcService);
  private readonly debounce = inject(DebounceService);
  private readonly injector = inject(Injector);
  private readonly destroyRef = inject(DestroyRef);

  /** Two-way: the picked sources rendered as removable chips. */
  readonly selected = model<SourceRef[]>([]);
  /** Placeholder for the popover search input. */
  readonly placeholder = input("Add a note or meeting…");
  /** Label on the trigger button. */
  readonly triggerLabel = input("+ Source");

  /** Whether the popover is open. */
  readonly open = signal(false);
  /** The live search text (owned here — this component has its own input). */
  readonly query = signal("");
  /** The live candidate rows for the current query (already kind-filtered). */
  readonly candidates = signal<NoteCitation[]>([]);
  /** Keyboard-highlighted row index. */
  readonly activeIndex = signal(0);
  /** True while the (debounced) fetch for the CURRENT query is in flight. */
  readonly loading = computed(() => this._loading());
  private readonly _loading = signal(false);

  /** Fast membership set of already-picked sources, keyed `kind + id`. */
  private readonly selectedKeys = computed(
    () => new Set(this.selected().map((s) => s.kind + s.id)),
  );

  /** Rows annotated with whether they are already picked (no template method). */
  readonly rows = computed(() =>
    this.candidates().map((c) => ({
      candidate: c,
      picked: this.selectedKeys().has(c.kind + c.id),
    })),
  );

  /** How many selected-source chips to show before collapsing the rest behind
   * "+N more" — a large Brain scope otherwise floods the panel and pushes the ask
   * input off-screen (user report, 2026-07-19). */
  private static readonly CHIP_COLLAPSE_AFTER = 3;
  /** Chips start COLLAPSED so a wide scope reads as a compact preview, not a wall. */
  private readonly _chipsExpanded = signal(false);
  readonly chipsExpanded = this._chipsExpanded.asReadonly();
  /** The chips actually rendered: the first few when collapsed, all when expanded. */
  readonly visibleChips = computed(() => {
    const all = this.selected();
    return this._chipsExpanded() ||
      all.length <= SourcePickerComponent.CHIP_COLLAPSE_AFTER
      ? all
      : all.slice(0, SourcePickerComponent.CHIP_COLLAPSE_AFTER);
  });
  /** How many chips are hidden behind "+N more" (0 when expanded or already short). */
  readonly hiddenChipCount = computed(
    () => this.selected().length - this.visibleChips().length,
  );
  /** Whether the collapse toggle should appear at all. */
  readonly canCollapseChips = computed(
    () => this.selected().length > SourcePickerComponent.CHIP_COLLAPSE_AFTER,
  );

  /** Expand / collapse the selected-source chips. */
  toggleChips(): void {
    this._chipsExpanded.update((v) => !v);
  }

  private readonly triggerEl = viewChild<ElementRef<HTMLButtonElement>>("trigger");
  private readonly popoverEl = viewChild<ElementRef<HTMLDivElement>>("popover");
  private readonly searchEl = viewChild<ElementRef<HTMLInputElement>>("search");
  private readonly listEl = viewChild<ElementRef<HTMLDivElement>>("list");

  /** Monotonic request token — a late reply for a superseded query is dropped. */
  private requestSeq = 0;
  /** True when the last page came back full, so another page may exist. */
  private hasMore = false;
  /** Re-entrancy guard: one append fetch at a time. */
  private loadingMore = false;

  constructor() {
    // Fetch on every query change while open (debounced) — a legitimate
    // signal-writing effect (T1): async IPC orchestration keyed on `query()`,
    // with a `requestSeq` stale guard. Gated on `open()` so a closed picker
    // isn't querying.
    effect(() => {
      if (!this.open()) {
        return;
      }
      const q = this.query();
      this.debounce.schedule(
        DEBOUNCE_KEY,
        () => void this.fetch(q),
        QUERY_DEBOUNCE_MS,
      );
    });
    // Keyboard nav over a paginated list: keep the active row scrolled into
    // view and pull the next page once ↑/↓ nears the tail of what's loaded.
    // `afterNextRender` so the `.is-active` class from THIS pass is painted
    // before we scroll to it.
    effect(() => {
      const idx = this.activeIndex();
      const total = this.candidates().length;
      if (this.open() && total > 0 && idx >= total - KEYBOARD_LOAD_AHEAD_ROWS) {
        void this.loadMore();
      }
      afterNextRender(
        () => {
          this.listEl()
            ?.nativeElement.querySelector(".sp-row.is-active")
            ?.scrollIntoView({ block: "nearest" });
        },
        { injector: this.injector },
      );
    });
    this.destroyRef.onDestroy(() => this.debounce.cancel(DEBOUNCE_KEY));
  }

  // --- Popover open/close ---------------------------------------------------

  toggle(): void {
    if (this.open()) {
      this.close();
    } else {
      this.openPopover();
    }
  }

  private openPopover(): void {
    this.open.set(true);
    this.query.set("");
    this.candidates.set([]);
    this.activeIndex.set(0);
    // Focus the search field + first fetch (empty prefix → recent candidates)
    // once the popover renders. afterNextRender is the zoneless-safe one-shot;
    // the injector is required outside field-init context.
    afterNextRender(
      () => {
        this.searchEl()?.nativeElement.focus();
      },
      { injector: this.injector },
    );
    void this.fetch("");
    this.reposition();
  }

  close(): void {
    this.open.set(false);
    this.debounce.cancel(DEBOUNCE_KEY);
    // Return focus to the trigger for keyboard users.
    afterNextRender(
      () => {
        this.triggerEl()?.nativeElement.focus();
      },
      { injector: this.injector },
    );
  }

  // --- Search input + keyboard nav -----------------------------------------

  onQuery(value: string): void {
    this.query.set(value);
    this.activeIndex.set(0);
  }

  onKeydown(event: KeyboardEvent): void {
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        this.move(1);
        break;
      case "ArrowUp":
        event.preventDefault();
        this.move(-1);
        break;
      case "Enter": {
        event.preventDefault();
        const row = this.candidates()[this.activeIndex()];
        if (row) {
          this.pick(row);
        }
        break;
      }
      case "Escape":
        event.preventDefault();
        this.close();
        break;
      default:
        break;
    }
  }

  private move(delta: number): void {
    const n = this.candidates().length;
    if (n === 0) {
      return;
    }
    const next = Math.min(Math.max(this.activeIndex() + delta, 0), n - 1);
    this.activeIndex.set(next);
  }

  // --- Fetch / paginate (reused from the link picker) ----------------------

  private async fetch(q: string): Promise<void> {
    const seq = ++this.requestSeq;
    this._loading.set(true);
    try {
      const rows = await this.ipc.listLinkCandidates(q, 0, PAGE_SIZE);
      if (seq !== this.requestSeq) {
        return; // superseded by a newer query.
      }
      const kept = rows.filter((c) => ALLOWED_KINDS.has(c.kind));
      // `hasMore` tracks the RAW backend page (unfiltered) — a full page means
      // more rows exist upstream even if this page's kept rows are few.
      this.hasMore = rows.length === PAGE_SIZE;
      this.candidates.set(kept);
      this.activeIndex.set(0);
      this.reposition();
    } catch {
      if (seq === this.requestSeq) {
        this.hasMore = false;
        this.candidates.set([]);
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
   * Pages by RAW offset (unfiltered backend rows loaded so far), so dropped
   * person/entity/org rows never desync the offset from the backend's ordering.
   */
  private async loadMore(): Promise<void> {
    if (!this.hasMore || this.loadingMore || this._loading()) {
      return;
    }
    const seq = this.requestSeq;
    this.loadingMore = true;
    try {
      const page = await this.ipc.listLinkCandidates(
        this.query(),
        this.rawOffset,
        PAGE_SIZE,
      );
      if (seq !== this.requestSeq) {
        return; // a newer query reset the list while this page was in flight.
      }
      this.rawOffset += page.length;
      this.hasMore = page.length === PAGE_SIZE;
      const kept = page.filter((c) => ALLOWED_KINDS.has(c.kind));
      // Dedupe on append: a row that shifted pages mid-scroll must not repeat —
      // the template's `track c.kind + c.id` requires unique keys.
      const seen = new Set(this.candidates().map((c) => c.kind + c.id));
      const fresh = kept.filter((c) => !seen.has(c.kind + c.id));
      if (fresh.length > 0) {
        this.candidates.set([...this.candidates(), ...fresh]);
        this.reposition();
      }
    } catch {
      if (seq === this.requestSeq) {
        // Keep what's loaded; stop probing so a failing backend isn't hammered.
        this.hasMore = false;
      }
    } finally {
      this.loadingMore = false;
    }
  }

  /**
   * RAW backend offset (rows requested so far, unfiltered). `fetch` resets it to
   * page 0's length; `loadMore` advances by each raw page length. Kept separate
   * from `candidates().length` because kept rows < raw rows once kinds are
   * dropped. A private field mirrors `requestSeq`/`hasMore` (never templated).
   */
  private rawOffset = PAGE_SIZE;

  /** Infinite scroll: fetch the next page when nearing the bottom of the list. */
  onScroll(): void {
    const el = this.listEl()?.nativeElement;
    if (!el) {
      return;
    }
    const nearBottom =
      el.scrollTop + el.clientHeight >= el.scrollHeight - SCROLL_LOAD_THRESHOLD_PX;
    if (nearBottom) {
      void this.loadMore();
    }
  }

  // --- Selection ------------------------------------------------------------

  pick(candidate: NoteCitation): void {
    const kind = candidate.kind as LinkKind;
    if (!ALLOWED_KINDS.has(kind)) {
      return;
    }
    const key = candidate.kind + candidate.id;
    if (this.selectedKeys().has(key)) {
      return; // already picked — multiselect dedupes by kind + id.
    }
    const ref: SourceRef = { kind, id: candidate.id, title: candidate.title };
    this.selected.update((list) => [...list, ref]);
    // Keep the popover open (multiselect) and refocus the search for the next add.
    afterNextRender(
      () => {
        this.searchEl()?.nativeElement.focus();
      },
      { injector: this.injector },
    );
  }

  remove(ref: SourceRef): void {
    this.selected.update((list) =>
      list.filter((s) => !(s.kind === ref.kind && s.id === ref.id)),
    );
  }

  // --- Positioning (reused from the link picker) ---------------------------

  reposition(): void {
    afterNextRender(
      () => {
        const el = this.popoverEl()?.nativeElement;
        const anchor = this.triggerEl()?.nativeElement;
        if (!el || !anchor) {
          return;
        }
        const rect = anchor.getBoundingClientRect();
        const width = el.offsetWidth || 300;
        const height = el.offsetHeight;
        const vw = window.innerWidth;
        const vh = window.innerHeight;

        let left = rect.left;
        left = Math.max(8, Math.min(left, vw - width - 8));

        // Prefer BELOW the trigger; flip above when there isn't room below.
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
