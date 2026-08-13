import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
  output,
  signal,
} from "@angular/core";
import type { ResolvedTile, TileData } from "../../../core/models";
import {
  MurIconComponent,
  type ShellIcon,
} from "../../../design-system/icon/icon.component";

const LABEL: Record<TileData["kind"], string> = {
  locked: "Sealed item",
  missing: "Missing source",
  unconfigured: "Needs setup",
  note: "Note",
  meeting: "Recording",
  document: "Document",
  person: "Person",
  reminders: "Reminders",
  drift: "Drift view",
  numbers: "Numbers view",
  pulse: "Pulse view",
  promises: "Promises view",
  livingAnswer: "Living answer",
};

const ICON: Record<TileData["kind"], ShellIcon> = {
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

@Component({
  selector: "app-dashboard-compose",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurIconComponent],
  templateUrl: "./dashboard-compose.component.html",
  styleUrl: "./dashboard-compose.component.scss",
})
export class DashboardComposeComponent {
  readonly tiles = input.required<readonly ResolvedTile[]>();
  readonly busy = input(false);
  readonly move = output<{ tile: ResolvedTile; delta: -1 | 1 }>();
  readonly remove = output<ResolvedTile>();
  readonly reorder = output<{ tileId: string; targetId: string }>();
  readonly draggingId = signal<string | null>(null);
  readonly dropTargetId = signal<string | null>(null);
  readonly announcement = signal("");
  readonly rows = computed(() =>
    this.tiles().map((tile, index, all) => ({
      tile,
      index,
      first: index === 0,
      last: index === all.length - 1,
      label: LABEL[tile.data.kind],
      icon: ICON[tile.data.kind],
      title: this.title(tile),
      state: this.state(tile),
    })),
  );

  onDragStart(tile: ResolvedTile, event: DragEvent): void {
    if (this.busy()) {
      event.preventDefault();
      return;
    }
    this.draggingId.set(tile.id);
    event.dataTransfer?.setData("text/plain", tile.id);
    if (event.dataTransfer) event.dataTransfer.effectAllowed = "move";
  }

  onDragOver(tile: ResolvedTile, event: DragEvent): void {
    if (!this.draggingId()) return;
    event.preventDefault();
    this.dropTargetId.set(tile.id);
  }

  onDrop(target: ResolvedTile, event: DragEvent): void {
    event.preventDefault();
    const tileId = this.draggingId();
    this.onDragEnd();
    if (tileId && tileId !== target.id && !this.busy()) {
      const moved = this.tiles().find((tile) => tile.id === tileId);
      const position =
        this.tiles().findIndex((tile) => tile.id === target.id) + 1;
      if (moved) {
        this.announcement.set(
          `${this.title(moved)} moved to position ${position} of ${this.tiles().length}`,
        );
      }
      this.reorder.emit({ tileId, targetId: target.id });
    }
  }

  onDragEnd(): void {
    this.draggingId.set(null);
    this.dropTargetId.set(null);
  }

  moveBy(tile: ResolvedTile, delta: -1 | 1): void {
    if (this.busy()) return;
    const index = this.tiles().findIndex(
      (candidate) => candidate.id === tile.id,
    );
    if (index < 0 || index + delta < 0 || index + delta >= this.tiles().length)
      return;
    this.announcement.set(
      `${this.title(tile)} moved to position ${index + delta + 1} of ${this.tiles().length}`,
    );
    this.move.emit({ tile, delta });
  }

  onHandleKeydown(tile: ResolvedTile, event: KeyboardEvent): void {
    if (this.busy()) return;
    if (!event.altKey) return;
    if (event.key === "ArrowUp") {
      event.preventDefault();
      this.moveBy(tile, -1);
    } else if (event.key === "ArrowDown") {
      event.preventDefault();
      this.moveBy(tile, 1);
    }
  }

  private title(tile: ResolvedTile): string {
    const data = tile.data;
    if (data.kind === "locked") return "Sealed item";
    if (data.kind === "missing") return "Source removed";
    if (data.kind === "unconfigured") return "Not configured";
    if (tile.title?.trim()) return tile.title.trim();
    switch (data.kind) {
      case "note":
      case "meeting":
      case "document":
        return data.title;
      case "person":
        return data.name;
      case "drift":
        return `${data.entity} · ${data.predicate || "history"}`;
      case "numbers":
      case "pulse":
        return data.entity;
      case "livingAnswer":
        return data.question || "Living answer";
      default:
        return LABEL[data.kind];
    }
  }

  private state(tile: ResolvedTile): string {
    const data = tile.data;
    switch (data.kind) {
      case "locked":
        return "Content hidden until its folder is unlocked";
      case "missing":
        return "The source is no longer available";
      case "unconfigured":
        return "Choose a source to finish setup";
      case "reminders":
        return `${data.rows.length} reminder ${data.rows.length === 1 ? "item" : "items"}`;
      case "promises":
        return `${data.rows.length} open ${data.rows.length === 1 ? "promise" : "promises"}`;
      case "livingAnswer":
        return data.answer ? "Answer ready" : "Waiting for an answer";
      case "note":
      case "document":
        return data.snippet.trim() ? "Readable material" : "No preview text";
      case "meeting":
        return data.hasAudio ? "Audio available" : "Transcript material";
      default:
        return "Live derived board view";
    }
  }
}
