import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import { IpcService } from "../../../core/ipc.service";
import type {
  BrainOverview,
  DocImportProgress,
  DocumentInfo,
  FolderNode,
  GraphData,
} from "../../../core/models";
import { FoldersService } from "../../../services/folders.service";
import { ToastService } from "../../../services/toast.service";
import { BrainEnableCardComponent } from "../brain-enable-card/brain-enable-card.component";
import { BrainMapComponent } from "../brain-map/brain-map.component";
import { FullBrainGraphComponent } from "../full-brain-graph/full-brain-graph.component";
import { BrainMemoryComponent } from "../brain-memory/brain-memory.component";
import { BrainNoteEditorComponent } from "../brain-note-editor/brain-note-editor.component";
import { BrainSourceCardComponent } from "../brain-source-card/brain-source-card.component";
import { BriefsComponent } from "../../briefs/briefs/briefs.component";
import { AuditComponent } from "../../audit/audit/audit.component";

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
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    RouterLink,
    BrainEnableCardComponent,
    BrainSourceCardComponent,
    BrainNoteEditorComponent,
    BrainMapComponent,
    FullBrainGraphComponent,
    BrainMemoryComponent,
    BriefsComponent,
    AuditComponent,
  ],
  templateUrl: "./brain.component.html",
  styleUrl: "./brain.component.scss",
})
export class BrainComponent {
  private readonly ipc = inject(IpcService);
  private readonly folders = inject(FoldersService);
  private readonly toast = inject(ToastService);
  private readonly destroyRef = inject(DestroyRef);

  // ── "Enable the brain" nudge — shown only when an on-device model is missing ─
  /**
   * Both on-device models present? null = unknown (probe in flight / failed).
   * The nudge card renders ONLY when this is confirmed `false` (never flashes on
   * an already-set-up brain, never shows behind a failed probe). Hoisted here
   * rather than read off the child (a template-ref can't gate its own creation);
   * the card re-probes and emits `enabled` when the download lands.
   */
  readonly brainReady = signal<boolean | null>(null);

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

  /**
   * Latest progress for the in-flight document import (from `EVENT_DOC_IMPORT`),
   * or null when no import is running. Fed by the `onDocImportProgress` listener
   * subscribed once in the constructor; cleared when the import settles. Counts +
   * stage only — NO PII (see {@link DocImportProgress}).
   */
  private readonly importProgress = signal<DocImportProgress | null>(null);

  /**
   * A short, human progress line for the importing Documents card, e.g.
   * "Extracting…" or "Embedding 12/40". Null unless an import is running.
   */
  readonly importProgressLabel = computed<string | null>(() => {
    if (!this.importing()) {
      return null;
    }
    const p = this.importProgress();
    if (!p) {
      return "Preparing…";
    }
    switch (p.stage) {
      case "extracting":
        return "Extracting text…";
      case "chunking":
        return "Chunking…";
      case "embedding":
        return p.total > 0
          ? `Embedding ${p.done}/${p.total}`
          : "Embedding…";
      case "done":
        return "Finishing…";
      default:
        return "Working…";
    }
  });

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

  /**
   * The FULL-BRAIN graph section (entities + meetings + notes + documents as one
   * typed graph, Brain v3 PR-4) — collapsed by default; its component self-loads
   * via IPC + the folder tree, so it costs nothing until opened.
   */
  readonly fullBrainOpen = signal(false);

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

