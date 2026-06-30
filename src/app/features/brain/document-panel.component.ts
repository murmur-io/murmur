import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import { open } from "@tauri-apps/plugin-dialog";
import { IpcService } from "../../core/ipc.service";
import type { DocumentInfo, FolderNode } from "../../core/models";
import { FoldersService } from "../../services/folders.service";
import { ToastService } from "../../services/toast.service";

/** A flattened folder option for the selector (indent reflects tree depth). */
interface FolderOption {
  id: string;
  /** Display label with a depth-indent prefix + a lock glyph when sealed. */
  label: string;
  /** Sealed-and-NOT-session-unlocked → import/list/delete are blocked. */
  blocked: boolean;
}

/**
 * The DOCUMENTS panel of the Brain view: pick a folder, upload `.md`/`.txt`
 * documents into it to expand the brain, list them, delete them.
 *
 * Upload flow: the native file dialog (`@tauri-apps/plugin-dialog` `open`,
 * filtered to `.md`/`.txt`) → on a chosen path, `importDocument(path, folderId)`
 * → refresh the list + a success toast. The backend is the authority on the
 * lock gate: a sealed folder rejects with `AppError::Locked`, surfaced as a
 * clear danger toast (and the affordance is pre-disabled for a sealed-selected
 * folder).
 *
 * The list re-fetches whenever the selected folder OR the folder lock-state
 * changes (a session unlock/relock shifts what `list_documents` returns — a
 * sealed folder is masked to empty), so it never shows stale docs.
 */
