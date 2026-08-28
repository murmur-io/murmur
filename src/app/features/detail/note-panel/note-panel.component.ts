import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  computed,
  inject,
  input,
  output,
  signal,
  viewChild,
} from "@angular/core";
import type {
  ClaimAlignment,
  GraphPayload,
  NoteAttachmentDto,
} from "../../../core/models";
import { MarkdownComponent } from "../../../shared/markdown/markdown.component";
import { AssistantSourcesComponent } from "../../../shared/assistant-sources/assistant-sources.component";
import { ConnectionsComponent } from "../../../shared/connections/connections.component";
import { MeetingActionsComponent } from "../meeting-actions/meeting-actions.component";
import { RelatedMeetingsComponent } from "../related-meetings/related-meetings.component";
import { Stage2PanelComponent } from "../stage2-panel/stage2-panel.component";
import { ToastService } from "../../../services/toast.service";
import {
  NoteAttachmentService,
  MAX_NOTE_ATTACHMENTS,
  insertMarkdownBlock,
  replacePendingAttachmentUri,
  type AttachmentPastePlan,
  type MarkdownEdit,
} from "../../../services/note-attachment.service";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";

/** One checklist entry parsed from a `- [ ]` / `- [x]` action-item line. */
export interface ActionItemLine {
  done: boolean;
  text: string;
}

/** A parsed `## Heading` section of the note body. */
export interface NoteSection {
  heading: string;
  kind: "actions" | "bullets" | "prose";
  paragraphs: string[];
  bullets: string[];
  actions: ActionItemLine[];
}

/** The whole note, decomposed into front-matter + body sections. */
export interface ParsedNote {
  tags: string[];
  participants: string[];
  sections: NoteSection[];
  raw: string | null;
  enhanced: boolean;
}

/**
 * One rendered "Receipt" (Brain v3 PR-5): a note claim that aligned to a
 * transcript segment, decorated for display. Carries the claim's own text
 * SNIPPET (from the raw markdown line at `claimIndex`) + the audio coordinate
 * and speaker/timestamp labels — clicking it seeks the shipped audio player.
 * `segId`/`startS`/`seq` drive the audio panel's flash + seek.
 */
export interface ReceiptChip {
  /** Short label of the claim line this receipt likely derives from. */
  claim: string;
  /** "Me" / "Others" / "" — from the segment speaker. */
  speaker: string;
  /** m:ss (h:mm:ss above 60 min) timestamp of the segment start. */
  time: string;
  /**
   * The full tooltip: likely-source phrasing + timestamp + speaker + the ASR
   * confidence TIER ("audio: clear" / "audio: unclear" — never the raw float).
   */
  title: string;
  /** The segment start in raw seconds (the player seek target). */
  startS: number;
  /** `Segment.idx` to flash in the transcript. */
  segId: number;
  /** The claim's line index — the stable UNIQUE key for `@for` track (two claims can share one segId). */
  claimIndex: number;
  /** Monotonic id so the parent can re-fire a repeat click on the same chip. */
  seq: number;
}

/**
 * ASR-confidence tier bounds for the receipt tooltip: at/above `CLEAR` the audio
 * was decoded confidently ("audio: clear"), below `UNCLEAR` it was acoustically
 * shaky ("audio: unclear" — the backend's `LOW_CONFIDENCE_P` operating point),
 * and the band between renders NOTHING (no over-claiming either way). The raw
 * float never reaches the UI.
 */
const RECEIPT_AUDIO_CLEAR_MIN = 0.8;
const RECEIPT_AUDIO_UNCLEAR_MAX = 0.55;

/** One grounding citation, split into vault vs web shapes for rendering. */
export interface ParsedCitation {
  kind: "vault" | "web";
  label: string;
  url: string | null;
}

/** A persisted assistant Q&A interaction enriched with parsed citations. */
export interface AssistantQa {
  id: string;
  command: string;
  answer: string;
  citations: ParsedCitation[];
  status: string;
  sourceLabel: string | null;
  createdAt: string;
}

