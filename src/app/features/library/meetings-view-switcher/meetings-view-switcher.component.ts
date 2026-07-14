import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  output,
  signal,
} from "@angular/core";
import { SavedViewsService } from "../../../services/saved-views.service";
import {
  MEETING_GROUP_FIELDS,
  MEETING_VIEW_FIELDS,
  type ViewField,
} from "../../../services/view-engine";
import {
  DEFAULT_VIEW_CONFIG,
  parseViewConfig,
  type SavedView,
  type ViewConfig,
  type ViewFilter,
} from "../../../core/models";

/** The filter ops offered per field type in the config menu. */
const OPS_BY_TYPE: Record<ViewField["type"], ViewFilter["op"][]> = {
  text: ["contains", "eq", "neq", "isEmpty", "isNotEmpty"],
  date: ["before", "after", "isEmpty", "isNotEmpty"],
  status: ["eq", "neq"],
  number: ["eq", "neq"],
};

const OP_LABELS: Record<ViewFilter["op"], string> = {
  eq: "is",
  neq: "is not",
  contains: "contains",
  before: "before",
  after: "after",
  isEmpty: "is empty",
  isNotEmpty: "is not empty",
};

/**
 * Feature B — the meetings-list VIEW SWITCHER. Renders a strip of "List"
 * (default) + every saved view as selectable tabs, a "+" to save a new view,
 * and — when a saved view is active — a layout toggle (Table/Board) plus a
 * filter/sort/group control that opens an OPAQUE overlay menu (trap T3: floats
 * over content → `.menu` primitive / `var(--surface-overlay)`, never the
 * frosted `.card`).
 *
 * It reads the roster from the root-persisted {@link SavedViewsService} and
 * drives it directly (select / create / delete / config edits), so the parent
 * `LibraryComponent` only has to render Table/Board off the SAME service state.
 * A config change persists through the service (backend is truth); the parent
 * re-derives its rows from the active view reactively.
 */
@Component({
  selector: "app-meetings-view-switcher",
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: {
    "(document:click)": "onDocumentClick($event)",
  },
  templateUrl: "./meetings-view-switcher.component.html",
  styleUrl: "./meetings-view-switcher.component.scss",
})
export class MeetingsViewSwitcherComponent {
  private readonly savedViews = inject(SavedViewsService);

  readonly views = this.savedViews.views;
  readonly activeViewId = this.savedViews.activeViewId;
  readonly activeView = this.savedViews.activeView;

  /** The field catalog for filter/sort; the low-cardinality subset for grouping. */
  readonly fields = MEETING_VIEW_FIELDS;
  readonly groupFields = MEETING_GROUP_FIELDS;

  /** Emitted after any change that alters what the parent should render (select/config/layout). */
  readonly changed = output<void>();

  /** Which floating panel is open: the "+" name prompt, or the config menu. */
  readonly openPanel = signal<"add" | "config" | null>(null);
  /** The new-view name being typed in the "+" prompt. */
  readonly newViewName = signal("");
  /** Non-empty while a create/config op is in flight (guards the buttons). */
  readonly busy = signal(false);

  /** The active view's parsed config (safe default when none / unparseable). */
  readonly activeConfig = computed<ViewConfig>(() => {
    const v = this.activeView();
    return v ? parseViewConfig(v.config) : { ...DEFAULT_VIEW_CONFIG };
  });

  /** Select "List" (null) or a saved view. */
  select(id: string | null): void {
    this.openPanel.set(null);
    this.savedViews.setActiveView(id);
    this.changed.emit();
  }

  /** Open the "+" name prompt. */
  openAdd(): void {
    this.newViewName.set("");
    this.openPanel.set(this.openPanel() === "add" ? null : "add");
  }

  /** Open the filter/sort/group config menu for the active view. */
  toggleConfig(): void {
    this.openPanel.set(this.openPanel() === "config" ? null : "config");
  }

  /** Close any open floating panel on an outside click (buttons stopPropagation). */
  onDocumentClick(event: MouseEvent): void {
    if (this.openPanel() === null) {
      return;
    }
    const target = event.target as HTMLElement | null;
    if (target?.closest(".sv-panel, .sv-panel-trigger")) {
      return;
    }
    this.openPanel.set(null);
  }

