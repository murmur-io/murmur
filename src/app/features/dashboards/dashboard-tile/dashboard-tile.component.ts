import { ChangeDetectionStrategy, Component, computed, input, output } from "@angular/core";
import type { ResolvedTile, SourceRef, TileData } from "../../../core/models";

/** What the tile header shows when the user gave the tile no title of its own. */
const DEFAULT_TITLE: Record<TileData["kind"], string> = {
  locked: "🔒 Locked",
  missing: "Source removed",
  unconfigured: "Not configured",
  note: "Note",
  meeting: "Recording",
  document: "Document",
  person: "Person",
  reminders: "Reminders",
  drift: "Drift lane",
  numbers: "Numbers",
  pulse: "Pulse",
  promises: "Promises",
  livingAnswer: "Living answer",
};

/** The kind label under the heading — the tile TYPE, always shown. */
const KIND_LABEL: Record<TileData["kind"], string> = {
  locked: "sealed source",
  missing: "missing source",
  unconfigured: "needs a source",
  note: "note",
  meeting: "recording",
  document: "document",
  person: "person",
  reminders: "reminders",
  drift: "drift lane",
  numbers: "numbers",
  pulse: "pulse",
  promises: "promise ledger",
  livingAnswer: "living answer",
};

/** Kinds that re-derive themselves from what was said — flagged "live" in the UI. */
const LIVE_KINDS = new Set<TileData["kind"]>([
  "drift",
  "numbers",
  "pulse",
  "promises",
  "livingAnswer",
]);

/**
 * ONE tile on a board.
 *
 * Everything here renders the payload the BACKEND already resolved and gated.
 * The component never fetches content of its own — in particular, a
 * `{ kind: "locked" }` payload has no fields to render, which is exactly the
 * lock-model contract (a sealed source shows a redacted placeholder, never a
 * title or snippet).
 */
@Component({
  selector: "app-dashboard-tile",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./dashboard-tile.component.html",
  styleUrl: "./dashboard-tile.component.scss",
  host: {
    "[style.--tile-span]": "tile().span",
    "[class.cited]": "cited() > 0",
    "[class.is-locked]": "tile().data.kind === 'locked'",
    "[class.is-arranging]": "editing()",
    "[class.is-dragging]": "dragging()",
    "[class.is-drop-target]": "dropTarget()",
  },
})
export class DashboardTileComponent {
  readonly tile = input.required<ResolvedTile>();
  /** 1-based citation index when board-Ask used this tile; 0 = not cited. */
  readonly cited = input(0);
  /** True while the board is in layout mode (resize / remove affordances shown). */
  readonly editing = input(false);
  /** This tile is the one being dragged. */
  readonly dragging = input(false);
  /** This tile is the current drop target. */
  readonly dropTarget = input(false);

  readonly remove = output<void>();
  readonly widen = output<void>();
  readonly narrow = output<void>();
  readonly openSource = output<SourceRef>();
  readonly refreshAnswer = output<void>();

  readonly data = computed(() => this.tile().data);

  readonly heading = computed(() => {
    const t = this.tile();
    // A user-authored title is board chrome — but NEVER for a WITHHELD tile, where
    // the wording routinely paraphrases content the session cannot read. The
    // backend already strips it (`redact_tile_chrome`); this is the second layer,
    // and it covers `missing`/`unconfigured` too, not just `locked`.
    if (
      t.data.kind === "locked" ||
      t.data.kind === "missing" ||
      t.data.kind === "unconfigured"
    ) {
      return DEFAULT_TITLE[t.data.kind];
    }
    if (t.title && t.title.trim()) return t.title;
    const d = t.data;
    switch (d.kind) {
      case "note":
      case "meeting":
      case "document":
        return d.title;
      case "person":
        return d.name;
      case "drift":
        return `${d.entity} · ${d.predicate || "history"}`;
      case "numbers":
      case "pulse":
        return d.entity;
      case "livingAnswer":
        return d.question || DEFAULT_TITLE.livingAnswer;
      default:
        return DEFAULT_TITLE[d.kind];
    }
  });

