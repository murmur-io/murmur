import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import { open } from "@tauri-apps/plugin-dialog";
import { IpcService } from "../../core/ipc.service";
import type {
  BrainOverview,
  DocumentInfo,
  FolderNode,
  GraphData,
} from "../../core/models";
import { FoldersService } from "../../services/folders.service";
import { ToastService } from "../../services/toast.service";
import { BrainMapComponent } from "./brain-map.component";
import { BrainNoteEditorComponent } from "./brain-note-editor.component";
import { BrainSourceCardComponent } from "./brain-source-card.component";

/** The hard cap the map applies — kept in sync with BrainMapComponent's MAX_NODES. */
const MAP_NODE_CAP = 60;

/**
 * Sentinel id for the default "All folders" selector option — aggregate every
 * visible folder's documents/notes into one whole-brain view (so the source
 * cards match the whole-brain header counts). Never a real folder id (those are
 * UUIDs), so it can't collide.
 */
const ALL_FOLDERS_ID = "__all__";

/** A flattened folder option for the selector (indent reflects tree depth). */
interface FolderOption {
  id: string;
  /** Display label with a depth-indent prefix + a lock glyph when sealed. */
  label: string;
  /** Raw folder name (no indent / lock glyph) — for the "adding to X" hint. */
  name: string;
  /** Sealed-and-NOT-session-unlocked → add/list/delete are blocked. */
  blocked: boolean;
}

/**
 * The `/brain` page — "what's in my brain" (ClickUp-way knowledge sources).
 *
 * Top → bottom:
 *  1. STATUS HEADER — the whole-brain stat units (meetings / documents / notes),
 *     a plain-language semantic-search status chip, a gentle Settings nudge when
 *     semantic search / the on-device model isn't set up, and an [Ask ↗] link.
 *     Data from `brainOverview()`.
 *  2. KNOWLEDGE SOURCES — three in-flow `.card`s (delegated to
 *     {@link BrainSourceCardComponent}): 🎙 Meetings (read-only, links to
 *     /library), 📄 Documents (upload `.md`/`.txt`), 📝 Notes (type text). A
 *     folder selector governs which folder's docs/notes are listed. It DEFAULTS
 *     to "All folders" (aggregate every visible folder → matches the whole-brain
 *     header counts); a specific folder narrows to just that one. Adds under
 *     "All" target the first addable folder (surfaced as a subtle hint); a
 *     sealed folder fails closed (add disabled + a note).
 *  3. CONNECTIONS — the entity graph ({@link BrainMapComponent}), DEMOTED into a
 *     collapsible section (fit-to-view so the nodes fill the canvas).
 *
 * Everything is lock-aware: the overview + document/note lists + the graph all
 * re-fetch whenever the {@link FoldersService} tree changes (a session
 * unlock/relock / screen-share relock shifts visibility), so sealed content
 * drops out — or reappears — live, exactly like the old /graph page.
 */
