import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
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
import {
  catchError,
  combineLatest,
  debounceTime,
  from,
  of,
  switchMap,
} from "rxjs";
import { MurKbdComponent } from "../kbd/kbd.component";
import { IpcService } from "../../core/ipc.service";
import type { AskVaultResult, SearchHit } from "../../core/models";
import { matchedInLabel } from "../../core/copy/labels";
import { TabsService } from "../../core/tabs.service";
import { MarkdownComponent } from "../../shared/markdown/markdown.component";
import { SourcesComponent } from "../../shared/sources/sources.component";
import { ErrorCopyService } from "../../core/copy/error-copy.service";
import { AskHistoryPrivacyBarrierService } from "../../core/ask-history-privacy-barrier.service";

type QuickSearchMode = "search" | "brain";

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
  imports: [MurKbdComponent, MarkdownComponent, SourcesComponent],
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./quick-search.component.html",
  styleUrl: "./quick-search.component.scss",
})
export class MurQuickSearchComponent {
  private readonly ipc = inject(IpcService);
  private readonly router = inject(Router);
  private readonly tabs = inject(TabsService);
  private readonly errorCopy = inject(ErrorCopyService);
  private readonly destroyRef = inject(DestroyRef);
  private readonly historyPrivacy = inject(AskHistoryPrivacyBarrierService);

  /** Fired when the palette should close (scrim click, Esc, or navigation). */
  readonly closed = output<void>();

  private readonly input =
    viewChild<ElementRef<HTMLInputElement>>("qsInput");

  /** The live query text. */
  readonly query = signal("");
  readonly mode = signal<QuickSearchMode>("search");
  /** Raw arrow-key selection index (clamped by `selected`). */
  readonly sel = signal(0);

  /**
   * Debounced query → `search_meetings` results. `switchMap` drops stale
   * in-flight responses when the query changes; errors resolve to no hits.
   */
  private readonly _hits = toSignal(
    combineLatest([toObservable(this.query), toObservable(this.mode)]).pipe(
      debounceTime(160),
      switchMap(([q, mode]) => {
        const term = q.trim();
        if (!term || mode !== "search") return of([] as SearchHit[]);
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
      // The RAW value ("transcript"/"note"/"title") used to render straight into the chip;
      // `matchedInLabel` is the ONE user-facing phrasing for that concept (P3). It returns "" for
      // a value it cannot name, and the template then renders NO chip — defaulting an unknown
      // match to "in the title" would be a confident lie about where the hit came from.
      matchedIn: matchedInLabel(h.matchedIn),
    })),
  );

  /** Selection clamped into the current result range (-1 when no results). */
  readonly selected = computed(() => {
    const n = this.rows().length;
    return n === 0 ? -1 : Math.min(this.sel(), n - 1);
  });

  /** True once the user typed a non-blank query. */
  readonly searching = computed(() => this.query().trim().length > 0);
  readonly placeholder = computed(() =>
    this.mode() === "brain"
      ? "Ask Brain about any meeting or note…"
      : "Search meetings, notes, transcripts…",
  );
  readonly brainPending = signal(false);
  readonly brainResult = signal<AskVaultResult | null>(null);
  readonly brainError = signal<string | null>(null);
  readonly brainPrivacyReady = this.historyPrivacy.ready;
  readonly brainPrivacyError = this.historyPrivacy.error;
  readonly brainCitations = computed(() => [
    ...new Set(this.brainResult()?.citations ?? []),
  ]);
  private brainRequestSequence = 0;
  private lastBrainQuestion = "";
  private removeHistoryInvalidator: (() => void) | null = null;

  constructor() {
    this.removeHistoryInvalidator = this.historyPrivacy.registerInvalidator(
      () => this.invalidateBrainResult(),
    );
    void this.historyPrivacy.ensureReady();
    this.destroyRef.onDestroy(() => {
      this.brainRequestSequence += 1;
      this.removeHistoryInvalidator?.();
      this.removeHistoryInvalidator = null;
    });
    // Focus the field as soon as the palette renders (zoneless-safe one-shot).
    afterNextRender(() => this.input()?.nativeElement.focus());
  }

  onQuery(value: string): void {
    this.query.set(value);
    this.sel.set(0);
    if (this.mode() === "brain") {
      this.brainError.set(null);
    }
  }

  setMode(mode: QuickSearchMode): void {
    if (this.mode() === mode) {
      return;
    }
    this.mode.set(mode);
    this.sel.set(0);
    this.brainRequestSequence += 1;
    this.brainPending.set(false);
    this.brainError.set(null);
    this.brainResult.set(null);
    this.input()?.nativeElement.focus();
  }

  move(delta: number): void {
    const n = this.rows().length;
    if (n === 0) return;
    this.sel.set(Math.min(Math.max(this.selected() + delta, 0), n - 1));
  }

  confirm(): void {
    if (this.mode() === "brain") {
      void this.askBrain();
      return;
    }
    const i = this.selected();
    const row = i >= 0 ? this.rows()[i] : undefined;
    if (row) {
      this.open(row);
    } else if (!this.searching()) {
      this.newNote();
    }
  }

  open(row: HitRow): void {
    void this.tabs.openMeeting(row.id, row.title);
    this.closed.emit();
  }

  newNote(): void {
    void this.router.navigate(["/notes/new"]);
    this.closed.emit();
  }

  async askBrain(question = this.query().trim()): Promise<void> {
    if (!question || this.brainPending() || !this.brainPrivacyReady()) {
      return;
    }
    const sequence = ++this.brainRequestSequence;
    this.lastBrainQuestion = question;
    this.brainPending.set(true);
    this.brainError.set(null);
    this.brainResult.set(null);
    try {
      const result = await this.ipc.askVault(
        question,
        [],
        crypto.randomUUID(),
      );
      if (sequence === this.brainRequestSequence && this.mode() === "brain") {
        this.brainResult.set(result);
      }
    } catch (error) {
      if (sequence === this.brainRequestSequence && this.mode() === "brain") {
        this.brainError.set(this.errorCopy.because("Couldn’t ask Brain", error));
      }
    } finally {
      if (sequence === this.brainRequestSequence) {
        this.brainPending.set(false);
      }
    }
  }

  retryBrain(): void {
    void this.askBrain(this.lastBrainQuestion);
  }

  retryBrainPrivacy(): void {
    this.invalidateBrainResult();
    void this.historyPrivacy.ensureReady();
  }

  private invalidateBrainResult(): void {
    this.brainRequestSequence += 1;
    this.brainPending.set(false);
    this.brainResult.set(null);
    this.brainError.set(null);
  }

  /** Close only when the click landed on the scrim itself, not the panel. */
  onScrim(e: MouseEvent): void {
    if (e.target === e.currentTarget) this.closed.emit();
  }
}
