import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  afterNextRender,
  computed,
  inject,
  output,
  signal,
  viewChild,
} from "@angular/core";
import { toObservable, toSignal } from "@angular/core/rxjs-interop";
import { Router } from "@angular/router";
import { catchError, debounceTime, from, of, switchMap } from "rxjs";
import { MurKbdComponent } from "../kbd/kbd.component";
import { IpcService } from "../../core/ipc.service";
import type { SearchHit } from "../../core/models";

/** One rendered result row — precomputed so the template calls no methods. */
interface HitRow {
  readonly id: string;
  readonly title: string;
  readonly dateLabel: string;
  readonly snippet: string;
  readonly matchedIn: string;
}

/**
 * PROTOTYPE (Apple TV shell) — the ⌘K quick-search spotlight.
 *
 * A floating command-palette over the whole app: type → `search_meetings` →
 * arrow/Enter to jump to a meeting. With an empty query it offers the quick
 * actions (New note). Per the overlay rule (T3) the PANEL is opaque
 * `--surface-overlay` — only the scrim behind it is translucent.
 *
 * The query→results wire is the sanctioned rxjs bridge: `toObservable(query)`
 * → debounce → `switchMap` (stale results dropped for free) → `toSignal`.
 */
@Component({
  selector: "mur-quick-search",
  standalone: true,
  imports: [MurKbdComponent],
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./quick-search.component.html",
  styleUrl: "./quick-search.component.scss",
})
export class MurQuickSearchComponent {
  private readonly ipc = inject(IpcService);
  private readonly router = inject(Router);

  /** Fired when the palette should close (scrim click, Esc, or navigation). */
  readonly closed = output<void>();

  private readonly input =
    viewChild<ElementRef<HTMLInputElement>>("qsInput");

  /** The live query text. */
  readonly query = signal("");
  /** Raw arrow-key selection index (clamped by `selected`). */
  readonly sel = signal(0);

  /**
   * Debounced query → `search_meetings` results. `switchMap` drops stale
   * in-flight responses when the query changes; errors resolve to no hits.
   */
  private readonly _hits = toSignal(
    toObservable(this.query).pipe(
      debounceTime(160),
      switchMap((q) => {
        const term = q.trim();
        if (!term) return of([] as SearchHit[]);
        return from(this.ipc.searchMeetings(term)).pipe(
          catchError(() => of([] as SearchHit[])),
        );
      }),
    ),
    { initialValue: [] as SearchHit[] },
  );

  /** Results as display rows (title/date preformatted — no template methods). */
  readonly rows = computed<HitRow[]>(() =>
    this._hits().map((h) => ({
      id: h.meeting.id,
      title: h.meeting.title ?? "Untitled meeting",
      dateLabel: new Date(h.meeting.startedAt).toLocaleDateString(undefined, {
        month: "short",
        day: "numeric",
      }),
      snippet: h.snippet,
      matchedIn: h.matchedIn,
    })),
  );

  /** Selection clamped into the current result range (-1 when no results). */
  readonly selected = computed(() => {
    const n = this.rows().length;
    return n === 0 ? -1 : Math.min(this.sel(), n - 1);
  });

  /** True once the user typed a non-blank query. */
  readonly searching = computed(() => this.query().trim().length > 0);

  constructor() {
    // Focus the field as soon as the palette renders (zoneless-safe one-shot).
    afterNextRender(() => this.input()?.nativeElement.focus());
  }

  onQuery(value: string): void {
    this.query.set(value);
    this.sel.set(0);
  }

  move(delta: number): void {
    const n = this.rows().length;
    if (n === 0) return;
    this.sel.set(Math.min(Math.max(this.selected() + delta, 0), n - 1));
  }

  confirm(): void {
    const i = this.selected();
    const row = i >= 0 ? this.rows()[i] : undefined;
    if (row) {
      this.open(row);
    } else if (!this.searching()) {
      this.newNote();
    }
  }

  open(row: HitRow): void {
    void this.router.navigate(["/meeting", row.id]);
    this.closed.emit();
  }

  newNote(): void {
    void this.router.navigate(["/record"]);
    this.closed.emit();
  }

  /** Close only when the click landed on the scrim itself, not the panel. */
  onScrim(e: MouseEvent): void {
    if (e.target === e.currentTarget) this.closed.emit();
  }
}