@Component({
  selector: "app-brain",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    RouterLink,
    BrainSourceCardComponent,
    BrainNoteEditorComponent,
    BrainMapComponent,
  ],
  template: `
    <section class="brain">
      <!-- 1 — STATUS HEADER ------------------------------------------------- -->
      <header class="b-head card">
        <div class="b-head-top">
          <div class="b-identity">
            <span class="b-mark" aria-hidden="true">🧠</span>
            <div class="b-identity-text">
              <h2 class="b-title">Brain</h2>
              <p class="b-tagline">Everything your assistant can reason over.</p>
            </div>
          </div>

          @if (overview(); as ov) {
            <dl class="b-stats" aria-label="What’s in your brain">
              <div class="b-stat">
                <dd>{{ ov.meetingCount }}</dd>
                <dt>Meetings</dt>
              </div>
              <span class="b-stat-sep" aria-hidden="true"></span>
              <div class="b-stat">
                <dd>{{ ov.documentCount }}</dd>
                <dt>Documents</dt>
              </div>
              <span class="b-stat-sep" aria-hidden="true"></span>
              <div class="b-stat">
                <dd>{{ ov.noteCount }}</dd>
                <dt>Notes</dt>
              </div>
            </dl>
          } @else if (overviewError()) {
            <p class="b-summary-state b-summary-err">
              Couldn’t load the brain summary.
            </p>
          } @else {
            <p class="b-summary-state">Loading the brain summary…</p>
          }

          <div class="b-head-side">
            @if (modelMissing()) {
              <a class="pill b-badge b-badge-action" routerLink="/settings">
                <span class="b-badge-dot" aria-hidden="true"></span>
                Enable AI search
              </a>
            } @else {
              <span class="pill b-badge" [class.is-on]="semanticOn()">
                <span class="b-badge-dot" aria-hidden="true"></span>
                {{ semanticBadge() }}
              </span>
            }
            <a class="btn btn-ghost b-ask" routerLink="/ask">
              Ask
              <svg viewBox="0 0 16 16" width="13" height="13" fill="none" aria-hidden="true">
                <path
                  d="M5.5 3.5h7v7M12.5 3.5 4 12"
                  stroke="currentColor"
                  stroke-width="1.6"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                />
              </svg>
            </a>
          </div>
        </div>

        @if (nudge(); as msg) {
          <div class="banner is-accent b-nudge" role="status">
            <span class="b-nudge-glyph" aria-hidden="true"></span>
            <span>{{ msg }} <a routerLink="/settings">Open Settings</a></span>
          </div>
        }
      </header>

      <!-- 2 — KNOWLEDGE SOURCES --------------------------------------------- -->
      <section class="b-sources">
        <header class="b-sec-head">
          <div class="b-sec-text">
            <h3 class="b-sec-title">Knowledge sources</h3>
            <p class="b-sec-sub">
              What the brain reasons over — your meetings, plus any documents and
              notes you add.
            </p>
          </div>
          <label class="b-folder">
            <span class="b-folder-label">Folder</span>
            <select
              class="b-folder-select"
              aria-label="Choose a folder"
              [value]="selectedFolderId() ?? ''"
              (change)="onFolderChange($event)"
            >
              @for (o of folderOptions(); track o.id) {
                <option [value]="o.id">{{ o.label }}</option>
              }
            </select>
            @if (addHint(); as h) {
              <span class="b-add-hint">{{ h }}</span>
            }
          </label>
        </header>

        <div class="b-cards">
          <app-brain-source-card
            glyph="🎙"
            title="Meetings"
            subtitle="Recorded and transcribed — added by recording."
            [count]="overview()?.meetingCount ?? 0"
            linkTo="/library"
            linkLabel="Open meetings"
          />

          <app-brain-source-card
            glyph="📄"
            title="Documents"
            [subtitle]="
              isAll()
                ? 'Markdown / text files across your brain.'
                : 'Markdown / text files you’ve added to this folder.'
            "
            [count]="documents().length"
            [items]="documents()"
            [expanded]="docsExpanded()"
            [loading]="listLoading()"
            [busy]="importing()"
            [deletingId]="deletingId()"
            [blocked]="selectedBlocked()"
            addLabel="Add document"
            [emptyLabel]="
              isAll() ? 'No documents yet.' : 'No documents in this folder yet.'
            "
            (add)="pickAndImportDocument()"
            (deleteItem)="removeItem($event)"
            (toggleList)="docsExpanded.set(!docsExpanded())"
          />

          <app-brain-source-card
            glyph="📝"
            title="Notes"
            [subtitle]="
              isAll()
                ? 'Typed or pasted text across your brain.'
                : 'Typed or pasted text you’ve added to this folder.'
            "
            [count]="notes().length"
            [items]="notes()"
            [expanded]="notesExpanded()"
            [loading]="listLoading()"
            [busy]="savingNote()"
            [deletingId]="deletingId()"
            [blocked]="selectedBlocked()"
            addLabel="Add note"
            [emptyLabel]="
              isAll() ? 'No notes yet.' : 'No notes in this folder yet.'
            "
            (add)="openNoteEditor()"
            (deleteItem)="removeItem($event)"
            (toggleList)="notesExpanded.set(!notesExpanded())"
          />
        </div>

        @if (listError()) {
          <p class="empty b-list-err">{{ listError() }}</p>
        }
      </section>

      <!-- 3 — CONNECTIONS (collapsible, demoted) ---------------------------- -->
      <section class="b-conn">
        <button
          type="button"
          class="b-conn-toggle"
          [attr.aria-expanded]="connOpen()"
          (click)="connOpen.set(!connOpen())"
        >
          <svg
            class="b-conn-chevron"
            [class.is-open]="connOpen()"
            viewBox="0 0 16 16"
            width="14"
            height="14"
            fill="none"
            aria-hidden="true"
          >
            <path
              d="M5.5 4l5 4-5 4"
              stroke="currentColor"
              stroke-width="1.7"
              stroke-linecap="round"
              stroke-linejoin="round"
            />
          </svg>
          <span class="b-conn-title">Connections</span>
          @if (nodeCount() > 0) {
            <span class="count b-conn-count">{{ nodeCount() }}</span>
          }
          <span class="b-conn-sub">
            How people and projects link across your brain.
          </span>
        </button>

        @if (connOpen()) {
          @if (graphLoading()) {
            <div class="card state-card">
              <p class="empty">Loading the map…</p>
            </div>
          } @else if (graphError()) {
            <div class="card empty-state">
              <span class="empty-mark" aria-hidden="true"></span>
              <p class="empty-title">Couldn’t load the map</p>
              <p class="empty">{{ graphError() }}</p>
            </div>
          } @else if (nodeCount() === 0) {
            <div class="card empty-state">
              <span class="empty-mark" aria-hidden="true"></span>
              <p class="empty-title">The map builds itself as you record</p>
              <p class="empty">
                As Murmur recognises the people and projects you talk about,
                they’ll appear here — connected by the meetings they share.
              </p>
            </div>
          } @else {
            @if (disclosure(); as msg) {
              <div class="banner is-accent b-banner" role="status">
                <span class="b-banner-glyph" aria-hidden="true"></span>
                <span>{{ msg }}</span>
              </div>
            }
            <app-brain-map [data]="graphData()" />
          }
        }
      </section>

      @if (noteEditorOpen()) {
        <app-brain-note-editor
          [saving]="savingNote()"
          (save)="onSaveNote($event)"
          (dismiss)="closeNoteEditor()"
        />
      }
    </section>
  `,
  styles: [
    `
      :host {
        display: block;
      }
      .brain {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
      }

      /* — status header — a designed anchor bar, not a bare count line — */
      .b-head {
        position: relative;
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
        padding: var(--space-5) var(--space-5) var(--space-4);
        overflow: hidden;
      }
      /* A faint accent wash gives the header intentional visual weight. */
      .b-head::before {
        content: "";
        position: absolute;
        inset: 0;
        pointer-events: none;
        background: radial-gradient(
          130% 150% at 0% 0%,
          rgba(110, 118, 255, 0.12),
          transparent 55%
        );
      }
      .b-head > * {
        position: relative;
        z-index: 1;
      }
      .b-head-top {
        display: flex;
        align-items: center;
        flex-wrap: wrap;
        gap: var(--space-4) var(--space-5);
      }
      .b-identity {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        min-width: 0;
      }
      .b-mark {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        flex: none;
        width: 44px;
        height: 44px;
        border-radius: var(--radius-md);
        background: var(--accent-soft);
        border: 1px solid var(--accent-ring);
        font-size: 1.4rem;
        line-height: 1;
      }
      .b-identity-text {
        display: flex;
        flex-direction: column;
        gap: 2px;
        min-width: 0;
      }
      .b-title {
        margin: 0;
        font-size: 1.35rem;
        letter-spacing: -0.01em;
      }
      .b-tagline {
        margin: 0;
        color: var(--text-secondary);
        font-size: 0.85rem;
      }

      /* Whole-brain stat units — number over label, divider-separated. */
      .b-stats {
        display: flex;
        align-items: center;
        justify-content: center;
        gap: var(--space-4);
        margin: 0;
        flex: 1 1 auto;
      }
      .b-stat {
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: 2px;
      }
      .b-stat dd {
        margin: 0;
        color: var(--text-primary);
        font-family: var(--font-mono);
        font-size: 1.5rem;
        font-weight: 500;
        font-variant-numeric: tabular-nums;
        line-height: 1.05;
      }
      .b-stat dt {
        color: var(--text-muted);
        font-size: 0.6875rem;
        font-weight: 600;
        letter-spacing: 0.06em;
        text-transform: uppercase;
      }
      .b-stat-sep {
        flex: none;
        width: 1px;
        height: 30px;
        background: var(--border-subtle);
      }
      .b-summary-state {
        margin: 0;
        flex: 1 1 auto;
        text-align: center;
        color: var(--text-muted);
        font-size: 0.9rem;
      }
      .b-summary-err {
        color: var(--danger);
      }

      .b-head-side {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        flex: none;
        margin-left: auto;
      }
      .b-badge {
        color: var(--text-muted);
      }
      .b-badge-dot {
        flex: none;
        width: 7px;
        height: 7px;
        border-radius: var(--radius-pill);
        background: currentColor;
        opacity: 0.55;
      }
      .b-badge.is-on {
        border-color: transparent;
        color: var(--success);
        background: var(--success-soft);
      }
      .b-badge.is-on .b-badge-dot {
        opacity: 1;
        box-shadow: 0 0 8px var(--success);
      }
      .b-badge-action {
        border-color: transparent;
        color: var(--accent-hover);
        background: var(--accent-soft);
        text-decoration: none;
        transition:
          filter var(--transition),
          transform var(--transition-fast);
      }
      .b-badge-action:hover {
        filter: brightness(1.12);
      }
      .b-badge-action:active {
        transform: translateY(1px);
      }
      .b-badge-action:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .b-ask {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
      }
      .b-nudge {
        align-items: center;
      }
      .b-nudge-glyph {
        flex: none;
        width: 8px;
        height: 8px;
        border-radius: var(--radius-pill);
        background: var(--accent-hover);
        box-shadow: 0 0 0 4px var(--accent-soft);
      }

      /* — sources — */
      .b-sources {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .b-sec-head {
        display: flex;
        align-items: flex-end;
        justify-content: space-between;
        flex-wrap: wrap;
        gap: var(--space-3);
      }
      .b-sec-text {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        min-width: 0;
      }
      .b-sec-title {
        margin: 0;
        font-size: 1.0625rem;
      }
      .b-sec-sub {
        margin: 0;
        color: var(--text-secondary);
        font-size: 0.875rem;
      }
      .b-folder {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        min-width: 220px;
      }
      .b-folder-label {
        color: var(--text-muted);
        font-size: 0.75rem;
        font-weight: 600;
        letter-spacing: 0.04em;
        text-transform: uppercase;
      }
      .b-folder-select {
        width: 100%;
        height: 38px;
      }
      .b-add-hint {
        color: var(--text-muted);
        font-size: 0.75rem;
        line-height: 1.3;
      }
      .b-cards {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
        gap: var(--space-4);
        align-items: start;
      }
      .b-list-err {
        margin: 0;
        color: var(--danger);
      }

      /* — connections (collapsible) — */
      .b-conn {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .b-conn-toggle {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        width: 100%;
        padding: var(--space-2) 0;
        background: none;
        border: none;
        color: var(--text-primary);
        cursor: pointer;
        text-align: left;
      }
      .b-conn-chevron {
        flex: none;
        color: var(--text-muted);
        transition: transform var(--transition);
      }
      .b-conn-chevron.is-open {
        transform: rotate(90deg);
      }
      .b-conn-title {
        font-size: 1.0625rem;
        font-weight: 600;
      }
      .b-conn-count {
        flex: none;
      }
      .b-conn-sub {
        color: var(--text-secondary);
        font-size: 0.875rem;
      }

      .b-banner {
        align-items: center;
        animation: b-rise 320ms var(--transition) both;
      }
      .b-banner-glyph {
        flex: none;
        width: 8px;
        height: 8px;
        border-radius: var(--radius-pill);
        background: var(--accent-hover);
        box-shadow: 0 0 0 4px var(--accent-soft);
      }
      @keyframes b-rise {
        from {
          opacity: 0;
          transform: translateY(6px);
        }
      }
      @media (prefers-reduced-motion: reduce) {
        .b-banner,
        .b-conn-chevron {
          animation: none;
          transition: none;
        }
      }

      @media (max-width: 640px) {
        .b-stats {
          justify-content: flex-start;
          flex: 1 1 100%;
          order: 3;
        }
        .b-head-side {
          margin-left: 0;
        }
      }
    `,
  ],
})
export class BrainComponent {
  private readonly ipc = inject(IpcService);
  private readonly folders = inject(FoldersService);
  private readonly toast = inject(ToastService);

