import {
  ChangeDetectionStrategy,
  Component,
  type ElementRef,
  Injector,
  OnInit,
  afterNextRender,
  computed,
  inject,
  signal,
  viewChild,
} from "@angular/core";
import { FormControl, ReactiveFormsModule } from "@angular/forms";
import { toObservable, toSignal } from "@angular/core/rxjs-interop";
import { EMPTY, interval, startWith, switchMap } from "rxjs";
import { IpcService } from "../../../core/ipc.service";
import type { AppLogEntry, AppLogSession } from "../../../core/models";
import { MurEmptyStateComponent } from "../../../design-system/empty-state/empty-state.component";
import { MurRowMenuComponent } from "../../../design-system/row-menu/row-menu.component";
import {
  MurSegmentedComponent,
  type SegmentOption,
} from "../../../design-system/segmented/segmented.component";
import { MurSpinnerComponent } from "../../../design-system/spinner/spinner.component";
import { ToastService } from "../../../services/toast.service";
import { LogsStore } from "../logs.store";

/** The level buckets the filter offers. `all` is not a level — it clears the filter. */
type LevelFilter = "all" | "error" | "warn" | "info" | "debug";

/** One `key=value` pair `tracing` appended after an event's message. */
interface LogField {
  readonly key: string;
  readonly value: string;
}

/** One rendered row — an entry with its presentation already derived. */
interface LogRow {
  readonly seq: number;
  /** Local wall-clock, e.g. `14:02:11.483`. */
  readonly time: string;
  /** Local date + time, shown in the expanded detail. */
  readonly fullTime: string;
  /** The timestamp as written (UTC). */
  readonly timestamp: string | null;
  readonly level: string;
  readonly levelClass: string;
  readonly target: string;
  /** The whole message, fields included — the collapsed row's last column. */
  readonly message: string;
  /** The message WITHOUT its trailing `key=value` fields. */
  readonly text: string;
  /** Those fields, split out for the detail panel. */
  readonly fields: readonly LogField[];
  /** The entry exactly as it appears in the file. */
  readonly raw: string;
  /** `aria-controls` target for the disclosure. */
  readonly detailId: string;
}

/** How many entries one read asks for. The backend clamps its own ceiling. */
const WINDOW = 1000;

/** Auto-refresh cadence while the toggle is on. */
const REFRESH_MS = 3000;

/**
 * Developer mode → Logs: what the app recorded, formatted to be read rather
 * than scrolled past — one row per event with its local time, level, target and
 * message, filterable by level and free text, over either the current session
 * or the previous one (where a crash's last words live).
 *
 * The raw file is `tracing`'s own line format; the parsing into entries happens
 * in Rust (`applog`), so continuation lines (a wrapped panic payload) arrive
 * already folded into the event they belong to instead of as orphan rows.
 *
 * Nothing here reads meeting content: the log carries IDs, stages, counts and
 * durations only, so there is no lock gate to route through and nothing to mask.
 */
