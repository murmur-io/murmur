import { ChangeDetectionStrategy, Component, computed, input, output } from "@angular/core";
import type { ResolvedTile, SourceRef, TileData } from "../../../core/models";
import type { ShellIcon } from "../../../design-system/icon/icon.component";
import { MurIconComponent } from "../../../design-system/icon/icon.component";

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

/** The 22px mark in each tile header — the kind, made scannable. */
const TILE_ICON: Record<TileData["kind"], ShellIcon> = {
  locked: "lock",
  missing: "document",
  unconfigured: "plus",
  note: "notes",
  meeting: "meetings",
  document: "document",
  person: "people",
  reminders: "reminders",
  drift: "drift",
  numbers: "numbers",
  pulse: "pulse",
  promises: "promises",
  livingAnswer: "ask",
};

/**
 * FOUR hues, and no more.
 *
 * The four graph families measure PASS on chroma, CVD separation and contrast in
 * both themes; adding a fifth (`--warning`) collides with amber at ΔE 5.7. So only
 * MATERIAL kinds — the things that exist in the vault as documents — wear a hue.
 * DERIVED tiles are views over that material and take the neutral mark, which is
 * also what keeps a board of ten tiles from reading as confetti.
 *
 * `--accent` is deliberately absent: it is reserved for the AI channel (the
 * citation ring, the Ask composer, the living answer), it is USER-SELECTABLE, and
 * five of its six options sit within ΔE 15 of a family hue. An accent doing
 * category work is an accent that stops meaning "the brain touched this".
 */
const TILE_HUE: Partial<Record<TileData["kind"], string>> = {
  note: "var(--graph-note)",
  document: "var(--graph-document)",
  meeting: "var(--graph-meeting)",
  person: "var(--graph-entity)",
  livingAnswer: "var(--accent)",
};

/**
 * The width a kind wants, in 12ths — applied only to tiles the user never resized.
 *
 * Why an override and not a default at add time: `commands/dashboards.rs` clamps
 * with `span.unwrap_or(4)`, so EVERY tile on EVERY existing board already holds a
 * concrete `4` in SQLite. A new add-time default would change nothing about a board
 * that already exists, which is exactly the board that was complained about. The
 * first explicit resize writes a real value and this stops applying.
 */
const DEFAULT_SPAN: Record<TileData["kind"], number> = {
  locked: 3,
  missing: 3,
  unconfigured: 3,
  note: 4,
  meeting: 6,
  document: 4,
  person: 3,
  reminders: 4,
  drift: 4,
  numbers: 4,
  pulse: 3,
  promises: 6,
  livingAnswer: 6,
};

/** The span the backend hands every un-resized tile — the value the override replaces. */
const STORED_DEFAULT_SPAN = 4;

/** Collapsed empties are uniform and narrow, so a row of them reads as one gutter. */
const EMPTY_SPAN = 3;

/**
 * What the body has to say, which is NOT the same question as "does it have rows".
 *
 * - `empty`      — never had data. Collapses to a header strip; a full card holding
 *                  one sentence of regret is what made nine tiles read as a wall.
 * - `good`       — genuinely zero, and that is the GOOD outcome. Keeps its card.
 *                  "Nothing open — every commitment on this board is closed" is a
 *                  result; rendering a success as an absence is the most
 *                  demoralising thing a board can do, and it is why this is a
 *                  separate state rather than `rows.length === 0`.
 * - `degenerate` — populated, but with too little to support the mark it implies.
 *                  A drift lane with one step asserts movement that never happened.
 * - `normal`     — render the body.
 */