  readonly kindLabel = computed(() => KIND_LABEL[this.data().kind]);
  readonly isLive = computed(() => LIVE_KINDS.has(this.data().kind));

  // ── narrowed views of the discriminated union ──────────────────────────────
  //
  // Angular's template type-checker does NOT narrow a union through
  // `@switch (data().kind)`, so each variant gets its own `computed()` and the
  // template reads `@if (note(); as n)`. Deriving with computed() (rather than a
  // method call in the template) is also what angular-zoneless.md §2 requires.
  private narrow$<K extends TileData["kind"]>(kind: K) {
    return computed(() => {
      const d = this.data();
      return d.kind === kind ? (d as Extract<TileData, { kind: K }>) : null;
    });
  }

  readonly note = this.narrow$("note");
  readonly meeting = this.narrow$("meeting");
  readonly document = this.narrow$("document");
  readonly person = this.narrow$("person");
  readonly reminders = this.narrow$("reminders");
  readonly drift = this.narrow$("drift");
  readonly numbers = this.narrow$("numbers");
  readonly pulse = this.narrow$("pulse");
  readonly promises = this.narrow$("promises");
  readonly livingAnswer = this.narrow$("livingAnswer");
  readonly isSealed = computed(() => this.data().kind === "locked");
  readonly isMissing = computed(() => this.data().kind === "missing");
  readonly isUnconfigured = computed(() => this.data().kind === "unconfigured");

  /** Pulse: the tallest weekly bucket, so the bars can be scaled to it. */
  readonly pulsePeak = computed(() => {
    const p = this.pulse();
    return p ? Math.max(1, ...p.weekly) : 1;
  });

  barHeight(value: number): number {
    return Math.round((value / this.pulsePeak()) * 100);
  }

  onSource(source: SourceRef | null): void {
    if (source) this.openSource.emit(source);
  }

  /** Same contract as `formatDate`: a template-reachable formatter degrades, never throws. */
  formatDuration(seconds: number | null | undefined): string {
    if (typeof seconds !== "number" || !Number.isFinite(seconds)) return "—";
    const m = Math.round(seconds / 60);
    if (m < 60) return `${m} min`;
    const h = Math.floor(m / 60);
    return `${h}h ${m % 60}m`;
  }

  /** "2h ago" / "yesterday" / "12 Jun" from an epoch-millis timestamp. */
  relative(ms: number): string {
    if (!ms) return "recently";
    const mins = Math.max(0, Math.round((Date.now() - ms) / 60000));
    if (mins < 60) return `${mins}m ago`;
    const hours = Math.round(mins / 60);
    if (hours < 24) return `${hours}h ago`;
    const days = Math.round(hours / 24);
    if (days === 1) return "yesterday";
    if (days < 30) return `${days}d ago`;
    return new Date(ms).toLocaleDateString(undefined, { day: "numeric", month: "short" });
  }

  /**
   * NEVER THROWS, and that is the point — this method used to.
   *
   * It read `iso.slice(0, 10)` on the NaN branch, so a missing timestamp raised a TypeError from
   * a template binding. Angular aborts the REST of a change-detection pass when a binding throws,
   * so one bad tile blanked every binding after it — including, three components away, the
   * Add-a-tile palette that `app-shell` renders later in the same pass. It presented as "the
   * palette won't open once the board has more than two tiles" and took six fixes aimed at the
   * wrong thing.
   *
   * The cause (a serde field-naming mismatch) is fixed at the seam and pinned by a Rust test. This
   * guard is the second half: a formatter reachable from a template must degrade to a string on
   * ANY input, so a future data glitch costs one wrong-looking cell instead of half the UI.
   */
  formatDate(iso: string | null | undefined): string {
    if (typeof iso !== "string" || iso === "") return "—";
    const t = Date.parse(iso);
    if (Number.isNaN(t)) return iso.slice(0, 10);
    return new Date(t).toLocaleDateString(undefined, {
      day: "numeric",
      month: "short",
      year: "numeric",
    });
  }
}