@Component({
  selector: "app-logs",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    ReactiveFormsModule,
    MurRowMenuComponent,
    MurSegmentedComponent,
    MurSpinnerComponent,
    MurEmptyStateComponent,
  ],
  templateUrl: "./logs.component.html",
  styleUrl: "./logs.component.scss",
})
export class LogsComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly toast = inject(ToastService);
  private readonly injector = inject(Injector);
  /** Root-persisted so returning to this route shows the last rows instantly. */
  private readonly store = inject(LogsStore);

  readonly log = this.store.log;
  readonly loading = this.store.loading;
  readonly error = this.store.error;
  readonly session = this.store.session;

  /**
   * Short labels on purpose: the switcher shares one bar with five level chips
   * and the text filter, and "This session"/"Previous session" alone cost the
   * ~120px that pushed the row into a second and third line. The group's
   * `ariaLabel` supplies the noun the labels drop.
   */
  readonly sessionOptions: readonly SegmentOption[] = [
    { value: "current", label: "Current" },
    { value: "previous", label: "Previous" },
  ];

  readonly levelFilter = signal<LevelFilter>("all");

  /** Auto-refresh while watching a live reproduction. Off by default. */
  readonly autoRefresh = signal(false);

  /** Free-text filter over the message + target. */
  readonly searchControl = new FormControl("", { nonNullable: true });
  private readonly _search = toSignal(
    this.searchControl.valueChanges.pipe(startWith("")),
    { initialValue: "" },
  );
  readonly searchQuery = computed(() => this._search().trim());

  /**
   * The auto-refresh cadence, as an rxjs interval bridged through `toSignal` —
   * the sanctioned shape for a repeating tick (§3/§5: never a hand-rolled
   * `setInterval` in a component). `toSignal` owns the subscription, so the
   * polling stops with the view; flipping the toggle off switches the source to
   * EMPTY, which cancels the interval without tearing anything down.
   */
  private readonly _autoRefreshTick = toSignal(
    toObservable(this.autoRefresh).pipe(
      switchMap((on) => (on ? interval(REFRESH_MS) : EMPTY)),
      switchMap(() => this.reload(true)),
    ),
    { initialValue: undefined },
  );

  /** The scroll container, so a load can pin the newest entry into view. */
  private readonly listRef = viewChild<ElementRef<HTMLElement>>("list");

  ngOnInit(): void {
    void this.reload();
  }

  /** Every entry in the loaded window (newest last), or `[]` before the first read. */
  readonly entries = computed<readonly AppLogEntry[]>(
    () => this.log()?.entries ?? [],
  );

  /** Count per level bucket — the filter chips show how much they hide. */
  readonly counts = computed(() => {
    const counts = { all: 0, error: 0, warn: 0, info: 0, debug: 0 };
    for (const entry of this.entries()) {
      counts.all += 1;
      const bucket = levelBucket(entry.level);
      if (bucket) {
        counts[bucket] += 1;
      }
    }
    return counts;
  });

  /**
   * The rows actually rendered: level bucket ∩ text query, already FORMATTED.
   * The presentation (local time, level pill class) is derived here rather than
   * by template method calls, which would re-run for every row on every change
   * detection pass (§2: derive with `computed`, never recompute in the view).
   */
  readonly visibleEntries = computed<readonly LogRow[]>(() => {
    const level = this.levelFilter();
    const query = this.searchQuery().toLowerCase();
    return this.entries()
      .filter((entry) => {
        if (level !== "all" && levelBucket(entry.level) !== level) {
          return false;
        }
        if (!query) {
          return true;
        }
        return (
          entry.message.toLowerCase().includes(query) ||
          entry.target.toLowerCase().includes(query)
        );
      })
      .map((entry) => {
        const split = splitFields(entry.message);
        return {
          seq: entry.seq,
          time: formatTime(entry.timestamp),
          fullTime: formatFullTime(entry.timestamp),
          timestamp: entry.timestamp,
          level: entry.level.toUpperCase(),
          levelClass: `is-${levelBucket(entry.level) ?? "other"}`,
          target: entry.target,
          message: entry.message,
          text: split.text,
          fields: split.fields,
          raw: entry.raw,
          detailId: `log-entry-${entry.seq}`,
        };
      });
  });

  /**
   * Which rows are expanded, by `seq`. A Set rather than a single id: comparing
   * two entries means having both open at once, and there is no reason to make
   * that a mode.
   */
  private readonly _expanded = signal<ReadonlySet<number>>(new Set());
  readonly expandedSeqs = this._expanded.asReadonly();

  /** True when there is nothing cached to show — gates the spinner (§8). */
  readonly listEmpty = computed(() => this.entries().length === 0);

  /** Every row is filtered out, but the log itself is not empty. */
  readonly filteredToNothing = computed(
    () => !this.listEmpty() && this.visibleEntries().length === 0,
  );

  /** The generation exists but holds no events yet (a just-cleared log). */
  readonly emptyFile = computed(() => {
    const log = this.log();
    return !!log && log.exists && log.entries.length === 0;
  });

  /** The previous-session file was never written (first ever launch). */
  readonly missingFile = computed(() => {
    const log = this.log();
    return !!log && !log.exists;
  });

  /** "312 KB" — the size of the WHOLE file, not of the loaded window. */
  readonly sizeLabel = computed(() => {
    const bytes = this.log()?.sizeBytes ?? 0;
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${Math.round(bytes / 1024)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  });

  /** Switch generation (segmented control) and load it. */
  selectSession(value: string): void {
    const session: AppLogSession = value === "previous" ? "previous" : "current";
    if (session === this.session()) {
      return;
    }
    this.session.set(session);
    void this.reload();
  }

  setLevel(level: LevelFilter): void {
    this.levelFilter.set(level);
  }

  /** Open/close one entry's detail. */
  toggleExpanded(seq: number): void {
    this._expanded.update((open) => {
      const next = new Set(open);
      if (!next.delete(seq)) {
        next.add(seq);
      }
      return next;
    });
  }

  /** Copy ONE entry exactly as the file has it. */
  async copyEntry(row: LogRow): Promise<void> {
    try {
      await navigator.clipboard.writeText(row.raw);
      this.toast.success("Entry copied");
    } catch {
      this.toast.danger("Couldn’t copy the entry.");
    }
  }

  /**
   * The menu items. `<mur-row-menu>` closes itself after any enabled
   * `[role=menuitem]`, so these only own the action — including the
   * auto-refresh toggle, whose state is reported afterwards by the header's
   * "Auto" pill rather than by a menu the user would have to dismiss.
   */
  chooseAutoRefresh(): void {
    this.autoRefresh.update((on) => !on);
  }

  chooseRefresh(): void {
    void this.reload();
  }

  chooseCopy(): void {
    void this.copyVisible();
  }

  chooseReveal(): void {
    void this.reveal();
  }

  chooseClear(): void {
    void this.clear();
  }

  /**
   * Fetch the selected generation. `quiet` (the auto-refresh tick) skips the
   * loading flag so the view does not flicker every few seconds.
   */
  async reload(quiet = false): Promise<void> {
    if (!quiet) {
      this.loading.set(true);
    }
    const requested = this.session();
    try {
      const log = await this.ipc.readAppLog(requested, WINDOW);
      // Stale-result guard: the user may have switched generation mid-flight,
      // and a late response must never overwrite the newer one.
      if (requested !== this.session()) {
        return;
      }
      this.store.log.set(log);
      this.error.set(null);
      // `seq` is a POSITION in the window, so it means something different after
      // every read — carrying the open set across would expand whichever entries
      // happen to land on those indices next.
      this._expanded.set(new Set());
      this.scrollToNewest();
    } catch (error) {
      if (requested !== this.session()) {
        return;
      }
      this.error.set(
        typeof error === "string" ? error : "Couldn’t read the log file.",
      );
    } finally {
      if (!quiet) {
        this.loading.set(false);
      }
    }
  }

  /** Copy the visible rows as plain text — what you paste into a bug report. */
  async copyVisible(): Promise<void> {
    const text = this.visibleEntries()
      .map((row) => row.raw)
      .join("\n");
    if (!text) {
      return;
    }
    try {
      await navigator.clipboard.writeText(text);
      this.toast.success("Log copied");
    } catch {
      this.toast.danger("Couldn’t copy the log.");
    }
  }

  /** Open the log folder in Finder so the raw file can be attached to a report. */
  async reveal(): Promise<void> {
    try {
      await this.ipc.revealAppLog();
    } catch {
      this.toast.danger("Couldn’t open the log folder.");
    }
  }

  /**
   * Empty the CURRENT session's file. Offered only on `current` — the previous
   * generation is the evidence this view exists to preserve.
   */
  async clear(): Promise<void> {
    try {
      await this.ipc.clearAppLog();
      await this.reload();
      this.toast.success("Log cleared");
    } catch {
      this.toast.danger("Couldn’t clear the log.");
    }
  }

  /**
   * Keep the newest entry in view after a load. `afterNextRender` (not
   * `setTimeout`) is the zoneless-safe hook, and the injector is passed because
   * this is called from an async handler, outside the construction context.
   */
  private scrollToNewest(): void {
    afterNextRender(
      () => {
        const list = this.listRef()?.nativeElement;
        if (list) {
          list.scrollTop = list.scrollHeight;
        }
      },
      { injector: this.injector },
    );
  }
}