  // ── status header ──────────────────────────────────────────────────────
  readonly overview = signal<BrainOverview | null>(null);
  readonly overviewError = signal(false);

  protected readonly semanticOn = computed(() => {
    const ov = this.overview();
    return !!ov && ov.semanticEnabled && ov.embedModelPresent;
  });

  /** True when the on-device search model hasn't been downloaded yet. */
  protected readonly modelMissing = computed(() => {
    const ov = this.overview();
    return !!ov && !ov.embedModelPresent;
  });

  /**
   * Plain-language semantic-search status for the header chip — NEVER internal
   * jargon (no "e5" embedder name). The model-missing case is handled by a
   * separate actionable chip-link ({@link modelMissing}), so this only covers
   * on / off.
   */
  protected readonly semanticBadge = computed(() => {
    const ov = this.overview();
    if (!ov) {
      return "Checking search…";
    }
    return ov.semanticEnabled
      ? "AI semantic search: on"
      : "Semantic search: off";
  });

  /**
   * Human explanation for the Settings nudge when semantic search isn't fully
   * set up — model-missing vs turned-off get different, actionable copy; null
   * when it's fully on (no nudge). Says WHAT it does ("search by meaning, not
   * just keywords"), never the embedder's internal name.
   */
  protected readonly nudge = computed<string | null>(() => {
    const ov = this.overview();
    if (!ov) {
      return null;
    }
    if (!ov.embedModelPresent) {
      return "Download the AI search model to search your brain by meaning, not just keywords.";
    }
    if (!ov.semanticEnabled) {
      return "Turn on semantic search to find your notes by meaning, not just keywords.";
    }
    return null;
  });