    // Probe whether the two on-device models are present (drives the nudge).
    void this.probeBrainReady();

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
    );

    // (Re)load the GRAPH whenever the folder lock-state changes (mirrors the old
    // /graph page — sealed entities drop out / reappear live). Same NG0600 shape.
    effect(
      () => {
        this.folders.tree();
        void this.fetchGraph();
      },
    );

    // Subscribe ONCE to the document-import progress stream (extract → chunk →
    // embed). Payloads are counts + stage only (NO PII); we push the latest into
    // `importProgress` so the Documents card shows live progress instead of a
    // frozen "Adding…". The listener is released on teardown (RecorderStore.init
    // idiom). `import_document` is a one-shot, so no stale-result guard is needed
    // — the `importing` flag already gates a second import.
    let unlisten: UnlistenFn | null = null;
    void this.ipc
      .onDocImportProgress((p) => this.importProgress.set(p))
      .then((fn) => {
        unlisten = fn;
      });
    this.destroyRef.onDestroy(() => {
      unlisten?.();
    });
  }

  /** Probe both on-device models; null on any failure (nudge stays hidden). */
  private async probeBrainReady(): Promise<void> {
    try {
      const [brain, embed] = await Promise.all([
        this.ipc.brainModelPresent(),
        this.ipc.embedModelPresent(),
      ]);
      this.brainReady.set(brain && embed);
    } catch {
      this.brainReady.set(null);
    }
  }

  /** The card landed both models — hide the nudge + refresh the header counts. */
  protected onBrainEnabled(): void {
    this.brainReady.set(true);
    void this.fetchOverview();
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

  /**
   * Open the native file dialog (Markdown / text / PDF / Word / PowerPoint /
   * Excel / HTML / image) → import the chosen path as a document. The backend
   * extracts the text — scanned PDFs and images run through on-device Vision
   * OCR — then chunks + embeds it behind the RAM floor, streaming progress
   * over `EVENT_DOC_IMPORT` (surfaced on the card via {@link importProgressLabel}).
   */
  async pickAndImportDocument(): Promise<void> {
    const folderId = this.addTargetId();
    if (!folderId || this.selectedBlocked() || this.importing()) {
      return;
    }
    const chosen = await open({
      multiple: false,
      filters: [
        {
          name: "Documents",
          extensions: [
            "md",
            "txt",
            "pdf",
            "docx",
            "pptx",
            "xlsx",
            "html",
            "htm",
            "png",
            "jpg",
            "jpeg",
            "heic",
            "tiff",
            "tif",
            "bmp",
            "gif",
          ],
        },
      ],
    });
    if (typeof chosen !== "string") {
      return;
    }

    this.importProgress.set(null);
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
      this.importProgress.set(null);
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

  /**
   * Map a raw backend error string into a friendly, actionable message. The
   * backend serializes `AppError` as `to_string()` — so the strings we match
   * are the `import_document` / `extract` messages, prefixed by the variant tag
   * (`"locked: …"`, `"invalid argument: …"`). Order matters: the SPECIFIC
   * extraction failures are checked before the generic "unsupported type" one.
   */
  private friendlyImportError(e: unknown): string {
    const msg = String(e);
    // Write-gate: a sealed-not-unlocked folder ("locked: …").
    if (/^locked:|\block/i.test(msg)) {
      return "That folder is locked — unlock it first to add to the brain.";
    }
    // OCR ran but found nothing — a scanned PDF or an image with no readable
    // text ("no text found in this document, even with OCR" / "no text found
    // in this image"). Scanned PDFs now OCR automatically, so this is the only
    // remaining no-text failure.
    if (/no text found/i.test(msg)) {
      return "No readable text found in that file, even after OCR.";
    }
    // Encrypted / password-protected PDF.
    if (/password-protected|password|encrypted/i.test(msg)) {
      return "That PDF is password-protected — unlock it and try again.";
    }
    // Corrupt / unreadable / malformed file (bad zip, malformed XML, unreadable PDF).
    if (
      /could not read|could not open|could not render|not a valid|corrupt|malformed|bad (docx|pptx)/i.test(
        msg,
      )
    ) {
      return "That file couldn’t be read — it may be corrupt or in an unexpected format.";
    }
    // Unsupported extension (allowlist reject) — the catch-all invalid-argument case.
    if (/unsupported document type|only .*md|invalid argument/i.test(msg)) {
      return "That file type can’t be imported. Try Markdown, text, PDF, Word, PowerPoint, Excel, or HTML.";
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
