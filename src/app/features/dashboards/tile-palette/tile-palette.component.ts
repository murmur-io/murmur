import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  computed,
  effect,
  inject,
  output,
  signal,
  viewChild,
} from "@angular/core";
import { IpcService } from "../../../core/ipc.service";
import { MurSpinnerComponent } from "../../../design-system/spinner/spinner.component";
import type { TileChoice } from "../../../services/tile-palette.service";
import type { NoteCitation, TileKind } from "../../../core/models";

/** What a chosen tile kind needs before it can be added. */
type SourceMode = "none" | "link" | "entity" | "question";

export type { TileChoice };

interface NodeType {
  kind: TileKind;
  name: string;
  description: string;
  /** True for the kinds no vault or cloud notetaker can build. */
  onlyMurmur: boolean;
  mode: SourceMode;
  family: "note" | "rec" | "person" | "doc" | "insight" | "remind";
}

const FOCUSABLE_SELECTOR = [
  "button:not([disabled])",
  "a[href]",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  '[tabindex]:not([tabindex="-1"])',
].join(",");

/**
 * The tile catalogue. Order is the palette's reading order: material first,
 * then the derived views that are the actual reason to build a board.
 *
 * RETIRED 2026-08-04 — `drift`, `numbers`, `pulse`. Each is anchored to ONE
 * entity over the two thinnest tables in the schema, and each is blocked by a
 * mechanism in the extractor rather than by a shortage of recordings:
 *
 *  - `numbers` post-filters facts with `looks_numeric`, but `facts::EXTRACT_SYSTEM`
 *    asks for "durable state worth tracking" and never asks for quantities, so the
 *    filter runs over a substrate that was never built to contain figures (and
 *    what it does match is mostly dates).
 *  - `drift` needs SUPERSEDED facts, and `facts::reconcile_facts` requires the same
 *    NORMALIZED `(entity, subject, predicate)` while predicates are free-form —
 *    so supersessions do not form.
 *  - `pulse` reads `entity_mentions`, written by `graph_store::Db::add_mention` as
 *    `INSERT OR IGNORE` on PK `(entity_id, meeting_id)`. It counts MEETINGS, not
 *    utterances, which makes this entry's own former copy ("how often this is
 *    actually talked about") false, and caps every weekly bucket at 1.
 *
 * A tile that is empty for most people is worse than no tile, so they stop being
 * OFFERED. Their `resolve_tile` arms stay alive and must never be deleted: that
 * function ends in `Err(AppError::InvalidArg("unknown tile kind"))` and
 * `get_dashboard` collects with `?`, so removing an arm would turn every board
 * that already contains one of these into a hard error at open. Fixing the
 * extractor is its own investigation; see
 * docs/superpowers/specs/2026-08-04-dashboards-rebuild-design.md §7.
 */
const NODE_TYPES: NodeType[] = [
  {
    kind: "note",
    name: "Note",
    description: "A note from your vault, with its opening lines.",
    onlyMurmur: false,
    mode: "link",
    family: "note",
  },
  {
    kind: "meeting",
    name: "Recording",
    description: "A recorded meeting — when it ran, how long, and its audio.",
    onlyMurmur: true,
    mode: "link",
    family: "rec",
  },
  {
    kind: "document",
    name: "Document",
    description: "A PDF, deck or doc you imported into the brain.",
    onlyMurmur: false,
    mode: "link",
    family: "doc",
  },
  {
    kind: "person",
    name: "Person",
    description: "How often you meet them, and what they still owe you.",
    onlyMurmur: true,
    mode: "entity",
    family: "person",
  },
  {
    kind: "promises",
    name: "Promise ledger",
    description: "Who committed to what, and whether it landed on time.",
    onlyMurmur: true,
    mode: "none",
    family: "insight",
  },
  {
    kind: "reminders",
    name: "Reminders",
    description: "Your open reminders, soonest first.",
    onlyMurmur: false,
    mode: "none",
    family: "remind",
  },
  {
    kind: "living_answer",
    name: "Living answer",
    description: "A saved question with its last gated answer, when available.",
    onlyMurmur: true,
    mode: "question",
    family: "insight",
  },
];