  // ── folder selector ────────────────────────────────────────────────────
  readonly selectedFolderId = signal<string | null>(null);

  /** True when the selector is on the default "All folders" aggregate view. */
  protected readonly isAll = computed(
    () => this.selectedFolderId() === ALL_FOLDERS_ID,
  );

  /**
   * The selector options: a leading "All folders" aggregate (the DEFAULT) plus
   * every folder in the tree, depth-indented + lock-glyphed. Empty when there
   * are no folders at all (nothing to aggregate over or add to).
   */
  protected readonly folderOptions = computed<FolderOption[]>(() => {
    const real: FolderOption[] = [];
    const walk = (nodes: FolderNode[], depth: number): void => {
      for (const node of nodes) {
        const sealed = node.locked && !node.unlocked;
        const indent = depth > 0 ? "  ".repeat(depth) + "↳ " : "";
        real.push({
          id: node.id,
          label: `${indent}${node.name}${sealed ? " 🔒" : ""}`,
          name: node.name,
          blocked: sealed,
        });
        if (node.children?.length) {
          walk(node.children, depth + 1);
        }
      }
    };
    walk(this.folders.tree(), 0);
    if (real.length === 0) {
      return [];
    }
    return [
      {
        id: ALL_FOLDERS_ID,
        label: "All folders",
        name: "All folders",
        blocked: false,
      },
      ...real,
    ];
  });