/**
 * The trailing `key=value` fields `tracing` writes after an event's message.
 *
 * Matched only as a SUFFIX, deliberately: fields always come last in the writer's format, and
 * scanning the whole string would tear apart a message that merely contains an `=` (a path, a
 * query, a serialized struct). A message with no trailing pairs comes back unchanged.
 */
function splitFields(message: string): { text: string; fields: LogField[] } {
  const suffix = /(?:\s+[A-Za-z_][\w.]*=(?:"(?:[^"\\]|\\.)*"|[^\s"]+))+$/.exec(message);
  if (!suffix) {
    return { text: message, fields: [] };
  }
  const fields: LogField[] = [];
  const pair = /([A-Za-z_][\w.]*)=("(?:[^"\\]|\\.)*"|[^\s"]+)/g;
  let match: RegExpExecArray | null;
  while ((match = pair.exec(suffix[0])) !== null) {
    const raw = match[2];
    // Unwrap the writer's quoting so the detail panel shows the VALUE, not its escaping.
    const value =
      raw.startsWith('"') && raw.endsWith('"')
        ? raw.slice(1, -1).replace(/\\(.)/g, "$1")
        : raw;
    fields.push({ key: match[1], value });
  }
  return { text: message.slice(0, suffix.index).trimEnd(), fields };
}

/** Local date + wall-clock for the expanded detail (`2026-09-01 14:02:11.483`). */
function formatFullTime(timestamp: string | null): string {
  if (!timestamp) {
    return "—";
  }
  const date = new Date(timestamp);
  if (Number.isNaN(date.getTime())) {
    return timestamp;
  }
  const pad = (n: number, width = 2) => String(n).padStart(width, "0");
  return (
    `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ` +
    formatTime(timestamp)
  );
}

/** Local wall-clock (`14:02:11.483`) — the log writes its timestamps in UTC. */
function formatTime(timestamp: string | null): string {
  if (!timestamp) {
    return "—";
  }
  const date = new Date(timestamp);
  if (Number.isNaN(date.getTime())) {
    return timestamp;
  }
  const pad = (n: number, width = 2) => String(n).padStart(width, "0");
  return (
    `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}` +
    `.${pad(date.getMilliseconds(), 3)}`
  );
}

/**
 * Map a raw level to a filter bucket. `DEBUG` and `TRACE` share one bucket (a
 * user chasing a bug wants "the noisy stuff", not two chips), and an
 * unrecognised level — including the `OTHER` a header-less fragment gets —
 * belongs to no bucket, so it shows only under "All".
 */
function levelBucket(level: string): Exclude<LevelFilter, "all"> | null {
  switch (level.toUpperCase()) {
    case "ERROR":
      return "error";
    case "WARN":
      return "warn";
    case "INFO":
      return "info";
    case "DEBUG":
    case "TRACE":
      return "debug";
    default:
      return null;
  }
}
