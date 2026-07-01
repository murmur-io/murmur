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

/** A flattened folder option for the selector (indent reflects tree depth). */
interface FolderOption {
  id: string;
  /** Display label with a depth-indent prefix + a lock glyph when sealed. */
  label: string;
  /** Sealed-and-NOT-session-unlocked → add/list/delete are blocked. */
  blocked: boolean;
}

/**
 * The `/brain` page — "what's in my brain" (ClickUp-way knowledge sources).
 *
 * Top → bottom:
 *  1. STATUS HEADER — a one-line count bar (🧠 N meetings · N documents · N
 *     notes) + a semantic badge, a gentle Settings nudge when semantic search /
 *     the e5 model isn't set up, and an [Ask ↗] link. Data from `brainOverview()`.
 *  2. KNOWLEDGE SOURCES — three in-flow `.card`s (delegated to
 *     {@link BrainSourceCardComponent}): 🎙 Meetings (read-only, links to
 *     /library), 📄 Documents (upload `.md`/`.txt`), 📝 Notes (type text). A
 *     folder selector governs which folder's docs/notes are listed / added to;
 *     a sealed folder fails closed (add disabled + a note).
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
        <div class="b-head-main">
          <h2 class="b-title">Brain</h2>
          @if (overview(); as ov) {
            <p class="b-counts">
              <span class="b-brainmark" aria-hidden="true">🧠</span>
              <span>{{ ov.meetingCount }} meetings</span>
              <span class="b-dot" aria-hidden="true">·</span>
              <span>{{ ov.documentCount }} documents</span>
              <span class="b-dot" aria-hidden="true">·</span>
              <span>{{ ov.noteCount }} notes</span>
            </p>
          } @else if (overviewError()) {
            <p class="b-counts b-counts-err">Couldn’t load the brain summary.</p>
          } @else {
            <p class="b-counts b-counts-muted">Loading the brain summary…</p>
          }
        </div>

        <div class="b-head-side">
          <span class="pill b-badge" [class.is-on]="semanticOn()">
            {{ semanticBadge() }}
          </span>
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

        @if (showNudge()) {
          <div class="banner is-accent b-nudge" role="status">
            <span class="b-nudge-glyph" aria-hidden="true"></span>
            <span>
              Turn on semantic search and download the e5 model in
              <a routerLink="/settings">Settings</a> to vectorize your brain for
              meaning-based recall.
            </span>
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
            subtitle="Markdown / text files you’ve added to this folder."
            [count]="documents().length"
            [items]="documents()"
            [expanded]="docsExpanded()"
            [loading]="listLoading()"
            [busy]="importing()"
            [deletingId]="deletingId()"
            [blocked]="selectedBlocked()"
            addLabel="Add document"
            emptyLabel="No documents in this folder yet."
            (add)="pickAndImportDocument()"
            (deleteItem)="removeItem($event)"
            (toggleList)="docsExpanded.set(!docsExpanded())"
          />

          <app-brain-source-card
            glyph="📝"
            title="Notes"
            subtitle="Typed or pasted text you’ve added to this folder."
            [count]="notes().length"
            [items]="notes()"
            [expanded]="notesExpanded()"
            [loading]="listLoading()"
            [busy]="savingNote()"
            [deletingId]="deletingId()"
            [blocked]="selectedBlocked()"
            addLabel="Add note"
            emptyLabel="No notes in this folder yet."
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
        gap: var(--space-6);
      }

      /* — status header — */
      .b-head {
        display: grid;
        grid-template-columns: 1fr auto;
        align-items: center;
        gap: var(--space-3) var(--space-4);
      }
      .b-head-main {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        min-width: 0;
      }
      .b-title {
        margin: 0;
      }
      .b-counts {
        margin: 0;
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: var(--space-2);
        color: var(--text-secondary);
        font-size: 0.9375rem;
      }
      .b-brainmark {
        font-size: 1.05rem;
        line-height: 1;
      }
      .b-dot {
        color: var(--text-muted);
      }
      .b-counts-muted {
        color: var(--text-muted);
      }
      .b-counts-err {
        color: var(--danger);
      }
      .b-head-side {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        justify-self: end;
      }
      .b-badge {
        border-color: var(--border-strong);
        color: var(--text-muted);
      }
      .b-badge.is-on {
        border-color: var(--accent-ring);
        color: var(--accent-hover);
        background: var(--accent-soft);
      }
      .b-ask {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
      }
      .b-nudge {
        grid-column: 1 / -1;
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
        .b-head {
          grid-template-columns: 1fr;
        }
        .b-head-side {
          justify-self: start;
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

  /** The semantic status badge text (on / off / model-missing). */
  protected readonly semanticBadge = computed(() => {
    const ov = this.overview();
    if (!ov) {
      return "Semantic…";
    }
    if (!ov.embedModelPresent) {
      return "Model not downloaded";
    }
    return ov.semanticEnabled ? "Semantic on · e5 ✓" : "Semantic off";
  });

  /** Show the Settings nudge when semantic search isn't fully set up. */
  protected readonly showNudge = computed(() => {
    const ov = this.overview();
    return !!ov && (!ov.semanticEnabled || !ov.embedModelPresent);
  });

  // ── folder selector ────────────────────────────────────────────────────
  readonly selectedFolderId = signal<string | null>(null);

  protected readonly folderOptions = computed<FolderOption[]>(() => {
    const out: FolderOption[] = [];
    const walk = (nodes: FolderNode[], depth: number): void => {
      for (const node of nodes) {
        const sealed = node.locked && !node.unlocked;
        const indent = depth > 0 ? "  ".repeat(depth) + "↳ " : "";
        out.push({
          id: node.id,
          label: `${indent}${node.name}${sealed ? " 🔒" : ""}`,
          blocked: sealed,
        });
        if (node.children?.length) {
          walk(node.children, depth + 1);
        }
      }
    };
    walk(this.folders.tree(), 0);
    return out;
  });

  protected readonly selectedBlocked = computed(() => {
    const id = this.selectedFolderId();
    return this.folderOptions().some((o) => o.id === id && o.blocked);
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

    // Default the selection to the first folder once options resolve, and keep
    // it valid if the tree changes. Tracked effect that writes the selection.
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
          this.selectedFolderId.set(opts[0].id);
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
    // state changes (a sealed folder masks to empty). Same NG0600 shape.
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
        void this.fetchItems(id);
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
    const folderId = this.selectedFolderId();
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
      await this.afterMutation(folderId);
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
    const folderId = this.selectedFolderId();
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
      await this.afterMutation(folderId);
    } catch (e) {
      this.toast.danger(this.friendlyImportError(e));
    } finally {
      this.savingNote.set(false);
    }
  }

  /** Permanently delete a document/note, then refresh the list + overview. */
  async removeItem(doc: DocumentInfo): Promise<void> {
    const folderId = this.selectedFolderId();
    if (this.deletingId()) {
      return;
    }
    this.deletingId.set(doc.id);
    try {
      await this.ipc.deleteDocument(doc.id);
      this.toast.info(`Removed “${doc.name}”.`);
      if (folderId) {
        await this.afterMutation(folderId);
      }
    } catch (e) {
      this.toast.danger(this.friendlyDeleteError(e));
    } finally {
      this.deletingId.set(null);
    }
  }

  /** Re-fetch the list (if still on the same folder) + the header counts. */
  private async afterMutation(folderId: string): Promise<void> {
    if (this.selectedFolderId() === folderId) {
      await this.fetchItems(folderId);
    }
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