@Component({
  selector: "app-document-panel",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [],
  template: `
    <section class="dp card">
      <header class="dp-head">
        <div class="dp-head-text">
          <h3 class="dp-title">Documents</h3>
          <p class="dp-sub">
            Add Markdown or text files to expand what the brain knows. They’re
            indexed alongside your meeting notes.
          </p>
        </div>
        <button
          type="button"
          class="btn btn-primary dp-add"
          [disabled]="importing() || !selectedFolderId() || selectedBlocked()"
          (click)="pickAndImport()"
        >
          @if (importing()) {
            Adding…
          } @else {
            <svg
              viewBox="0 0 16 16"
              width="14"
              height="14"
              fill="none"
              aria-hidden="true"
            >
              <path
                d="M8 3.5v9M3.5 8h9"
                stroke="currentColor"
                stroke-width="1.7"
                stroke-linecap="round"
              />
            </svg>
            Add document
          }
        </button>
      </header>

      <label class="dp-folder">
        <span class="dp-folder-label">Folder</span>
        <select
          class="dp-folder-select"
          aria-label="Choose a folder"
          [value]="selectedFolderId() ?? ''"
          (change)="onFolderChange($event)"
        >
          @for (o of folderOptions(); track o.id) {
            <option [value]="o.id">{{ o.label }}</option>
          }
        </select>
      </label>

      @if (selectedBlocked()) {
        <div class="banner is-accent dp-locked" role="status">
          <span class="dp-locked-glyph" aria-hidden="true"></span>
          <span>
            This folder is locked. Unlock it (in Meetings → folders) to add or
            view its documents.
          </span>
        </div>
      }

      @if (loading()) {
        <p class="empty dp-state">Loading documents…</p>
      } @else if (error()) {
        <p class="empty dp-state">{{ error() }}</p>
      } @else if (documents().length === 0) {
        <div class="empty-state dp-empty">
          <span class="empty-mark" aria-hidden="true"></span>
          <p class="empty-title">No documents in this folder yet</p>
          <p class="empty">
            Add a Markdown or text file to give the brain more to reason over.
          </p>
        </div>
      } @else {
        <ul class="dp-list" role="list">
          @for (doc of documents(); track doc.id) {
            <li class="dp-item">
              <span class="dp-doc-glyph" aria-hidden="true">
                <svg viewBox="0 0 16 16" width="15" height="15" fill="none">
                  <path
                    d="M4 1.75h5L12.25 5v9.25H4z"
                    stroke="currentColor"
                    stroke-width="1.3"
                    stroke-linejoin="round"
                  />
                  <path
                    d="M9 1.75V5h3.25"
                    stroke="currentColor"
                    stroke-width="1.3"
                    stroke-linejoin="round"
                  />
                </svg>
              </span>
              <span class="dp-doc-text">
                <span class="dp-doc-name">{{ doc.name }}</span>
                <span class="dp-doc-date">{{ formatDate(doc.createdAt) }}</span>
              </span>
              <button
                type="button"
                class="btn btn-ghost dp-del"
                [attr.aria-label]="'Delete ' + doc.name"
                [disabled]="deletingId() === doc.id"
                (click)="remove(doc)"
              >
                <svg
                  viewBox="0 0 16 16"
                  width="14"
                  height="14"
                  fill="none"
                  aria-hidden="true"
                >
                  <path
                    d="M3 4.5h10M6.5 4.5V3.2c0-.4.3-.7.7-.7h1.6c.4 0 .7.3.7.7v1.3M5 4.5l.5 8.3h5L11 4.5"
                    stroke="currentColor"
                    stroke-width="1.3"
                    stroke-linecap="round"
                    stroke-linejoin="round"
                  />
                </svg>
              </button>
            </li>
          }
        </ul>
      }
    </section>
  `,
  styles: [
    `
      :host {
        display: block;
      }
      .dp {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .dp-head {
        display: flex;
        align-items: flex-start;
        justify-content: space-between;
        gap: var(--space-4);
      }
      .dp-head-text {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        min-width: 0;
      }
      .dp-title {
        margin: 0;
        font-size: 1.0625rem;
      }
      .dp-sub {
        margin: 0;
        max-width: 46ch;
        color: var(--text-secondary);
        font-size: 0.875rem;
      }
      .dp-add {
        flex: none;
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
      }

      .dp-folder {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .dp-folder-label {
        color: var(--text-muted);
        font-size: 0.75rem;
        font-weight: 600;
        letter-spacing: 0.04em;
        text-transform: uppercase;
      }
      .dp-folder-select {
        width: 100%;
        height: 38px;
      }

      .dp-locked {
        align-items: center;
      }
      .dp-locked-glyph {
        flex: none;
        width: 8px;
        height: 8px;
        border-radius: var(--radius-pill);
        background: var(--accent-hover);
        box-shadow: 0 0 0 4px var(--accent-soft);
      }

      .dp-state {
        margin: 0;
        padding: var(--space-4) 0;
      }
      .dp-empty {
        padding: var(--space-6) var(--space-4);
      }
      .dp-empty .empty-title {
        margin: 0 0 var(--space-1);
      }
      .dp-empty .empty {
        margin: 0;
      }

      .dp-list {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .dp-item {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        padding: var(--space-2) var(--space-3);
        border: 1px solid var(--glass-border);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        transition: border-color var(--transition);
      }
      .dp-item:hover {
        border-color: var(--border-strong);
      }
      .dp-doc-glyph {
        flex: none;
        display: inline-flex;
        color: var(--accent-hover);
      }
      .dp-doc-text {
        display: flex;
        flex-direction: column;
        gap: 1px;
        min-width: 0;
        flex: 1 1 auto;
      }
      .dp-doc-name {
        color: var(--text-primary);
        font-size: 0.9375rem;
        font-weight: 550;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .dp-doc-date {
        color: var(--text-muted);
        font-size: 0.75rem;
      }
      .dp-del {
        flex: none;
        width: 34px;
        height: 34px;
        padding: 0;
        color: var(--text-muted);
      }
      .dp-del:hover {
        color: var(--danger);
      }
    `,
  ],
})
export class DocumentPanelComponent {
  private readonly ipc = inject(IpcService);
  private readonly folders = inject(FoldersService);
  private readonly toast = inject(ToastService);

  /** The user-chosen folder, or null until the first folder option resolves. */
  readonly selectedFolderId = signal<string | null>(null);