  /** Every real (non-sentinel) folder option — the aggregation set for "All". */
  private readonly realFolderOptions = computed(() =>
    this.folderOptions().filter((o) => o.id !== ALL_FOLDERS_ID),
  );

  /**
   * The folder an "Add" actually targets. A specific selection targets itself;
   * you can't add to the "All" aggregate, so under "All" adds default to the
   * first ADDABLE (unlocked) folder — falling back to the first folder (whose
   * sealed state then disables the add). Null when there are no folders.
   */
  private readonly addTarget = computed<FolderOption | null>(() => {
    const opts = this.realFolderOptions();
    if (this.isAll()) {
      return opts.find((o) => !o.blocked) ?? opts[0] ?? null;
    }
    const id = this.selectedFolderId();
    return opts.find((o) => o.id === id) ?? null;
  });

  /** The id an "Add" targets (see {@link addTarget}); null when none. */
  protected readonly addTargetId = computed(() => this.addTarget()?.id ?? null);

  /** True when adds are blocked (no target, or the target folder is sealed). */
  protected readonly selectedBlocked = computed(() => {
    const target = this.addTarget();
    return !target || target.blocked;
  });

  /**
   * Subtle "adding to X" hint under the selector — only in the "All" view (where
   * the add target isn't obvious) and only when adding is actually possible.
   */
  protected readonly addHint = computed<string | null>(() => {
    if (!this.isAll()) {
      return null;
    }
    const target = this.addTarget();
    if (!target || target.blocked) {
      return null;
    }
    return `New documents and notes are added to “${target.name}”.`;
  });

