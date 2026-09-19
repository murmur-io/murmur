import {
  ChangeDetectionStrategy,
  Component,
  TemplateRef,
  ElementRef,
  viewChild,
  computed,
  input,
  output,
} from "@angular/core";
import { splitNoteDocument, joinNoteDocument } from "./front-matter";
import { NgTemplateOutlet } from "@angular/common";
import type { NoteAttachmentDto } from "../../core/models";
import { MarkdownComponent } from "../markdown/markdown.component";
import { ConnectionsComponent } from "../connections/connections.component";

export type NoteViewMode =
  | { readonly access: "editable"; readonly view: "edit" | "preview" }
  | { readonly access: "view-only"; readonly view: "preview"; readonly reason: string };

export interface NoteDocumentRelated {
  readonly kind: "note" | "meeting" | "org";
  readonly id: string;
  readonly locked?: boolean;
  readonly expandedByDefault?: boolean;
  readonly prominent?: boolean;
  readonly inlineWikilinkTitles?: string[];
}

@Component({
  selector: "app-note-document",
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: {
    "[attr.data-mode]": "mode().view",
    "[attr.data-access]": "mode().access",
  },
  imports: [NgTemplateOutlet, MarkdownComponent, ConnectionsComponent],
  templateUrl: "./note-document.component.html",
  styleUrl: "./note-document.component.scss",
})
export class NoteDocumentComponent {
  readonly mode = input.required<NoteViewMode>();
  readonly title = input<string | null>(null);
  readonly markdown = input.required<string>();
  readonly printMarkdown = input<string | null>(null);
  readonly printBody = computed(() => splitNoteDocument(this.printMarkdown() ?? "").body);
  readonly attachments = input<readonly NoteAttachmentDto[]>([]);
  readonly showTitle = input(true);
  readonly origin = input<readonly string[]>([]);
  readonly related = input<NoteDocumentRelated | null>(null);
  readonly relatedUnavailable = input("");
  readonly toolbarTemplate = input<TemplateRef<unknown> | null>(null);
  readonly footerTemplate = input<TemplateRef<unknown> | null>(null);
  readonly emptyTemplate = input<TemplateRef<unknown> | null>(null);
  readonly editorBody = input<string | null>(null);
  readonly editingBody = computed(() => this.editorBody() ?? this.document().body);
  readonly editorLabel = input("Note content (markdown)");
  readonly editorPlaceholder = input("");
  readonly editorSpellcheck = input(true);
  readonly editorAutocapitalize = input("sentences");
  readonly editorArea = viewChild<ElementRef<HTMLTextAreaElement>>("editorArea");
  readonly document = computed(() => splitNoteDocument(this.markdown()));
  readonly editorInput = output<Event>();
  readonly editorKeydown = output<KeyboardEvent>();
  readonly editorPaste = output<ClipboardEvent>();
  readonly editorDragover = output<DragEvent>();
  readonly editorDrop = output<DragEvent>();
  readonly editorSelect = output<void>();
  readonly editorBlur = output<void>();

  onInput(event: Event): void {
    const body = (event.target as HTMLTextAreaElement).value;
    this.markdownChange.emit(joinNoteDocument(this.document().frontMatterPrefix, body));
    this.editorInput.emit(event);
  }
  readonly measure = input<"page" | "embedded">("page");
  readonly wide = input(false);
  readonly busy = input(false);
  readonly error = input("");

  readonly titleChange = output<string>();
  readonly titleBlur = output<void>();
  readonly markdownChange = output<string>();
}