/**
 * The NOTE tab (default): the note IS the product, so its body comes first —
 * primary Re-summarize verb + a `⋯ More` OPAQUE overflow menu (Manage / Export
 * & save / Hi-res masters / Graph, Delete isolated last), then the rendered
 * analysis, the persisted Q&A, action items, Ask-this-meeting chat and Related.
 * Presentational: the shell owns the meeting + all IPC handlers; this panel
 * renders inputs and emits outputs. Hosts the reused sub-components
 * (`meeting-actions` / `-chat` / `related-meetings`).
 *
 * Lives in its own file so its inline styles get their own per-component
 * `anyComponentStyle` budget — the reason the giant detail component is split.
 */
@Component({
  selector: "app-note-panel",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    MarkdownComponent,
    AssistantSourcesComponent,
    ConnectionsComponent,
    MeetingActionsComponent,
    RelatedMeetingsComponent,
    Stage2PanelComponent,
  ],
  templateUrl: "./note-panel.component.html",
  styleUrl: "./note-panel.component.scss",
})
export class NotePanelComponent {
  private readonly attachmentService = inject(NoteAttachmentService);
  private readonly toast = inject(ToastService);
  private readonly destroyRef = inject(DestroyRef);
  private readonly errorCopy = inject(ErrorCopyService);
  private destroyed = false;

  constructor() {
    this.destroyRef.onDestroy(() => {
      this.destroyed = true;
    });
  }

  // --- Identity / meeting inputs ------------------------------------------
  readonly meetingId = input.required<string>();
  /** The meeting's title — the anchor-chip label for the Ask-this-meeting picker. */
  readonly meetingTitle = input<string | null>(null);
  readonly folderId = input<string | null>(null);
  /** The parsed note body (null when there is no note / masked). */
  readonly note = input<ParsedNote | null>(null);
  /** The persisted in-meeting assistant Q&A. */
  readonly interactions = input<AssistantQa[]>([]);
  /**
   * Per-claim audio receipts (Brain v3 PR-5): each note line that aligned to a
   * transcript segment (backend `get_note_receipts`, `meeting_is_unlocked`-gated,
   * EMPTY for a locked meeting). `claimIndex` is an index into the note's raw
   * `markdown.split("\n")` lines — mapped to a display snippet via {@link noteRaw}.
   */
  readonly receipts = input<ClaimAlignment[]>([]);
  /**
   * The note's RAW markdown (unparsed), used only to snippet the claim line a
   * receipt's `claimIndex` points at. Null when there is no note / it is masked.
   */
  readonly noteRaw = input<string | null>(null);
  /** Gated image DTOs for this meeting note. */
  readonly attachments = input<readonly NoteAttachmentDto[]>([]);
  /** The vault export path from the note DTO (Saved-to-vault line). */
  readonly exportedPath = input<string | null>(null);
  /** Model provenance for the ghost badge (null → hidden). */
  readonly provenanceLabel = input<{ model: string; provider: string } | null>(null);

  // --- Busy / capability flags (mirror the shell) -------------------------
  readonly busy = input(false);
  readonly renaming = input(false);
  readonly editing = input(false);
  readonly saving = input(false);
  readonly exporting = input(false);
  readonly exportingCanvas = input(false);
  readonly linking = input(false);
  readonly keepsMasters = input(false);
  readonly hasAudio = input(false);
  readonly moveOpen = input(false);

  // --- Transient status text ----------------------------------------------
  readonly msg = input("");
  readonly exportMsg = input("");
  readonly exportError = input("");
  readonly canvasMsg = input("");
  readonly canvasError = input("");
  readonly graph = input<GraphPayload | null>(null);
  readonly graphError = input("");
  readonly justSaved = input(false);
  readonly pathCopied = input(false);

  // --- Editor / delete state ----------------------------------------------
  readonly draft = input("");
  readonly saveError = input("");
  readonly confirmingDelete = input(false);
  readonly deleting = input(false);
  readonly deleteError = input("");