/** Which link-candidate kinds satisfy which tile kind. */
const LINK_KIND: Partial<Record<TileKind, string>> = {
  note: "note",
  meeting: "meeting",
  document: "document",
};

/**
 * The "Add a tile" palette — a FLOATING overlay, so it is OPAQUE
 * (`--surface-overlay`, `backdrop-filter: none`) per angular-zoneless.md T3.
 *
 * PRESENTATION IS PURELY DECLARATIVE, and that is deliberate. The component is
 * rendered by `app-shell` behind `@if (tilePalette.open())` (see
 * `TilePaletteService` for why the shell and not the board), and its own template
 * is a plain `position: fixed` layer. There is no imperative "reveal" step at
 * all — no `showModal()`, no `matches(":modal")`, no teleport, no top layer.
 *
 * That is the lesson of angular-zoneless.md T5, paid for five times over: every
 * one of those is a modern-ish API that the engine the tests run supports and the
 * engine we ship may not, and each failure looked identical from outside — the
 * trigger flipping to "Close" while nothing appeared. A template `@if` plus CSS
 * has nothing left that can throw, be refused, or resolve differently.
 *
 * Source pickers reuse the SHIPPED gated readers: `list_link_candidates` for
 * notes/recordings/documents and `get_graph` for entities. Nothing sealed can
 * appear in either, so a board can only ever be composed from sources the user
 * can already see.
 */
@Component({
  selector: "app-tile-palette",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurSpinnerComponent],
  templateUrl: "./tile-palette.component.html",
  styleUrl: "./tile-palette.component.scss",
  host: {
    // Escape is handled on the DOCUMENT rather than on the overlay, because the
    // catalogue step focuses nothing — a `(keydown.escape)` bound to the overlay
    // would only fire once something inside it happened to hold focus. The
    // component only exists while the palette is open, so the listener cannot
    // outlive it.
    "(document:keydown)": "onKeydown($event)",
  },
})
export class TilePaletteComponent {
  private readonly ipc = inject(IpcService);

  readonly dismiss = output<void>();
  readonly choose = output<TileChoice>();

  readonly types = NODE_TYPES;
  readonly sourceTypes = NODE_TYPES.filter((type) => type.mode === "link");
  readonly viewTypes = NODE_TYPES.filter((type) => type.mode !== "link");

  /** The kind being configured; `null` ⇒ the catalogue step. */
  readonly selected = signal<NodeType | null>(null);
  readonly query = signal("");
  readonly loading = signal(false);
  readonly candidates = signal<NoteCitation[]>([]);
  readonly entities = signal<{ id: string; name: string; mentionCount: number }[]>([]);
  readonly question = signal("");

  /**
   * Focus the picker's field the moment it exists.
   *
   * This replaces a `registerField()` method that was bound NOWHERE in the
   * template, so it never ran and the field never focused — the palette opened
   * its second step and left the caret in the void. A `viewChild` signal fires
   * on its own when the `@switch` swaps the step in, with no call site to forget.
   */
  private readonly searchField = viewChild<ElementRef<HTMLInputElement>>("searchField");
  private readonly panel = viewChild<ElementRef<HTMLElement>>("panel");
  private readonly firstChoice = viewChild<ElementRef<HTMLButtonElement>>("firstChoice");

  private readonly _focusField = effect(() => {
    const field = this.searchField()?.nativeElement;
    if (field) field.focus();
    else this.firstChoice()?.nativeElement.focus();
  });

  readonly filteredEntities = computed(() => {
    const q = this.query().trim().toLowerCase();
    const all = this.entities();
    if (!q) return all.slice(0, 40);
    return all.filter((e) => e.name.toLowerCase().includes(q)).slice(0, 40);
  });