  readonly documents = signal<DocumentInfo[]>([]);
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);
  readonly importing = signal(false);
  readonly deletingId = signal<string | null>(null);

  /** Flattened folder tree → selector options (indented, lock-flagged). */
  protected readonly folderOptions = computed<FolderOption[]>(() => {
    const out: FolderOption[] = [];
    const walk = (nodes: FolderNode[], depth: number): void => {
      for (const node of nodes) {
        const sealed = node.locked && !node.unlocked;
        const indent = depth > 0 ? "  ".repeat(depth) + "↳ " : "";
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

  /** Whether the currently selected folder is sealed-and-not-unlocked. */
  protected readonly selectedBlocked = computed(() => {
    const id = this.selectedFolderId();
    return this.folderOptions().some((o) => o.id === id && o.blocked);
  });

  constructor() {
    // Ensure the folder tree is loaded so the selector has options.
    void this.folders.load();

    // Default the selection to the first folder once options resolve, and keep
    // it valid if the tree changes (a deleted/renamed folder shouldn't strand
    // the selection). Tracked effect that writes the selection signal.
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

    // (Re)load the document list whenever the selected folder OR the folder
    // lock-state changes — a session unlock/relock changes what
    // `list_documents` returns (a sealed folder is masked to empty). Reading
    // both signals registers the dependency; the fetch sets loading/error
    // synchronously before its first await, so writes must be allowed (NG0600).
    effect(
      () => {
        const id = this.selectedFolderId();
        // Establish a dependency on the lock-state too (drops masked docs live).
        this.folders.tree();
        if (!id) {
          this.documents.set([]);
          this.loading.set(false);
          return;
        }
        this.loading.set(true);
        this.error.set(null);
        void this.fetchDocuments(id);
      },
      { allowSignalWrites: true },
    );
  }

  private async fetchDocuments(folderId: string): Promise<void> {
    try {
      const docs = await this.ipc.listDocuments(folderId);
      // Stale-result guard: drop a response for a folder we've since left.
      if (this.selectedFolderId() !== folderId) {
        return;
      }
      this.documents.set(docs);
    } catch (e) {
      if (this.selectedFolderId() !== folderId) {
        return;
      }
      this.documents.set([]);
      this.error.set(String(e));
    } finally {
      if (this.selectedFolderId() === folderId) {
        this.loading.set(false);
      }
    }
  }

  protected onFolderChange(event: Event): void {
    this.selectedFolderId.set((event.target as HTMLSelectElement).value);
  }

  /** Open the native file dialog (md/txt) → import the chosen path. */
  async pickAndImport(): Promise<void> {
    const folderId = this.selectedFolderId();
    if (!folderId || this.selectedBlocked() || this.importing()) {
      return;
    }
    const chosen = await open({
      multiple: false,
      filters: [{ name: "Documents", extensions: ["md", "txt"] }],
    });
    // `open` returns string | string[] | null; we requested a single file.
    if (typeof chosen !== "string") {
      return;
    }

    this.importing.set(true);
    try {
      await this.ipc.importDocument(chosen, folderId);
      this.toast.success("Document added to the brain.");
      // Re-fetch the list (only if we're still on the same folder).
      if (this.selectedFolderId() === folderId) {
        await this.fetchDocuments(folderId);
      }
    } catch (e) {
      this.toast.danger(this.friendlyImportError(e));
    } finally {
      this.importing.set(false);
    }
  }

  /** Permanently delete a document, then refresh the list + toast. */
  async remove(doc: DocumentInfo): Promise<void> {
    const folderId = this.selectedFolderId();
    if (this.deletingId()) {
      return;
    }
    this.deletingId.set(doc.id);
    try {
      await this.ipc.deleteDocument(doc.id);
      this.toast.info(`Removed “${doc.name}”.`);
      if (folderId && this.selectedFolderId() === folderId) {
        await this.fetchDocuments(folderId);
      }
    } catch (e) {
      this.toast.danger(this.friendlyDeleteError(e));
    } finally {
      this.deletingId.set(null);
    }
  }

  /** Epoch-millis → a short local date string. */
  protected formatDate(epochMs: number): string {
    return new Date(epochMs).toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    });
  }

  /** Map a backend import error to a clear user message (Locked is the common one). */
  private friendlyImportError(e: unknown): string {
    const msg = String(e);
    if (/lock/i.test(msg)) {
      return "That folder is locked — unlock it first to add a document.";
    }
    if (/\.md and \.txt|only .*md|invalid/i.test(msg)) {
      return "Only Markdown (.md) and text (.txt) files can be imported.";
    }
    return "Couldn’t add that document. Please try again.";
  }

  private friendlyDeleteError(e: unknown): string {
    const msg = String(e);
    if (/lock/i.test(msg)) {
      return "That folder is locked — unlock it first to delete a document.";
    }
    return "Couldn’t remove that document. Please try again.";
  }
}