  // ── documents + notes (one list, split by kind) ────────────────────────
  private readonly items = signal<DocumentInfo[]>([]);
  readonly listLoading = signal(false);
  readonly listError = signal<string | null>(null);
  readonly importing = signal(false);
  readonly savingNote = signal(false);
  readonly deletingId = signal<string | null>(null);

  readonly documents = computed(() =>
    this.items().filter((d) => d.kind !== "note"),
  );
  readonly notes = computed(() => this.items().filter((d) => d.kind === "note"));

  readonly docsExpanded = signal(false);
  readonly notesExpanded = signal(false);

  // ── note editor modal ──────────────────────────────────────────────────
  readonly noteEditorOpen = signal(false);

  // ── connections (graph) ────────────────────────────────────────────────
  readonly graphData = signal<GraphData | null>(null);
  readonly graphLoading = signal(true);
  readonly graphError = signal<string | null>(null);
  readonly connOpen = signal(false);

  protected readonly nodeCount = computed(
    () => this.graphData()?.nodes.length ?? 0,
  );

  protected readonly disclosure = computed<string | null>(() => {
    const d = this.graphData();
    if (!d) {
      return null;
    }
    const capped = d.nodes.length > MAP_NODE_CAP;
    if (d.hasHidden && capped) {
      return `Showing the ${MAP_NODE_CAP} most-connected entities. More are hidden in locked folders — unlock to include them.`;
    }
    if (capped) {
      return `Showing the ${MAP_NODE_CAP} most-connected of ${d.nodes.length} entities.`;
    }
    if (d.hasHidden) {
      return "Some entities are hidden — unlock a folder to include them.";
    }
    return null;
  });

