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
import { IpcService } from "./core/ipc.service";
import type { SearchHit } from "./core/models";

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
  selector: "app-quick-search",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div
      class="qs-scrim"
      role="dialog"
      aria-modal="true"
      aria-label="Quick search"
      (click)="onScrim($event)"
      (keydown.escape)="closed.emit()"
    >
      <div class="qs-panel">
        <div class="qs-input-row">
          <svg class="qs-glass" viewBox="0 0 20 20" fill="none" aria-hidden="true">
            <circle cx="9" cy="9" r="5.2" stroke="currentColor" stroke-width="1.5" />
            <path d="m13.2 13.2 3.3 3.3" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" />
          </svg>
          <input
            #qsInput
            class="qs-input"
            type="text"
            placeholder="Search meetings, notes, transcripts…"
            autocomplete="off"
            spellcheck="false"
            [value]="query()"
            (input)="onQuery($any($event.target).value)"
            (keydown.arrowDown)="move(1); $event.preventDefault()"
            (keydown.arrowUp)="move(-1); $event.preventDefault()"
            (keydown.enter)="confirm()"
          />
          <kbd class="qs-kbd">esc</kbd>
        </div>

        @if (!searching()) {
          <div class="qs-actions">
            <button type="button" class="qs-row qs-action" (click)="newNote()">
              <span class="qs-row-icon" aria-hidden="true">
                <svg viewBox="0 0 20 20" fill="none">
                  <path d="M10 4.5v11M4.5 10h11" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
                </svg>
              </span>
              <span class="qs-row-title">New note</span>
              <kbd class="qs-kbd">⌘N</kbd>
            </button>
          </div>
          <p class="qs-hint">Type to search your vault — everything stays on device.</p>
        } @else {
          <div class="qs-results" role="listbox" aria-label="Search results">
            @for (row of rows(); track row.id; let i = $index) {
              <button
                type="button"
                class="qs-row"
                role="option"
                [attr.aria-selected]="i === selected()"
                [class.selected]="i === selected()"
                (mouseenter)="sel.set(i)"
                (click)="open(row)"
              >
                <span class="qs-row-icon" aria-hidden="true">
                  <svg viewBox="0 0 20 20" fill="none">
                    <rect x="3.25" y="3.75" width="13.5" height="12.5" rx="2.2" stroke="currentColor" stroke-width="1.4" />
                    <path d="M6.5 7.5h7M6.5 10.5h7M6.5 13.25h4" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" />
                  </svg>
                </span>
                <span class="qs-row-main">
                  <span class="qs-row-title">{{ row.title }}</span>
                  <span class="qs-row-snippet">{{ row.snippet }}</span>
                </span>
                <span class="qs-row-meta">
                  <span class="qs-chip">{{ row.matchedIn }}</span>
                  <span class="qs-date">{{ row.dateLabel }}</span>
                </span>
              </button>
            } @empty {
              <p class="qs-hint">No matches in the vault.</p>
            }
          </div>
        }
      </div>
    </div>
  `,
  styles: [
    `
      /* Scrim: the ONLY translucent layer — dims + slightly blurs the app. */
      .qs-scrim {
        position: fixed;
        inset: 0;
        z-index: 70; /* above the toast viewport (60) */
        display: flex;
        justify-content: center;
        align-items: flex-start;
        padding: 14vh var(--space-4) var(--space-4);
        background: rgba(0, 0, 0, 0.42);
        -webkit-backdrop-filter: blur(6px);
        backdrop-filter: blur(6px);
      }
      /* Panel: OPAQUE overlay surface (rule T3 — never the frosted .card). */
      .qs-panel {
        width: min(580px, 100%);
        max-height: 62vh;
        display: flex;
        flex-direction: column;
        overflow: hidden;
        border: 1px solid var(--border-strong);
        border-radius: var(--radius-xl);
        background: var(--surface-overlay);
        backdrop-filter: none;
        box-shadow: var(--shadow-lg), var(--glass-highlight);
      }
      .qs-input-row {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        padding: var(--space-4) var(--space-4) var(--space-3);
        border-bottom: 1px solid var(--border-subtle);
      }
      .qs-glass {
        flex: none;
        width: 20px;
        height: 20px;
        color: var(--text-muted);
      }
      .qs-input {
        flex: 1 1 auto;
        min-width: 0;
        border: none;
        outline: none;
        background: transparent;
        color: var(--text-primary);
        font-family: inherit;
        font-size: 1.05rem;
        letter-spacing: -0.01em;
      }
      .qs-input::placeholder { color: var(--text-muted); }
      .qs-kbd {
        flex: none;
        padding: 2px 7px;
        border: 1px solid var(--border-subtle);
        border-radius: 6px;
        background: var(--surface-input);
        color: var(--text-muted);
        font-family: var(--font-mono);
        font-size: 0.68rem;
        line-height: 1.5;
      }
      .qs-actions { padding: var(--space-2); }
      .qs-results {
        flex: 1 1 auto;
        min-height: 0;
        overflow-y: auto;
        padding: var(--space-2);
        display: flex;
        flex-direction: column;
        gap: 2px;
      }
      .qs-row {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        width: 100%;
        padding: var(--space-2) var(--space-3);
        border: none;
        border-radius: var(--radius-md);
        background: transparent;
        color: var(--text-secondary);
        font-family: inherit;
        font-size: 0.9rem;
        text-align: left;
        cursor: pointer;
        transition: background var(--transition-fast), color var(--transition-fast);
      }
      .qs-row:hover,
      .qs-row.selected {
        background: var(--accent-soft);
        color: var(--text-primary);
      }
      .qs-row:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .qs-row-icon {
        display: grid;
        place-items: center;
        flex: none;
        width: 20px;
        height: 20px;
        color: var(--text-muted);
      }
      .qs-row-icon svg { width: 18px; height: 18px; display: block; }
      .qs-row.selected .qs-row-icon,
      .qs-row:hover .qs-row-icon { color: var(--accent-hover); }
      .qs-row-main {
        flex: 1 1 auto;
        min-width: 0;
        display: flex;
        flex-direction: column;
        gap: 1px;
      }
      .qs-row-title {
        font-weight: 600;
        color: var(--text-primary);
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
      }
      .qs-row-snippet {
        font-size: 0.8rem;
        color: var(--text-muted);
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
      }
      .qs-row-meta {
        flex: none;
        display: flex;
        align-items: center;
        gap: var(--space-2);
      }
      .qs-chip {
        padding: 1px 8px;
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
        color: var(--text-muted);
        font-size: 0.68rem;
        font-weight: 600;
        letter-spacing: 0.02em;
        text-transform: uppercase;
      }
      .qs-date {
        color: var(--text-muted);
        font-size: 0.78rem;
        font-variant-numeric: tabular-nums;
      }
      .qs-action .qs-row-title { color: var(--text-primary); }
      .qs-hint {
        margin: 0;
        padding: var(--space-3) var(--space-4) var(--space-4);
        color: var(--text-muted);
        font-size: 0.82rem;
      }
    `,
  ],
})
export class QuickSearchComponent {
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
