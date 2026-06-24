import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  OnInit,
  computed,
  inject,
  signal,
  viewChild,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import { IpcService } from "../../core/ipc.service";
import type { Meeting, MeetingStatus, SearchHit } from "../../core/models";

/** Debounce window for search-as-you-type — quick enough to feel instant. */
const SEARCH_DEBOUNCE_MS = 180;

/** A snippet split into runs around the query match, for safe <mark>-style emphasis. */
interface SnippetPart {
  text: string;
  hit: boolean;
}

@Component({
  selector: "app-library",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  template: `
    <section class="library">
      <!-- Frosted search field, pinned to the top of the screen -->
      <div class="search" [class.is-active]="searching()">
        <span class="search-icon" aria-hidden="true">
          <svg viewBox="0 0 20 20" width="18" height="18" fill="none">
            <circle
              cx="8.5"
              cy="8.5"
              r="5.5"
              stroke="currentColor"
              stroke-width="1.7"
            />
            <path
              d="m13 13 4 4"
              stroke="currentColor"
              stroke-width="1.7"
              stroke-linecap="round"
            />
          </svg>
        </span>
        <input
          #searchInput
          type="search"
          class="search-input"
          placeholder="Search meetings, transcripts & notes…"
          autocapitalize="off"
          autocomplete="off"
          spellcheck="false"
          aria-label="Search meetings"
          [value]="query()"
          (input)="onQueryInput($event)"
          (keydown.escape)="clear()"
        />
        @if (query()) {
          <button
            type="button"
            class="search-clear"
            aria-label="Clear search"
            (click)="clear()"
          >
            <svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true">
              <path
                d="M4 4l8 8M12 4l-8 8"
                stroke="currentColor"
                stroke-width="1.7"
                stroke-linecap="round"
              />
            </svg>
          </button>
        }
      </div>

      @if (hasQuery()) {
        <!-- ===================== SEARCH RESULTS ===================== -->
        <header class="library-head">
          <h2>Search</h2>
          @if (!searching() && results().length > 0) {
            <span class="count">{{ results().length }}</span>
          }
        </header>

        @if (searching()) {
          <div class="card state-card">
            <p class="empty searching">
              <span class="spinner" aria-hidden="true"></span>
              Searching…
            </p>
          </div>
        } @else if (results().length === 0) {
          <div class="card empty-state">
            <span class="empty-mark" aria-hidden="true"></span>
            <p class="empty-title">No matches for “{{ query().trim() }}”</p>
            <p class="empty">Try a different word or a shorter phrase.</p>
          </div>
        } @else {
          <ul class="list card">
            @for (hit of results(); track hit.meeting.id; let i = $index) {
              <li>
                <a
                  class="row"
                  [routerLink]="['/meeting', hit.meeting.id]"
                  [style.animation-delay.ms]="i * 35"
                >
                  <span class="row-main">
                    <span class="title">{{
                      hit.meeting.title || "(untitled)"
                    }}</span>
                    <span class="meta">
                      <span
                        class="badge"
                        [class]="matchBadgeClass(hit.matchedIn)"
                        >{{ matchLabel(hit.matchedIn) }}</span
                      >
                      <span class="date">{{
                        formatDate(hit.meeting.startedAt)
                      }}</span>
                    </span>
                    @if (hit.snippet) {
                      <span class="snippet">
                        @for (part of snippetParts(hit.snippet); track $index) {
                          @if (part.hit) {
                            <mark class="snippet-hit">{{ part.text }}</mark>
                          } @else {
                            {{ part.text }}
                          }
                        }
                      </span>
                    }
                  </span>
                  <span class="chevron" aria-hidden="true">›</span>
                </a>
              </li>
            }
          </ul>
        }
      } @else {
        <!-- ===================== MEETINGS LIST (no query) ===================== -->
        <header class="library-head">
          <h2>Meetings</h2>
          @if (!loading() && meetings().length > 0) {
            <span class="count">{{ meetings().length }}</span>
          }
        </header>

        @if (loading()) {
          <div class="card state-card">
            <p class="empty">Loading…</p>
          </div>
        } @else if (meetings().length === 0) {
          <div class="card empty-state">
            <span class="empty-mark" aria-hidden="true"></span>
            <p class="empty-title">No meetings yet</p>
            <p class="empty">Record one from the Record tab to see it here.</p>
          </div>
        } @else {
          <ul class="list card">
            @for (m of meetings(); track m.id; let i = $index) {
              <li>
                <a
                  class="row"
                  [routerLink]="['/meeting', m.id]"
                  [style.animation-delay.ms]="i * 45"
                >
                  <span class="row-main">
                    <span class="title">{{ m.title || "(untitled)" }}</span>
                    <span class="meta">
                      <span class="date">{{ formatDate(m.startedAt) }}</span>
                      @if (m.durationS > 0) {
                        <span class="dot" aria-hidden="true">·</span>
                        <span class="duration">{{
                          formatDuration(m.durationS)
                        }}</span>
                      }
                    </span>
                  </span>
                  <span class="row-aside">
                    <span class="pill" [class]="statusPillClass(m.status)">
                      <span class="pill-dot"></span>
                      {{ statusLabel(m.status) }}
                    </span>
                    <span class="chevron" aria-hidden="true">›</span>
                  </span>
                </a>
              </li>
            }
          </ul>
        }
      }
    </section>
  `,
  styles: [
    `
      .library {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
      }

      /* --- Frosted search field (pinned, sticky to the scroll top) --- */
      .search {
        position: sticky;
        top: 0;
        z-index: 5;
        display: flex;
        align-items: center;
        gap: var(--space-2);
        padding: 0 var(--space-3);
        height: 48px;
        border: 1px solid var(--glass-border);
        border-radius: var(--radius-md);
        background: var(--surface-raised);
        -webkit-backdrop-filter: blur(var(--glass-blur))
          saturate(var(--glass-saturate));
        backdrop-filter: blur(var(--glass-blur)) saturate(var(--glass-saturate));
        box-shadow: var(--shadow-sm), var(--glass-highlight);
        transition:
          border-color var(--transition),
          box-shadow var(--transition);
      }
      .search:focus-within {
        border-color: var(--accent);
        box-shadow:
          0 0 0 3px var(--accent-ring),
          var(--glass-highlight);
      }
      .search-icon {
        display: inline-flex;
        flex: none;
        color: var(--text-muted);
        transition: color var(--transition);
      }
      .search:focus-within .search-icon,
      .search.is-active .search-icon {
        color: var(--accent-hover);
      }
      /* Reset the global input chrome — the wrapper carries the frosting. */
      .search-input {
        flex: 1 1 auto;
        min-width: 0;
        height: 100%;
        padding: 0;
        border: none;
        border-radius: 0;
        background: transparent;
        color: var(--text-primary);
        font-size: 0.9375rem;
        letter-spacing: -0.01em;
      }
      .search-input:hover,
      .search-input:focus {
        border: none;
        background: transparent;
        box-shadow: none;
      }
      .search-input::placeholder {
        color: var(--text-muted);
      }
      /* Hide the native WebKit search affordances — we draw our own ✕. */
      .search-input::-webkit-search-decoration,
      .search-input::-webkit-search-cancel-button {
        -webkit-appearance: none;
        appearance: none;
      }
      .search-clear {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        flex: none;
        width: 28px;
        height: 28px;
        padding: 0;
        border: none;
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        color: var(--text-muted);
        cursor: pointer;
        transition:
          background var(--transition),
          color var(--transition),
          transform var(--transition-fast);
      }
      .search-clear:hover {
        background: var(--surface-hover);
        color: var(--text-primary);
      }
      .search-clear:active {
        transform: scale(0.92);
      }
      .search-clear:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }

      .library-head {
        display: flex;
        align-items: center;
        gap: var(--space-3);
      }
      .library-head h2 {
        margin: 0;
      }
      .count {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        min-width: 24px;
        height: 24px;
        padding: 0 var(--space-2);
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        border: 1px solid var(--border);
        color: var(--text-secondary);
        font-size: 0.8125rem;
        font-weight: 600;
        font-variant-numeric: tabular-nums;
        line-height: 1;
      }

      /* --- Meeting / results list --- */
      .list {
        list-style: none;
        padding: var(--space-2);
        margin: 0;
      }
      .row {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-4);
        padding: var(--space-3) var(--space-4);
        border-radius: var(--radius-md);
        text-decoration: none;
        color: inherit;
        animation: rise 360ms var(--transition) both;
        transition:
          background var(--transition),
          transform var(--transition-fast);
      }
      .list li + li {
        border-top: 1px solid var(--border-subtle);
      }
      .row:hover {
        background: var(--surface-hover);
      }
      .row:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .row:active {
        transform: translateY(1px);
      }

      .row-main {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        min-width: 0;
      }
      .title {
        color: var(--text-primary);
        font-weight: 600;
        letter-spacing: -0.01em;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .meta {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        color: var(--text-muted);
        font-size: 0.8125rem;
      }
      .meta .dot {
        color: var(--text-muted);
      }
      .date {
        font-family: var(--font-mono);
        font-variant-numeric: tabular-nums;
        letter-spacing: -0.01em;
      }
      .duration {
        font-family: var(--font-mono);
        font-variant-numeric: tabular-nums;
      }

      .row-aside {
        display: inline-flex;
        align-items: center;
        gap: var(--space-3);
        flex: none;
      }
      .chevron {
        color: var(--text-muted);
        font-size: 1.25rem;
        line-height: 1;
        transition:
          color var(--transition),
          transform var(--transition);
      }
      .row:hover .chevron {
        color: var(--text-secondary);
        transform: translateX(2px);
      }

      /* --- Search result extras: matched-in badge + snippet --- */
      .badge {
        display: inline-flex;
        align-items: center;
        height: 18px;
        padding: 0 var(--space-2);
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        border: 1px solid var(--border);
        color: var(--text-secondary);
        font-size: 0.6875rem;
        font-weight: 600;
        letter-spacing: 0.02em;
        text-transform: uppercase;
        line-height: 1;
        flex: none;
      }
      .badge.is-accent {
        background: var(--accent-soft);
        border-color: transparent;
        color: var(--accent-hover);
      }
      .badge.is-success {
        background: var(--success-soft);
        border-color: transparent;
        color: var(--success);
      }
      .snippet {
        display: -webkit-box;
        -webkit-line-clamp: 2;
        -webkit-box-orient: vertical;
        overflow: hidden;
        color: var(--text-muted);
        font-size: 0.8125rem;
        line-height: 1.5;
      }
      .snippet-hit {
        background: var(--accent-soft);
        color: var(--text-primary);
        border-radius: 3px;
        padding: 0 2px;
      }

      /* --- Empty / loading states --- */
      .state-card {
        padding: var(--space-6);
      }
      .searching {
        display: inline-flex;
        align-items: center;
        gap: var(--space-3);
      }
      .spinner {
        width: 16px;
        height: 16px;
        flex: none;
        border-radius: 50%;
        border: 2px solid var(--border-strong);
        border-top-color: var(--accent-hover);
        animation: spin 700ms linear infinite;
      }
      @keyframes spin {
        to {
          transform: rotate(360deg);
        }
      }
      .empty-state {
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: var(--space-2);
        padding: var(--space-7) var(--space-5);
        text-align: center;
      }
      .empty-mark {
        width: 44px;
        height: 44px;
        margin-bottom: var(--space-2);
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        border: 1px solid var(--border);
      }
      .empty-title {
        margin: 0;
        color: var(--text-primary);
        font-weight: 600;
      }
      .empty {
        margin: 0;
        color: var(--text-muted);
      }
    `,
  ],
})
export class LibraryComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly destroyRef = inject(DestroyRef);

  /** The search box element — focused after a clear. */
  private readonly searchInput =
    viewChild<ElementRef<HTMLInputElement>>("searchInput");

  // --- No-query meetings list (unchanged behaviour) -----------------------
  readonly meetings = signal<Meeting[]>([]);
  readonly loading = signal(true);

  // --- Search state -------------------------------------------------------
  /** Raw, untrimmed query bound to the input. */
  readonly query = signal("");
  /** Latest applied search hits. */
  readonly results = signal<SearchHit[]>([]);
  /** True while an IPC search is in flight (drives the "Searching…" state). */
  readonly searching = signal(false);

  /** Whether the (trimmed) query is non-empty — switches list ↔ results. */
  readonly hasQuery = computed(() => this.query().trim().length > 0);

  /** Tracked so we can cancel a pending debounce on re-trigger / destroy. */
  private searchTimer: ReturnType<typeof setTimeout> | null = null;

  async ngOnInit(): Promise<void> {
    try {
      this.meetings.set(await this.ipc.listMeetings());
    } finally {
      this.loading.set(false);
    }
    // Clean up any in-flight debounce timer when the view is torn down.
    this.destroyRef.onDestroy(() => {
      if (this.searchTimer) {
        clearTimeout(this.searchTimer);
      }
    });
  }

  // --- Search-as-you-type --------------------------------------------------

  /** Mirror the input into the `query` signal, then debounce the search. */
  onQueryInput(event: Event): void {
    this.query.set((event.target as HTMLInputElement).value);
    this.scheduleSearch();
  }

  /**
   * Debounced search dispatch (DestroyRef-tracked timeout — no bare setTimeout
   * lifecycle). An empty/whitespace query clears results immediately; a real
   * query runs after the debounce window.
   */
  private scheduleSearch(): void {
    if (this.searchTimer) {
      clearTimeout(this.searchTimer);
      this.searchTimer = null;
    }

    const q = this.query().trim();
    if (!q) {
      // Empty query: drop any in-flight state and show the meetings list.
      this.searching.set(false);
      this.results.set([]);
      return;
    }

    this.searching.set(true);
    this.searchTimer = setTimeout(() => {
      void this.runSearch(q);
    }, SEARCH_DEBOUNCE_MS);
  }

  /**
   * Execute one search. Latest-query-wins: by the time the promise resolves the
   * user may have typed on, so we only apply results if `q` still matches the
   * current trimmed query — otherwise a slower earlier request can't clobber a
   * newer one.
   */
  private async runSearch(q: string): Promise<void> {
    try {
      const hits = await this.ipc.searchMeetings(q);
      if (this.query().trim() !== q) {
        return; // stale — a newer keystroke superseded this request.
      }
      this.results.set(hits);
    } catch {
      if (this.query().trim() === q) {
        this.results.set([]);
      }
    } finally {
      if (this.query().trim() === q) {
        this.searching.set(false);
      }
    }
  }

  /** Reset the query + results and return focus to the input. */
  clear(): void {
    if (this.searchTimer) {
      clearTimeout(this.searchTimer);
      this.searchTimer = null;
    }
    this.query.set("");
    this.results.set([]);
    this.searching.set(false);
    this.searchInput()?.nativeElement.focus();
  }

  // --- Snippet highlighting (no innerHTML / DomSanitizer) ------------------

  /**
   * Split a snippet into runs around case-insensitive matches of the current
   * query, so the template can wrap matched runs in a styled <mark> element.
   * Returns a single non-hit run when the query doesn't occur in the snippet.
   */
  snippetParts(snippet: string): SnippetPart[] {
    const q = this.query().trim();
    if (!q) {
      return [{ text: snippet, hit: false }];
    }
    const parts: SnippetPart[] = [];
    const haystack = snippet.toLowerCase();
    const needle = q.toLowerCase();
    let from = 0;
    let at = haystack.indexOf(needle, from);
    while (at !== -1) {
      if (at > from) {
        parts.push({ text: snippet.slice(from, at), hit: false });
      }
      parts.push({ text: snippet.slice(at, at + needle.length), hit: true });
      from = at + needle.length;
      at = haystack.indexOf(needle, from);
    }
    if (from < snippet.length) {
      parts.push({ text: snippet.slice(from), hit: false });
    }
    return parts;
  }

  /** Human label for the field a hit matched in. */
  matchLabel(matchedIn: string): string {
    switch (matchedIn) {
      case "transcript":
        return "in transcript";
      case "note":
        return "in note";
      default:
        return "title";
    }
  }

  /** Tint the matched-in badge: transcript/note = accent, title = neutral. */
  matchBadgeClass(matchedIn: string): string {
    switch (matchedIn) {
      case "transcript":
        return "is-accent";
      case "note":
        return "is-success";
      default:
        return "";
    }
  }

  statusLabel(s: string): string {
    return s.charAt(0) + s.slice(1).toLowerCase();
  }

  /** Maps a meeting status to a status-pill state modifier (matches Record). */
  statusPillClass(s: MeetingStatus): string {
    switch (s) {
      case "RECORDING":
      case "ERROR":
        return "is-danger";
      case "TRANSCRIBED":
      case "SUMMARIZED":
        return "is-accent";
      case "EXPORTED":
        return "is-success";
      default:
        return "";
    }
  }

  /** Presentational only: render the stored timestamp as a friendly local date. */
  formatDate(startedAt: string): string {
    const d = new Date(startedAt);
    if (Number.isNaN(d.getTime())) {
      return startedAt;
    }
    return d.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
  }

  /** Presentational only: seconds → compact "Hh Mm" / "Mm Ss" / "Ss" duration. */
  formatDuration(durationS: number): string {
    const total = Math.max(0, Math.round(durationS));
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const s = total % 60;
    if (h > 0) {
      return `${h}h ${m}m`;
    }
    if (m > 0) {
      return `${m}m ${s}s`;
    }
    return `${s}s`;
  }
}