  constructor() {
    // Ensure the folder tree is loaded so the selector has options.
    void this.folders.load();

    // Default the selection to "All folders" once options resolve, and keep it
    // valid if the tree changes (a removed folder falls back to "All"). Tracked
    // effect that writes the selection (NG0600 guard). ALL_FOLDERS_ID is always
    // present whenever there is at least one folder.
    effect(
      () => {
        const opts = this.folderOptions();
        const cur = this.selectedFolderId();
        if (opts.length === 0) {
          if (cur !== null) {
            this.selectedFolderId.set(null);
          }
          return;
        }
        if (cur === null || !opts.some((o) => o.id === cur)) {
          this.selectedFolderId.set(ALL_FOLDERS_ID);
        }
      },
      { allowSignalWrites: true },
    );

    // (Re)load the OVERVIEW whenever the folder lock-state changes — a session
    // unlock/relock shifts the visible counts. Reading `tree()` registers the
    // dependency; the fetch writes signals synchronously before its first await,
    // so writes must be allowed (NG0600 guard).
    effect(
      () => {
        this.folders.tree();
        void this.fetchOverview();
      },
      { allowSignalWrites: true },
    );

    // (Re)load the document/note LIST whenever the selected folder OR the lock
    // state changes (a sealed folder masks to empty). Under "All" this aggregates
    // across every visible folder. Same NG0600 shape.
    effect(
      () => {
        const id = this.selectedFolderId();
        this.folders.tree();
        if (!id) {
          this.items.set([]);
          this.listLoading.set(false);
          return;
        }
        this.listLoading.set(true);
        this.listError.set(null);
        if (id === ALL_FOLDERS_ID) {
          void this.fetchAllItems();
        } else {
          void this.fetchItems(id);
        }
      },
      { allowSignalWrites: true },
    );

    // (Re)load the GRAPH whenever the folder lock-state changes (mirrors the old
    // /graph page — sealed entities drop out / reappear live). Same NG0600 shape.
    effect(
      () => {
        this.folders.tree();
        void this.fetchGraph();
      },
      { allowSignalWrites: true },
    );
  }

  private async fetchOverview(): Promise<void> {
    this.overviewError.set(false);
    try {
      this.overview.set(await this.ipc.brainOverview());
    } catch {
      this.overview.set(null);
      this.overviewError.set(true);
    }
  }

  private async fetchItems(folderId: string): Promise<void> {
    try {
      const docs = await this.ipc.listDocuments(folderId);
      // Stale-result guard: drop a response for a folder we've since left.
      if (this.selectedFolderId() !== folderId) {
        return;
      }
      this.items.set(docs);
    } catch (e) {
      if (this.selectedFolderId() !== folderId) {
        return;
      }
      this.items.set([]);
      this.listError.set(String(e));
    } finally {
      if (this.selectedFolderId() === folderId) {
        this.listLoading.set(false);
      }
    }
  }

  /**
   * Aggregate documents/notes across EVERY folder for the default "All folders"
   * view (so the source cards match the whole-brain header counts). Each
   * per-folder call is independently gated server-side — a sealed-not-unlocked
   * folder returns an EMPTY list (never a name behind the lock) — and a
   * per-folder failure degrades to [] rather than failing the whole aggregate.
   * Stale-guarded: a response is dropped if the user has since left "All".
   */
  private async fetchAllItems(): Promise<void> {
    const ids = this.realFolderOptions().map((o) => o.id);
    try {
      const perFolder = await Promise.all(
        ids.map((id) =>
          this.ipc.listDocuments(id).catch(() => [] as DocumentInfo[]),
        ),
      );
      if (this.selectedFolderId() !== ALL_FOLDERS_ID) {
        return;
      }
      this.items.set(perFolder.flat());
    } finally {
      if (this.selectedFolderId() === ALL_FOLDERS_ID) {
        this.listLoading.set(false);
      }
    }
  }