  /** Create a saved view from the "+" prompt (default table layout + default config). */
  async createView(layout: "table" | "board"): Promise<void> {
    const name = this.newViewName().trim();
    if (!name || this.busy()) {
      return;
    }
    this.busy.set(true);
    try {
      await this.savedViews.create(
        name,
        layout,
        JSON.stringify(DEFAULT_VIEW_CONFIG),
      );
      this.openPanel.set(null);
      this.changed.emit();
    } finally {
      this.busy.set(false);
    }
  }

  /** Delete the active saved view (falls back to List). */
  async deleteActive(): Promise<void> {
    const v = this.activeView();
    if (!v || this.busy()) {
      return;
    }
    this.busy.set(true);
    try {
      await this.savedViews.delete(v.id);
      this.openPanel.set(null);
      this.changed.emit();
    } finally {
      this.busy.set(false);
    }
  }

  /** Switch the active view's layout (Table ↔ Board), persisting it. */
  async setLayout(layout: "table" | "board"): Promise<void> {
    const v = this.activeView();
    if (!v || v.layout === layout || this.busy()) {
      return;
    }
    await this.persist({ ...v, layout });
  }

  /** Add an empty filter row to the active view's config. */
  async addFilter(): Promise<void> {
    const field = this.fields[0];
    const op = OPS_BY_TYPE[field.type][0];
    await this.patchConfig((c) => ({
      ...c,
      filters: [...c.filters, { field: field.id, op, value: "" }],
    }));
  }

  /** Change a filter's field (resets its op to the field's first valid op). */
  async setFilterField(index: number, fieldId: string): Promise<void> {
    const type = this.fieldType(fieldId);
    const op = OPS_BY_TYPE[type][0];
    await this.patchConfig((c) => ({
      ...c,
      filters: c.filters.map((f, i) =>
        i === index ? { ...f, field: fieldId, op, value: "" } : f,
      ),
    }));
  }

  async setFilterOp(index: number, op: string): Promise<void> {
    await this.patchConfig((c) => ({
      ...c,
      filters: c.filters.map((f, i) =>
        i === index ? { ...f, op: op as ViewFilter["op"] } : f,
      ),
    }));
  }

  async setFilterValue(index: number, value: string): Promise<void> {
    await this.patchConfig((c) => ({
      ...c,
      filters: c.filters.map((f, i) => (i === index ? { ...f, value } : f)),
    }));
  }

  async removeFilter(index: number): Promise<void> {
    await this.patchConfig((c) => ({
      ...c,
      filters: c.filters.filter((_, i) => i !== index),
    }));
  }

  /** Set the single primary sort field + direction. */
  async setSort(fieldId: string, direction: "asc" | "desc"): Promise<void> {
    await this.patchConfig((c) => ({
      ...c,
      sort: fieldId ? [{ field: fieldId, direction }] : [],
    }));
  }

  /** Set the board group-by field (empty ⇒ single "All" column). */
  async setGroupBy(fieldId: string): Promise<void> {
    await this.patchConfig((c) => ({
      ...c,
      groupBy: fieldId || null,
    }));
  }

  // --- config plumbing -----------------------------------------------------

  /** The valid ops for a filter's current field (drives its op dropdown). */
  opsFor(fieldId: string): { value: ViewFilter["op"]; label: string }[] {
    return OPS_BY_TYPE[this.fieldType(fieldId)].map((op) => ({
      value: op,
      label: OP_LABELS[op],
    }));
  }

  /** Whether a filter op needs a text/date value input (the *Empty ops don't). */
  opNeedsValue(op: string): boolean {
    return op !== "isEmpty" && op !== "isNotEmpty";
  }

  /** The current sort field id ("" = unsorted), for the sort dropdown binding. */
  readonly sortField = computed(() => this.activeConfig().sort[0]?.field ?? "");
  readonly sortDirection = computed(
    () => this.activeConfig().sort[0]?.direction ?? "desc",
  );
  readonly groupBy = computed(() => this.activeConfig().groupBy ?? "");

  private fieldType(fieldId: string): ViewField["type"] {
    return this.fields.find((f) => f.id === fieldId)?.type ?? "text";
  }

  /** Apply a config transform, then persist the whole view. */
  private async patchConfig(
    fn: (c: ViewConfig) => ViewConfig,
  ): Promise<void> {
    const v = this.activeView();
    if (!v) {
      return;
    }
    const next = fn(this.activeConfig());
    await this.persist({ ...v, config: JSON.stringify(next) });
  }

  private async persist(view: SavedView): Promise<void> {
    this.busy.set(true);
    try {
      await this.savedViews.save(view);
      this.changed.emit();
    } finally {
      this.busy.set(false);
    }
  }
}
