import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
  output,
  signal,
} from "@angular/core";
import type { DashboardSummary, TileKind } from "../../../core/models";

/** One box in the miniature preview — a scaled stand-in for a real tile. */
export interface MiniTile {
  span: number;
  family: "note" | "rec" | "person" | "doc" | "insight" | "remind";
}

/** How a tile kind reads in the miniature. The derived kinds share one look. */
const FAMILY: Record<TileKind, MiniTile["family"]> = {
  note: "note",
  meeting: "rec",
  document: "doc",
  person: "person",
  reminders: "remind",
  drift: "insight",
  numbers: "insight",
  pulse: "insight",
  promises: "insight",
  living_answer: "insight",
};

const FAMILY_NAME: Record<MiniTile["family"], [string, string]> = {
  note: ["note", "notes"],
  rec: ["recording", "recordings"],
  person: ["person", "people"],
  doc: ["document", "documents"],
  insight: ["insight", "insights"],
  remind: ["reminder", "reminders"],
};

/**
 * One board card in the `/dashboards` list.
 *
 * The card's miniature is drawn from the board's REAL tile layout (kind +
 * span). That is layout METADATA — the list never reads a gated payload, so a
 * card can never leak a sealed source's title, only the shape of the board.
 */
@Component({
  selector: "app-board-card",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./board-card.component.html",
  styleUrl: "./board-card.component.scss",
})
export class BoardCardComponent {
  readonly board = input.required<DashboardSummary>();
  /** Pinned cards render larger (a taller miniature). */
  readonly big = input(false);

  readonly open = output<void>();
  readonly togglePin = output<void>();
  readonly remove = output<void>();

  /** A calm placeholder shape for a board with no tiles yet. */
  private readonly EMPTY_MINI: MiniTile[] = [
    { span: 7, family: "note" },
    { span: 5, family: "insight" },
    { span: 4, family: "rec" },
    { span: 8, family: "person" },
  ];

  readonly miniTiles = computed<MiniTile[]>(() => {
    const b = this.board();
    if (b.tileCount === 0) return this.EMPTY_MINI;
    return b.tileKinds.map((t) => ({
      span: Math.min(12, Math.max(3, t.span)),
      family: FAMILY[t.kind] ?? "note",
    }));
  });

  /** Source-mix chips: how many tiles of each family the board carries. */
  readonly chips = computed(() => {
    const counts = new Map<MiniTile["family"], number>();
    for (const t of this.board().tileKinds) {
      const f = FAMILY[t.kind] ?? "note";
      counts.set(f, (counts.get(f) ?? 0) + 1);
    }
    return [...counts.entries()].map(([family, n]) => ({
      family,
      label: `${n} ${FAMILY_NAME[family][n === 1 ? 0 : 1]}`,
    }));
  });

  readonly updatedLabel = computed(() => relative(this.board().updatedAt));

  /** Deterministic bar heights for the miniature waveform (no Math.random). */
  readonly waveBars = [26, 62, 40, 84, 34, 70, 48, 92];

  onOpen(): void {
    this.open.emit();
  }
  onPin(event: Event): void {
    event.stopPropagation();
    this.togglePin.emit();
  }
  /**
   * Arm, then fire.
   *
   * A board is the one artifact in this feature the user BUILT rather than recorded,
   * and delete went straight through on a single click with no undo. This is a
   * two-step in the component rather than `window.confirm`, because a native dialog
   * blocks the entire webview — the trap `angular-zoneless.md` calls out.
   */
  readonly confirming = signal(false);

  onRemove(event: Event): void {
    event.stopPropagation();
    if (!this.confirming()) {
      this.confirming.set(true);
      return;
    }
    this.confirming.set(false);
    this.remove.emit();
  }

  cancelRemove(event: Event): void {
    event.stopPropagation();
    this.confirming.set(false);
  }
}

/** "2h ago" / "yesterday" / "12 Jun". */
function relative(iso: string): string {
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "";
  const mins = Math.max(0, Math.round((Date.now() - then) / 60000));
  if (mins < 1) return "just now";
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.round(hours / 24);
  if (days === 1) return "yesterday";
  if (days < 30) return `${days}d ago`;
  return new Date(then).toLocaleDateString(undefined, {
    day: "numeric",
    month: "short",
  });
}