  private async fetchGraph(): Promise<void> {
    this.graphError.set(null);
    try {
      this.graphData.set(await this.ipc.getGraph());
    } catch (e) {
      this.graphData.set(null);
      this.graphError.set(String(e));
    } finally {
      this.graphLoading.set(false);
    }
  }

  protected onFolderChange(event: Event): void {
    this.selectedFolderId.set((event.target as HTMLSelectElement).value);
  }

  /** Open the native file dialog (md/txt) → import the chosen path as a document. */
  async pickAndImportDocument(): Promise<void> {
    const folderId = this.addTargetId();
    if (!folderId || this.selectedBlocked() || this.importing()) {
      return;
    }
    const chosen = await open({
      multiple: false,
      filters: [{ name: "Documents", extensions: ["md", "txt"] }],
    });
    if (typeof chosen !== "string") {
      return;
    }

    this.importing.set(true);
    try {
      await this.ipc.importDocument(chosen, folderId);
      this.toast.success("Document added to the brain.");
      this.docsExpanded.set(true);
      await this.afterMutation();
    } catch (e) {
      this.toast.danger(this.friendlyImportError(e));
    } finally {
      this.importing.set(false);
    }
  }

  openNoteEditor(): void {
    if (this.selectedBlocked()) {
      return;
    }
    this.noteEditorOpen.set(true);
  }

  closeNoteEditor(): void {
    if (this.savingNote()) {
      return;
    }
    this.noteEditorOpen.set(false);
  }

  /** Ingest a typed note → refresh + toast + close the editor. */
  async onSaveNote(payload: { name: string; text: string }): Promise<void> {
    const folderId = this.addTargetId();
    if (!folderId || this.selectedBlocked() || this.savingNote()) {
      return;
    }
    if (!payload.text.trim()) {
      return;
    }
    this.savingNote.set(true);
    try {
      await this.ipc.importText(payload.name, payload.text, folderId);
      this.toast.success("Note added to the brain.");
      this.notesExpanded.set(true);
      this.noteEditorOpen.set(false);
      await this.afterMutation();
    } catch (e) {
      this.toast.danger(this.friendlyImportError(e));
    } finally {
      this.savingNote.set(false);
    }
  }

  /** Permanently delete a document/note, then refresh the list + overview. */
  async removeItem(doc: DocumentInfo): Promise<void> {
    if (this.deletingId()) {
      return;
    }
    this.deletingId.set(doc.id);
    try {
      await this.ipc.deleteDocument(doc.id);
      this.toast.info(`Removed “${doc.name}”.`);
      await this.afterMutation();
    } catch (e) {
      this.toast.danger(this.friendlyDeleteError(e));
    } finally {
      this.deletingId.set(null);
    }
  }

  /** Re-fetch the visible list for the CURRENT selection (a folder or "All"). */
  private async refreshList(): Promise<void> {
    const sel = this.selectedFolderId();
    if (!sel) {
      this.items.set([]);
      return;
    }
    if (sel === ALL_FOLDERS_ID) {
      await this.fetchAllItems();
    } else {
      await this.fetchItems(sel);
    }
  }

  /** Re-fetch the visible list + the whole-brain header counts after a change. */
  private async afterMutation(): Promise<void> {
    await this.refreshList();
    await this.fetchOverview();
  }

  private friendlyImportError(e: unknown): string {
    const msg = String(e);
    if (/lock/i.test(msg)) {
      return "That folder is locked — unlock it first to add to the brain.";
    }
    if (/\.md and \.txt|only .*md|invalid/i.test(msg)) {
      return "Only Markdown (.md) and text (.txt) files can be imported.";
    }
    return "Couldn’t add that to the brain. Please try again.";
  }

  private friendlyDeleteError(e: unknown): string {
    const msg = String(e);
    if (/lock/i.test(msg)) {
      return "That folder is locked — unlock it first to delete its items.";
    }
    return "Couldn’t remove that. Please try again.";
  }
}