  // --- Outputs back to the shell (which owns the IPC + writable state) -----
  readonly resummarize = output<void>();
  readonly rename = output<void>();
  readonly move = output<void>();
  readonly moved = output<string | null>();
  readonly closeMove = output<void>();
  readonly delete = output<void>();
  readonly cancelDelete = output<void>();
  readonly confirmDelete = output<void>();
  readonly copyMd = output<void>();
  readonly saveMd = output<void>();
  readonly savePdf = output<void>();
  readonly exportCanvas = output<void>();
  readonly saveAudio = output<void>();
  readonly exportMaster = output<"mic" | "sys">();
  readonly linkGraph = output<void>();
  /** Bubbles the live-context panel's apply/clear up so the parent reloads the note. */
  readonly noteChanged = output<void>();
  readonly edit = output<void>();
  readonly cancelEdit = output<void>();
  readonly saveNote = output<void>();
  readonly draftInput = output<string>();
  readonly attachmentAdded = output<NoteAttachmentDto>();
  readonly attachmentBusyChange = output<boolean>();
  readonly copyPath = output<void>();
  readonly openRelated = output<string>();
  /**
   * A receipt chip was clicked (Brain v3 PR-5): asks the shell to switch to the
   * Audio tab and seek/flash the proving segment. Carries only audio coordinates
   * (`startS`/`segId`) + a `seq` so a repeat click on the same chip re-fires.
   */
  readonly seekReceipt = output<{ startS: number; segId: number; seq: number }>();

  private readonly editorArea =
    viewChild<ElementRef<HTMLTextAreaElement>>("editorArea");
  private readonly imageFileInput =
    viewChild<ElementRef<HTMLInputElement>>("imageFileInput");
  readonly importingImages = signal(0);
  private imageInsertion: { start: number; end: number } | null = null;


  rememberImageInsertion(): void {
    const el = this.editorArea()?.nativeElement;
    if (el) {
      this.imageInsertion = { start: el.selectionStart, end: el.selectionEnd };
    }
  }

  openImagePicker(): void {
    if (this.saving() || this.importingImages() > 0) {
      return;
    }
    if (!this.imageInsertion) {
      this.rememberImageInsertion();
    }
    this.imageFileInput()?.nativeElement.click();
  }

  onImageFilesSelected(event: Event): void {
    const input = event.target as HTMLInputElement;
    const plan = this.attachmentService.planFromFiles(
      input.files ?? [],
      this.availableImageSlots(),
    );
    input.value = "";
    this.notifyAttachmentWarnings(plan);
    const el = this.editorArea()?.nativeElement;
    const selection = this.imageInsertion ?? {
      start: el?.selectionStart ?? this.draft().length,
      end: el?.selectionEnd ?? this.draft().length,
    };
    this.imageInsertion = null;
    this.startAttachmentImport(plan, selection.start, selection.end);
  }

  onEditorPaste(event: ClipboardEvent): void {
    if (!event.clipboardData) {
      return;
    }
    const plan = this.attachmentService.planFromTransfer(
      event.clipboardData,
      this.availableImageSlots(),
    );
    this.notifyAttachmentWarnings(plan);
    if (!plan.segments.some((segment) => segment.kind === "image")) {
      return;
    }
    event.preventDefault();
    const el = event.target as HTMLTextAreaElement;
    this.startAttachmentImport(plan, el.selectionStart, el.selectionEnd);
  }

  onEditorDragOver(event: DragEvent): void {
    if (event.dataTransfer?.types.includes("Files")) {
      event.preventDefault();
    }
  }