export type TileBodyState = "empty" | "good" | "degenerate" | "normal";

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
  imports: [MurIconComponent],
  host: {
    "[attr.data-tile-id]": "tile().id",
    "[attr.data-kind]": "data().kind",
    "[style.--tile-span]": "displaySpan()",
    "[style.--tile-hue]": "hue()",
    "[class.cited]": "cited() > 0",
    "[class.is-locked]": "tile().data.kind === 'locked'",
    "[class.is-empty]": "bodyState() === 'empty'",
    "[class.is-duplicate]": "duplicateOf() !== null",
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
  readonly canMoveEarlier = input(false);
  readonly canMoveLater = input(false);
  /** Security-sensitive persistence is opt-in; dashboards keep this disabled. */
  readonly allowRefreshAnswer = input(false);
  /** This tile is the one being dragged. */
  readonly dragging = input(false);
  /** This tile is the current drop target. */
  readonly dropTarget = input(false);
  /**
   * The title of an EARLIER tile that resolves to exactly the same thing, or null.
   *
   * The board that prompted this work carried two identical Promise ledgers. That is
   * not user error: `tile-palette` declares `promises` with `mode: "none"`, so it
   * never asks for an owner, so `config.owner` is never written, so both tiles
   * resolve to the same global list — structurally guaranteed. The second one
   * renders as a back-reference instead of a second copy of the same rows.
   */
  readonly duplicateOf = input<string | null>(null);

  readonly remove = output<void>();
  readonly widen = output<void>();
  readonly narrow = output<void>();
  readonly moveEarlier = output<void>();
  readonly moveLater = output<void>();
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
  readonly mark = computed(() => TILE_ICON[this.data().kind]);
  readonly hue = computed(() => TILE_HUE[this.data().kind] ?? "var(--text-secondary)");

  /**
   * What the body has to say. See {@link TileBodyState} — the load-bearing
   * distinction is `empty` (never had data → collapse) versus `good` (zero, and
   * zero is the win → keep the card).
   */
  readonly bodyState = computed<TileBodyState>(() => {
    const d = this.data();
    switch (d.kind) {
      case "missing":
      case "unconfigured":
        return "empty";
      case "note":
      case "document":
        return d.snippet.trim() ? "normal" : "empty";
      case "numbers":
        return d.rows.length === 0 ? "empty" : "normal";
      case "pulse":
        return d.total === 0 ? "empty" : "normal";
      case "drift":
        // Zero steps is nothing; ONE step is worse than nothing, because a rail
        // drawn through a single point asserts a movement that never happened.
        if (d.rows.length === 0) return "empty";
        return d.rows.length < 2 ? "degenerate" : "normal";
      case "promises":
      case "reminders":
        return d.rows.length === 0 ? "good" : "normal";
      default:
        // `locked` keeps its full card ON PURPOSE: a redacted region is content,
        // and a board that can be screen-shared as-is is the point of the lock model.
        return "normal";
    }
  });

  /** One sentence saying where data WILL land — never an apology for its absence. */
  readonly emptyCopy = computed(() => {
    const d = this.data();
    switch (d.kind) {
      case "missing":
        return "Its source is gone from the vault";
      case "unconfigured":
        return "No source chosen yet";
      case "note":
      case "document":
        return "Nothing written in it yet";
      case "numbers":
        return "Figures said out loud land here";
      case "pulse":
        return "Mentions land here once it comes up";
      case "drift":
        return "Values land here as they get revised";
      default:
        return "Nothing here yet";
    }
  });

  /** The good-news line for a tile whose emptiness is the desired outcome. */
  readonly goodCopy = computed(() =>
    this.data().kind === "promises"
      ? "Nothing open — every commitment on this board is closed"
      : "No open reminders",
  );

  /**
   * A tile with nothing in it does not get to claim it is live, and neither does
   * a collapsed strip — a blinking dot on an empty box is the board asserting
   * activity it cannot show.
   */
  readonly isLive = computed(
    () =>
      LIVE_KINDS.has(this.data().kind) &&
      this.bodyState() === "normal" &&
      this.duplicateOf() === null,
  );

  /**
   * The width actually rendered. Collapsed empties are uniform and narrow; a tile
   * the user never resized takes its kind's natural width; an explicitly resized
   * tile is left exactly alone.
   */
  readonly displaySpan = computed(() => {
    if (this.duplicateOf() !== null) return EMPTY_SPAN;
    if (this.bodyState() === "empty") return EMPTY_SPAN;
    const stored = this.tile().span;
    return stored === STORED_DEFAULT_SPAN ? DEFAULT_SPAN[this.data().kind] : stored;
  });

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

  /**
   * Pulse, as a heat strip on a FIXED ladder — not bars scaled to the local peak.
   *
   * The bar chart this replaces lied twice. `min-height: 3px` drew a visible mark
   * for a week with no mentions at all, so absence looked like activity; and
   * scaling to `max(...weekly)` meant a peak of 1 rendered at full height, so a
   * board with forty mentions a week was pixel-identical to one with a single
   * mention. A fixed ladder is what makes two tiles — and two boards —
   * comparable at a glance, which is the entire job of the mark.
   *
   * Level 0 is drawn as an empty well: the week EXISTS and had nothing, which is
   * a different statement from "no data".
   */
  heatLevel(value: number): 0 | 1 | 2 | 3 | 4 {
    if (value <= 0) return 0;
    if (value <= 1) return 1;
    if (value <= 3) return 2;
    if (value <= 6) return 3;
    return 4;
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
