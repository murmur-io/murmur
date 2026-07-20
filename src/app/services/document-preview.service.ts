import { Injectable, signal } from "@angular/core";
import type { DocumentPreviewTarget } from "../core/models";

/**
 * The app-wide handle to the ONE read-only document/note content-preview modal
 * ({@link import('../features/brain/document-preview/document-preview.component').DocumentPreviewComponent}).
 *
 * A brain-ingested document (`documents` row, `kind:"document"`, e.g. a PDF) has
 * no route of its own — `get_note` rejects a document id, so `["/notes", id]` is
 * a dead end. Instead every "open a document" surface (a Related/Suggested chip,
 * a `[[wikilink]]`, a full-brain-graph node) calls {@link open} and this service
 * feeds the target into the SINGLE `<app-document-preview>` host mounted in the
 * always-visible app shell — so a document is viewable from every route, from
 * one source of truth (no per-surface modal copy).
 *
 * A pure signal-holder (no IPC of its own): the modal owns the gated
 * `getDocument(id)` fetch + view state; this only tracks WHICH document is open.
 * The read is gated server-side — a sealed-and-not-session-unlocked folder masks
 * the text — so surfacing a target here can never reveal locked content.
 */
@Injectable({ providedIn: "root" })
export class DocumentPreviewService {
  /** The document/note currently open in the preview modal; null = closed. */
  private readonly _target = signal<DocumentPreviewTarget | null>(null);

  /** The open target (read-only); null when the modal is closed. */
  readonly target = this._target.asReadonly();

  /** Open the read-only preview for a document/note. */
  open(t: DocumentPreviewTarget): void {
    this._target.set(t);
  }

  /** Close the preview modal. */
  close(): void {
    this._target.set(null);
  }
}