  onEditorDrop(event: DragEvent): void {
    if (!event.dataTransfer) {
      return;
    }
    const plan = this.attachmentService.planFromTransfer(
      event.dataTransfer,
      this.availableImageSlots(),
    );
    this.notifyAttachmentWarnings(plan);
    if (!plan.segments.some((segment) => segment.kind === "image")) {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    const el = event.target as HTMLTextAreaElement;
    this.startAttachmentImport(plan, el.selectionStart, el.selectionEnd);
  }

  private startAttachmentImport(
    plan: AttachmentPastePlan,
    selectionStart: number,
    selectionEnd: number,
  ): void {
    const pending = this.attachmentService.pendingPlan(plan);
    if (!pending.images.length || !pending.markdown) {
      return;
    }
    this.applyDraftEdit(
      insertMarkdownBlock(this.draft(), selectionStart, selectionEnd, pending.markdown),
      true,
    );
    this.importingImages.update((count) => count + pending.images.length);
    this.attachmentBusyChange.emit(true);
    void this.performAttachmentImport(this.meetingId(), pending.images);
  }

  private async performAttachmentImport(
    meetingId: string,
    pendingImages: ReturnType<NoteAttachmentService["pendingPlan"]>["images"],
  ): Promise<void> {
    try {
      // One decoder/canvas at a time bounds peak RGBA memory.
      for (const { id, image } of pendingImages) {
        try {
          const attachment = await this.attachmentService.importImage(
            "meeting",
            meetingId,
            image,
          );
          if (
            this.destroyed ||
            this.meetingId() !== meetingId ||
            !this.editing()
          ) {
            void this.attachmentService
              .deleteAttachment("meeting", meetingId, attachment.id)
              .catch(() => undefined);
            continue;
          }
          const replaced = this.replacePendingAttachment(
            id,
            this.attachmentService.attachmentMarkdown(attachment, image.alt),
          );
          if (replaced) {
            this.attachmentAdded.emit(attachment);
          } else {
            void this.attachmentService
              .deleteAttachment("meeting", meetingId, attachment.id)
              .catch(() => undefined);
          }
        } catch (error) {
          if (this.meetingId() === meetingId && this.editing()) {
            this.replacePendingAttachment(id, "");
            this.toast.danger(this.errorCopy.humanize(error));
          }
        }
      }
    } finally {
      this.importingImages.update((count) =>
        Math.max(0, count - pendingImages.length),
      );
      if (!this.destroyed && this.importingImages() === 0) {
        this.attachmentBusyChange.emit(false);
      }
    }
  }

  private replacePendingAttachment(pendingId: string, replacement: string): boolean {
    const el = this.editorArea()?.nativeElement;
    const edit = replacePendingAttachmentUri(
      this.draft(),
      pendingId,
      replacement,
      el?.selectionStart ?? this.draft().length,
      el?.selectionEnd ?? this.draft().length,
    );
    if (!edit) {
      return false;
    }
    this.applyDraftEdit(edit, false);
    return edit.canonicalSlot;
  }

  private availableImageSlots(): number {
    if (this.importingImages() > 0) {
      return 0;
    }
    return Math.max(0, MAX_NOTE_ATTACHMENTS - this.attachments().length);
  }

  private applyDraftEdit(edit: MarkdownEdit, focus: boolean): void {
    this.draftInput.emit(edit.value);
    const el = this.editorArea()?.nativeElement;
    if (el) {
      el.value = edit.value;
      el.setSelectionRange(edit.selectionStart, edit.selectionEnd);
      if (focus) {
        el.focus();
      }
    }
  }

  private notifyAttachmentWarnings(plan: AttachmentPastePlan): void {
    if (plan.skippedExternalImages) {
      this.toast.info("External images were skipped to protect your privacy.");
    }
    if (plan.skippedUnsupportedImages) {
      this.toast.danger("Some images were skipped. Use PNG, JPEG, or WebP files.");
    }
    if (plan.skippedTooManyImages) {
      this.toast.danger(`A note can contain up to ${MAX_NOTE_ATTACHMENTS} images.`);
    }
  }

  /** Monotonic click id so a repeat click on the SAME receipt re-fires downstream. */
  private receiptSeq = 0;

  /**
   * The receipts decorated for display: each aligned claim → a chip labelled with
   * a snippet of the claim line (from the raw markdown at `claimIndex`) + the
   * speaker + timestamp of the likely-source segment. A receipt whose `claimIndex`
   * is out of range (a stale note vs a just-recomputed alignment) OR points into
   * the YAML front-matter (metadata like `attendees:` is never a claim — the
   * backend skips it too; this is defense-in-depth against a stale/older backend)
   * is dropped so a chip never shows a wrong/blank/metadata claim. Pure
   * `computed` (no template method).
   */
  readonly receiptChips = computed<ReceiptChip[]>(() => {
    const raw = this.noteRaw();
    if (!raw) {
      return [];
    }
    const lines = raw.split("\n");
    const fmEnd = this.frontmatterEnd(lines);
    const out: ReceiptChip[] = [];
    for (const r of this.receipts()) {
      if (r.claimIndex < fmEnd) {
        continue; // front-matter line ⇒ never a chip
      }
      const line = lines[r.claimIndex];
      const claim = line ? this.claimSnippet(line) : "";
      if (!claim) {
        continue; // out-of-range / non-content line ⇒ no chip
      }
      const speaker = this.speakerLabel(r.speaker);
      const time = this.fmtTime(r.startS);
      out.push({
        claim,
        speaker,
        time,
        title: this.receiptTitle(time, speaker, r.confidence ?? null),
        startS: r.startS,
        segId: r.segmentId,
        claimIndex: r.claimIndex,
        seq: 0, // filled at click time (a fresh id per click, see onReceipt)
      });
    }
    return out;
  });

  /** Emit the seek/flash request for a clicked receipt (fresh `seq` each click). */
  onReceipt(chip: ReceiptChip): void {
    this.seekReceipt.emit({
      startS: chip.startS,
      segId: chip.segId,
      seq: ++this.receiptSeq,
    });
  }

  /**
   * The number of leading lines occupied by a YAML front-matter block (`---`
   * fence on line 0 through the closing `---`), or 0 when there is none. Mirrors
   * the backend's `frontmatter_end` semantics, including the conservative
   * unterminated case (an opened-but-never-closed block makes EVERY line
   * front-matter — never chip a line we cannot prove is body).
   */
  private frontmatterEnd(lines: string[]): number {
    if (lines[0] !== "---") {
      return 0;
    }
    const close = lines.indexOf("---", 1);
    return close === -1 ? lines.length : close + 1;
  }

  /**
   * The chip tooltip: likely-source phrasing (an overlap heuristic, never a
   * proof) + the ASR confidence rendered as a TIER — "audio: clear" at/above
   * {@link RECEIPT_AUDIO_CLEAR_MIN}, "audio: unclear" below
   * {@link RECEIPT_AUDIO_UNCLEAR_MAX}, nothing in the band between or when
   * whisper computed no confidence. The raw float is never rendered.
   */
  private receiptTitle(
    time: string,
    speaker: string,
    confidence: number | null,
  ): string {
    let title = `Likely source · ${time}`;
    if (speaker) {
      title += ` · ${speaker}`;
    }
    if (confidence !== null && confidence >= RECEIPT_AUDIO_CLEAR_MIN) {
      title += " · audio: clear";
    } else if (confidence !== null && confidence < RECEIPT_AUDIO_UNCLEAR_MAX) {
      title += " · audio: unclear";
    }
    return title;
  }

  /** A short, marker-stripped snippet of a claim line for the chip label. */
  private claimSnippet(line: string): string {
    // Drop list/checkbox/quote markers and collapse whitespace; cap the length so
    // the chip stays compact (the full claim stays in the note body above).
    const cleaned = line
      .replace(/^\s*(?:[-*+]\s+)?(?:\[[ xX]\]\s+)?/, "")
      .replace(/^\s*>\s?/, "")
      .replace(/[#*_`]/g, "")
      .trim();
    if (cleaned.length <= 64) {
      return cleaned;
    }
    return cleaned.slice(0, 63).trimEnd() + "…";
  }

  /** "me"/"others"/legacy → "Me"/"Others"/"" for the chip. */
  private speakerLabel(speaker: string | null | undefined): string {
    if (speaker === "me") {
      return "Me";
    }
    if (speaker === "others" || /^others-\d+$/.test(speaker ?? "")) {
      return "Others";
    }
    return "";
  }

  /** Seconds → m:ss, or h:mm:ss above 60 min (a 2h receipt is never "125:07"). */
  private fmtTime(s: number): string {
    const total = Math.max(0, Math.floor(s || 0));
    const h = Math.floor(total / 3600);
    const m = Math.floor((total % 3600) / 60);
    const sec = total % 60;
    if (h > 0) {
      return `${h}:${m.toString().padStart(2, "0")}:${sec
        .toString()
        .padStart(2, "0")}`;
    }
    return `${m}:${sec.toString().padStart(2, "0")}`;
  }

  /** Map an interaction status to a global `.pill` variant. */
  qaStatusPillClass(status: string): string {
    switch (status) {
      case "ok":
        return "is-success";
      case "needs_consent":
        return "is-warning";
      case "unavailable":
      case "unrecognized":
        return "is-accent";
      case "nothing_heard":
        return "";
      default:
        return "is-danger";
    }
  }

  /** Short human label for the status pill. */
  qaStatusLabel(status: string): string {
    switch (status) {
      case "ok":
        return "Odpowiedziano";
      case "needs_consent":
        return "Wymaga zgody";
      case "unavailable":
        return "Unavailable";
      case "unrecognized":
        return "Nierozpoznane";
      case "nothing_heard":
        return "Nothing was heard";
      case "error":
        return "Error";
      default:
        return status;
    }
  }

}
