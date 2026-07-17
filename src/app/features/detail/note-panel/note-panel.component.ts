import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  computed,
  input,
  output,
  signal,
  viewChild,
} from "@angular/core";
import type {
  BacklinkSource,
  ClaimAlignment,
  GraphPayload,
} from "../../../core/models";
import { MarkdownComponent } from "../../../shared/markdown/markdown.component";
import { AssistantSourcesComponent } from "../../../shared/assistant-sources/assistant-sources.component";
import { BacklinksComponent } from "../../../shared/backlinks/backlinks.component";
import { ConnectionsComponent } from "../../../shared/connections/connections.component";
import { MoveToMenuComponent } from "../../folders/move-to-menu/move-to-menu.component";
import { MeetingActionsComponent } from "../meeting-actions/meeting-actions.component";
import { MeetingChatComponent } from "../meeting-chat/meeting-chat.component";
import { MeetingRecipesComponent } from "../meeting-recipes/meeting-recipes.component";
import { RelatedMeetingsComponent } from "../related-meetings/related-meetings.component";
import { Stage2PanelComponent } from "../stage2-panel/stage2-panel.component";

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
  /** Short label of the claim line this receipt proves. */
  claim: string;
  /** "Me" / "Others" / "" — from the segment speaker. */
  speaker: string;
  /** m:ss timestamp of the segment start. */
  time: string;
  /** The segment start in raw seconds (the player seek target). */
  startS: number;
  /** `Segment.idx` to flash in the transcript. */
  segId: number;
  /** The claim's line index — the stable UNIQUE key for `@for` track (two claims can share one segId). */
  claimIndex: number;
  /** Monotonic id so the parent can re-fire a repeat click on the same chip. */
  seq: number;
}

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
 * analysis, the persisted Q&A, action items, Recipes, Ask-this-meeting chat and
 * Related. Presentational: the shell owns the meeting + all IPC handlers; this
 * panel renders inputs and emits outputs. Hosts the reused sub-components
 * (`meeting-actions` / `-recipes` / `-chat` / `related-meetings`).
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
    BacklinksComponent,
    ConnectionsComponent,
    MoveToMenuComponent,
    MeetingActionsComponent,
    MeetingChatComponent,
    MeetingRecipesComponent,
    RelatedMeetingsComponent,
    Stage2PanelComponent,
  ],
  templateUrl: "./note-panel.component.html",
  styleUrl: "./note-panel.component.scss",
})
export class NotePanelComponent {
  // --- Identity / meeting inputs ------------------------------------------
  readonly meetingId = input.required<string>();
  readonly folderId = input<string | null>(null);
  /** The parsed note body (null when there is no note / masked). */
  readonly note = input<ParsedNote | null>(null);
  /** The persisted in-meeting assistant Q&A. */
  readonly interactions = input<AssistantQa[]>([]);
  /**
   * Note↔note backlinks ("Linked mentions") — the VISIBLE inbound sources
   * (meetings + notes) that link this meeting. Empty when the meeting is
   * locked (the shell skips the gated fetch there). Rendered below the note body.
   */
  readonly backlinks = input<BacklinkSource[]>([]);
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
  readonly copyPath = output<void>();
  readonly openRelated = output<string>();
  /**
   * A receipt chip was clicked (Brain v3 PR-5): asks the shell to switch to the
   * Audio tab and seek/flash the proving segment. Carries only audio coordinates
   * (`startS`/`segId`) + a `seq` so a repeat click on the same chip re-fires.
   */
  readonly seekReceipt = output<{ startS: number; segId: number; seq: number }>();

  // --- ⋯ More overlay menu (local presentational open/close) --------------
  private readonly moreAnchor =
    viewChild<ElementRef<HTMLElement>>("moreAnchor");
  readonly menuOpen = signal(false);

  toggleMenu(): void {
    this.menuOpen.update((v) => !v);
  }

  /** Close the menu after an item fires (the action itself is an output). */
  pick(): void {
    this.menuOpen.set(false);
  }

  /** Whether the menu should offset the ⋯ trigger when no badge precedes it. */
  readonly hasTrailingBadge = computed(
    () => this.provenanceLabel() !== null,
  );

  /** Monotonic click id so a repeat click on the SAME receipt re-fires downstream. */
  private receiptSeq = 0;

  /**
   * The receipts decorated for display: each aligned claim → a chip labelled with
   * a snippet of the claim line (from the raw markdown at `claimIndex`) + the
   * speaker + m:ss timestamp of the proving segment. A receipt whose `claimIndex`
   * is out of range (a stale note vs a just-recomputed alignment) is dropped so a
   * chip never shows a wrong/blank claim. Pure `computed` (no template method).
   */
  readonly receiptChips = computed<ReceiptChip[]>(() => {
    const raw = this.noteRaw();
    if (!raw) {
      return [];
    }
    const lines = raw.split("\n");
    const out: ReceiptChip[] = [];
    for (const r of this.receipts()) {
      const line = lines[r.claimIndex];
      const claim = line ? this.claimSnippet(line) : "";
      if (!claim) {
        continue; // out-of-range / non-content line ⇒ no chip
      }
      out.push({
        claim,
        speaker: this.speakerLabel(r.speaker),
        time: this.fmtTime(r.startS),
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

  /** Seconds → m:ss for the receipt timestamp. */
  private fmtTime(s: number): string {
    const total = Math.max(0, Math.floor(s || 0));
    const m = Math.floor(total / 60);
    const sec = total % 60;
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
        return "Niedostępne";
      case "unrecognized":
        return "Nierozpoznane";
      case "nothing_heard":
        return "Nic nie usłyszano";
      case "error":
        return "Błąd";
      default:
        return status;
    }
  }

  /** Suppress unused-viewChild lint while keeping the anchor ref available. */
  protected anchorEl(): HTMLElement | null {
    return this.moreAnchor()?.nativeElement ?? null;
  }
}
