import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  inject,
  output,
  signal,
  viewChild,
} from "@angular/core";
import { IpcService } from "../../../core/ipc.service";
import { MurSpinnerComponent } from "../../../design-system/spinner/spinner.component";
import type { NoteCitation, TileConfig, TileKind } from "../../../core/models";

/** What a chosen tile kind needs before it can be added. */
type SourceMode = "none" | "link" | "entity" | "question";

export interface TileChoice {
  kind: TileKind;
  refId?: string;
  title?: string;
  config?: TileConfig;
}

interface NodeType {
  kind: TileKind;
  name: string;
  description: string;
  /** True for the kinds no vault or cloud notetaker can build. */
  onlyMurmur: boolean;
  mode: SourceMode;
  family: "note" | "rec" | "person" | "doc" | "insight" | "remind";
}

/**
 * The tile catalogue. Order is the palette's reading order: material first,
 * then the derived views that are the actual reason to build a board.
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
    kind: "drift",
    name: "Drift lane",
    description: "How ONE value moved over time — GA: Apr 30 → May 24 → Jun 14.",
    onlyMurmur: true,
    mode: "entity",
    family: "insight",
  },
  {
    kind: "numbers",
    name: "Numbers",
    description: "Figures that were said out loud, with what they used to be.",
    onlyMurmur: true,
    mode: "entity",
    family: "insight",
  },
  {
    kind: "pulse",
    name: "Pulse",
    description: "How often this is actually talked about — and where it went quiet.",
    onlyMurmur: true,
    mode: "entity",
    family: "insight",
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
    description: "A pinned question whose answer you can re-run any time.",
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
 * The "Add a tile" palette — a FLOATING modal, so it is OPAQUE
 * (`--surface-overlay`, `backdrop-filter: none`) per angular-zoneless.md T3.
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
})
export class TilePaletteComponent {
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);

  private readonly dlg = viewChild<ElementRef<HTMLDialogElement>>("dlg");

  /**
   * Promote the dialog into the TOP LAYER once it renders.
   *
   * `showModal()` (not `open`) is the point: only the modal form enters the top
   * layer, which is what puts it outside every stacking context and every
   * fixed-positioning containing block. `afterNextRender` is the zoneless-safe
   * one-shot, and the injector is required because this runs from a field
   * initialiser's effect rather than a constructor body.
   */
  private readonly _open = afterNextRender(() => {
    const el = this.dlg()?.nativeElement;
    if (el && !el.open) el.showModal();
  });

  readonly dismiss = output<void>();
  readonly choose = output<TileChoice>();

  readonly types = NODE_TYPES;

  /** The kind being configured; `null` ⇒ the catalogue step. */
  readonly selected = signal<NodeType | null>(null);
  readonly query = signal("");
  readonly loading = signal(false);
  readonly candidates = signal<NoteCitation[]>([]);
  readonly entities = signal<{ id: string; name: string; mentionCount: number }[]>([]);
  readonly question = signal("");

  private readonly searchField =
    signal<ElementRef<HTMLInputElement> | null>(null);

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
   * A click that lands on the DIALOG ITSELF is a backdrop click: the dialog's own
   * box is the panel, and its ::backdrop is painted by the element, so a press
   * outside the panel is reported with the dialog as target. Anything inside the
   * panel has a descendant target and is left alone.
   */
  onDialogClick(event: MouseEvent): void {
    if (event.target === this.dlg()?.nativeElement) this.dismiss.emit();
  }

  /** Escape is handled by the platform; this mirrors it back into our state. */
  onDialogClose(): void {
    this.dismiss.emit();
  }

  /**
   * Escape on the SECOND step goes back to the catalogue instead of closing the
   * whole palette — losing the whole modal because you changed your mind about
   * which KIND of tile you wanted is a needless step backwards.
   */
  onDialogKeydown(event: KeyboardEvent): void {
    if (event.key !== "Escape" || !this.selected()) return;
    event.preventDefault();
    this.back();
  }

  registerField(el: ElementRef<HTMLInputElement> | undefined): void {
    if (!el) return;
    this.searchField.set(el);
    afterNextRender(() => el.nativeElement.focus(), { injector: this.injector });
  }
}
