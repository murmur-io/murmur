import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  effect,
  inject,
  input,
  output,
  signal,
  viewChild,
} from "@angular/core";
import { IpcService } from "../../../core/ipc.service";
import type { DocumentPreviewTarget } from "../../../core/models";
import { MurSpinnerComponent } from "../../../design-system/spinner/spinner.component";

/**
 * A read-only CONTENT PREVIEW for one brain document/note — presented as an
 * OPAQUE modal (T3): a full-viewport dim scrim + an opaque
 * `var(--surface-overlay)` panel, NEVER the frosted `.card` (which would bleed
 * the Brain page through). The body is the CLEAN display text from
 * `IpcService.getDocument(id)`.
 *
 * The read is GATED server-side: a sealed-and-NOT-session-unlocked folder
 * returns "" (masked — never the stored text). We render that empty-string case
 * as an explicit "🔒 Locked" state — NOT a blank/broken panel — so a sealed
 * document is visibly locked and its content never appears (leak-relevant path).
 * `get_document` also returns "" for an unknown id or a document whose display
 * text is genuinely empty; those are INDISTINGUISHABLE from the sealed mask at
 * the FE, so we FAIL SAFE and treat every "" as LOCKED — a sealed doc is never
 * mislabeled "empty", and no content is ever shown behind the lock. (A
 * genuinely-empty document is a near-impossible edge: import rejects no-text
 * files with "No readable text found".)
 *
 * Pure/presentational: it owns the fetch + view state (`content` / `loading` /
 * `error` signals, fed by a stale-guarded effect keyed on the doc id); the
 * parent owns which doc is open (the `doc` input) and closes on {@link dismiss}
 * (scrim click / × button / Escape).
 */
@Component({
  selector: "app-document-preview",
  changeDetection: ChangeDetectionStrategy.OnPush,
  // Esc lives at DOCUMENT level on purpose: after clicking non-focusable text
  // (the preview body) focus falls to <body>, so a panel-scoped
  // (keydown.escape) would go dead. Mirrors LibraryComponent / MoveToMenu.
  host: {
    "(document:keydown.escape)": "onEscape()",
  },
  imports: [MurSpinnerComponent],
  templateUrl: "./document-preview.component.html",
  styleUrl: "./document-preview.component.scss",
})
export class DocumentPreviewComponent {
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);

  /**
   * The document/note to preview; null = the modal is closed (renders nothing).
   * A {@link DocumentPreviewTarget} — the minimal `{ id, name, kind }` the
   * component reads — so a document reachable only as a link target (a graph
   * node / `[[wikilink]]` / Related chip, with no full `DocumentInfo` in hand)
   * can be previewed too. `DocumentInfo` is structurally assignable, so the
   * Brain page's existing `DocumentInfo`-typed call site is unaffected.
   */
  readonly doc = input<DocumentPreviewTarget | null>(null);

  // Named `dismiss` (not `close`): `close` is a native DOM event name, which
  // `@angular-eslint/no-output-native` forbids as an output. Matches the
  // BrainNoteEditorComponent modal's `dismiss` convention.
  readonly dismiss = output<void>();

  /** The fetched clean text ("" when sealed-masked or genuinely empty). */
  protected readonly content = signal<string>("");
  /** True while `getDocument` is in flight. */
  protected readonly loading = signal(false);
  /** A read failure (e.g. transient IPC error) — distinct from locked/empty. */
  protected readonly error = signal<string | null>(null);

  private readonly closeBtn =
    viewChild<ElementRef<HTMLButtonElement>>("closeBtn");

  /**
   * The type BADGE for the header, derived from the file extension in the doc
   * name — a `kind: "note"` doc is always "Note" regardless of its name.
   */
  protected readonly badge = computed<string>(() => {
    const d = this.doc();
    if (!d) {
      return "";
    }
    if (d.kind === "note") {
      return "Note";
    }
    const ext = this.extensionOf(d.name);
    switch (ext) {
      case "pdf":
        return "PDF";
      case "docx":
        return "DOCX";
      case "pptx":
        return "PPTX";
      case "xlsx":
        return "XLSX";
      case "html":
      case "htm":
        return "HTML";
      case "md":
        return "MD";
      case "txt":
        return "TXT";
      case "png":
      case "jpg":
      case "jpeg":
      case "heic":
      case "tiff":
      case "tif":
      case "bmp":
      case "gif":
        return "Image";
      default:
        return "Doc";
    }
  });

  /**
   * Sealed-masked state: the fetch resolved successfully but returned "" — the
   * folder is sealed-and-not-unlocked (the backend masks the text). We show a
   * "🔒 Locked" panel, never the (absent) content. Distinct from a genuinely
   * empty visible document only in that we can't tell them apart from "" alone,
   * so we treat "" as LOCKED (fail-safe: never imply a sealed doc is "empty").
   * A load error is a THIRD, separate state.
   */
  protected readonly masked = computed(
    () => !this.loading() && !this.error() && this.content().length === 0,
  );

  constructor() {
    // Focus the close button when the modal OPENS. This host is now MOUNTED ONCE
    // in the app shell (globally reachable) with `doc` toggled null↔target — so
    // the component is NOT recreated per open (it used to live behind a parent
    // `@if`). An effect keyed on `doc()` therefore fires on every open/target
    // change; `afterNextRender` (never setTimeout/rAF) focuses the close button
    // once the modal has painted. On close (`doc` → null) the `@if` has removed
    // the button, so `closeBtn()` is undefined and the focus no-ops.
    effect(() => {
      if (!this.doc()) {
        return;
      }
      afterNextRender(() => this.closeBtn()?.nativeElement.focus(), {
        injector: this.injector,
      });
    });

    // Fetch the document text whenever a non-null doc is set. Stale-result
    // guard keyed on the doc id: an id can change mid-flight (the user opens a
    // different doc), so a late response for a doc we've since left is dropped.
    // Writes-in-effect are allowed since v19 (no allowSignalWrites flag); this
    // effect genuinely orchestrates an async IPC fetch, the sanctioned case.
    effect(() => {
      const d = this.doc();
      if (!d) {
        this.content.set("");
        this.loading.set(false);
        this.error.set(null);
        return;
      }
      const id = d.id;
      this.loading.set(true);
      this.error.set(null);
      this.content.set("");
      void this.fetch(id);
    });
  }

  /** Await the gated read; drop the response if the open doc changed since. */
  private async fetch(id: string): Promise<void> {
    try {
      const text = await this.ipc.getDocument(id);
      if (this.doc()?.id !== id) {
        return;
      }
      this.content.set(text);
    } catch (e) {
      if (this.doc()?.id !== id) {
        return;
      }
      this.error.set(String(e));
    } finally {
      if (this.doc()?.id === id) {
        this.loading.set(false);
      }
    }
  }

  protected onEscape(): void {
    if (this.doc()) {
      this.dismiss.emit();
    }
  }

  /** Lowercased final extension of a filename, or "" when there is none. */
  private extensionOf(name: string): string {
    const dot = name.lastIndexOf(".");
    if (dot < 0 || dot === name.length - 1) {
      return "";
    }
    return name.slice(dot + 1).toLowerCase();
  }
}