  async pick(type: NodeType): Promise<void> {
    if (type.mode === "none") {
      this.choose.emit({ kind: type.kind });
      return;
    }
    this.selected.set(type);
    this.query.set("");
    this.question.set("");
    this.candidates.set([]);
    if (type.mode === "link") await this.searchLinks("");
    if (type.mode === "entity") await this.loadEntities();
  }

  back(): void {
    this.selected.set(null);
  }

  onQuery(event: Event): void {
    const value = (event.target as HTMLInputElement).value;
    this.query.set(value);
    const type = this.selected();
    if (type?.mode === "link") void this.searchLinks(value);
  }

  onQuestion(event: Event): void {
    this.question.set((event.target as HTMLInputElement).value);
  }

  /**
   * Monotonic token for the in-flight search. Every keystroke fires an IPC call,
   * and without this a slower EARLIER query (or one from a tile kind the user
   * has since navigated away from) can land last and replace newer results.
   */
  private searchToken = 0;

  private async searchLinks(prefix: string): Promise<void> {
    const type = this.selected();
    if (!type) return;
    const want = LINK_KIND[type.kind];
    const token = ++this.searchToken;
    this.loading.set(true);
    try {
      const rows = await this.ipc.listLinkCandidates(prefix, 0, 60);
      if (token !== this.searchToken) return; // a newer keystroke won
      this.candidates.set(rows.filter((r) => r.kind === want));
    } finally {
      if (token === this.searchToken) this.loading.set(false);
    }
  }

  private async loadEntities(): Promise<void> {
    const token = ++this.searchToken;
    this.loading.set(true);
    try {
      const graph = await this.ipc.getGraph();
      if (token !== this.searchToken) return;
      this.entities.set(
        [...graph.nodes]
          .sort((a, b) => b.mentionCount - a.mentionCount)
          .map((n) => ({ id: n.id, name: n.name, mentionCount: n.mentionCount })),
      );
    } finally {
      if (token === this.searchToken) this.loading.set(false);
    }
  }

  /**
   * SOURCE TITLES ARE DELIBERATELY NEVER SENT — the same rule reminders follow
   * (`models.ts`: "Create/update payload. Source titles are deliberately never
   * sent"). Persisting the chosen source's title into `dashboard_tiles.title`
   * would put a plaintext COPY of gated content in an ungated column, which then
   * survives sealing and keeps rendering as the tile's heading. Only `refId` is
   * stored; the heading comes from the tile's freshly-gated payload on each read,
   * so it masks itself the moment the source is sealed.
   */
  chooseCandidate(candidate: NoteCitation): void {
    const type = this.selected();
    if (!type) return;
    this.choose.emit({ kind: type.kind, refId: candidate.id });
  }

  chooseEntity(entity: { id: string; name: string }): void {
    const type = this.selected();
    if (!type) return;
    this.choose.emit({ kind: type.kind, refId: entity.id });
  }

  chooseQuestion(): void {
    const type = this.selected();
    const q = this.question().trim();
    if (!type || !q) return;
    this.choose.emit({ kind: type.kind, title: q, config: { question: q } });
  }

  /**
   * Escape on the SECOND step goes back to the catalogue instead of closing the
   * whole palette — losing the whole overlay because you changed your mind about
   * which KIND of tile you wanted is a needless step backwards.
   */
  onKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape") {
      event.preventDefault();
      if (this.selected()) this.back();
      else this.dismiss.emit();
      return;
    }
    if (event.key !== "Tab") return;
    const panel = this.panel()?.nativeElement;
    if (!panel) return;
    const focusable = [...panel.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)]
      .filter((element) => element.getClientRects().length > 0);
    if (focusable.length === 0) {
      event.preventDefault();
      panel.focus();
      return;
    }
    const active = document.activeElement;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (!focusable.includes(active as HTMLElement)) {
      event.preventDefault();
      first.focus();
    } else if (event.shiftKey && active === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus();
    }
  }

}
