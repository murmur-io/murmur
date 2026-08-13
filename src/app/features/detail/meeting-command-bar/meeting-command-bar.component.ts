import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  OnInit,
  inject,
  input,
  output,
  signal,
} from "@angular/core";
import type { NoteTemplate } from "../../../core/models";
import { IpcService } from "../../../core/ipc.service";
import { MurProviderIconComponent } from "../../../design-system/provider-icon/provider-icon.component";
import { ReminderComposerService } from "../../reminders/reminder-composer/reminder-composer.service";
import { MoveToMenuComponent } from "../../folders/move-to-menu/move-to-menu.component";

interface BuiltinTemplate {
  readonly id: string;
  readonly name: string;
  readonly description: string;
}

const BUILTIN_TEMPLATES: readonly BuiltinTemplate[] = [
  { id: "standard", name: "Standard", description: "Balanced summary" },
  { id: "brief", name: "Brief", description: "TL;DR and actions" },
  { id: "detailed", name: "Detailed", description: "Full context" },
  { id: "action", name: "Action-focused", description: "Owners and due dates" },
];

/**
 * The meeting page's single command surface. It lives below tags and above
 * Note/Audio/Share so actions never jump when the active tab changes.
 */
@Component({
  selector: "app-meeting-command-bar",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurProviderIconComponent, MoveToMenuComponent],
  templateUrl: "./meeting-command-bar.component.html",
  styleUrl: "./meeting-command-bar.component.scss",
  host: {
    "(document:click)": "onDocumentClick($event)",
    "(document:keydown.escape)": "closeOverlays()",
  },
})
export class MeetingCommandBarComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly reminders = inject(ReminderComposerService);
  private readonly host = inject(ElementRef<HTMLElement>);
  private readonly destroyRef = inject(DestroyRef);
  private destroyed = false;

  readonly meetingId = input.required<string>();
  readonly folderId = input<string | null>(null);
  readonly notePresent = input(false);
  readonly provenance = input<{ model: string; provider: string } | null>(null);
  readonly busy = input(false);
  readonly converting = input(false);
  readonly renaming = input(false);
  readonly editing = input(false);
  readonly exporting = input(false);
  readonly exportingCanvas = input(false);
  readonly linking = input(false);
  readonly keepsMasters = input(false);
  readonly hasAudio = input(false);
  readonly moveOpen = input(false);
  readonly exportMsg = input("");

  readonly convert = output<string | null>();
  readonly resummarize = output<void>();
  readonly rename = output<void>();
  readonly move = output<void>();
  readonly moved = output<string | null>();
  readonly closeMove = output<void>();
  readonly delete = output<void>();
  readonly copyMd = output<void>();
  readonly saveMd = output<void>();
  readonly savePdf = output<void>();
  readonly exportCanvas = output<void>();
  readonly saveAudio = output<void>();
  readonly exportMaster = output<"mic" | "sys">();
  readonly linkGraph = output<void>();
  readonly edit = output<void>();

  readonly templateOpen = signal(false);
  readonly menuOpen = signal(false);
  readonly reminderOpening = signal(false);
  readonly templates = signal<NoteTemplate[]>([]);
  readonly templatesLoading = signal(false);
  readonly builtins = BUILTIN_TEMPLATES;
  readonly reminderListenerState = this.reminders.listenerState;

  ngOnInit(): void {
    this.destroyRef.onDestroy(() => {
      this.destroyed = true;
    });
    void this.loadTemplates();
  }

  async newReminder(): Promise<void> {
    if (this.reminderListenerState() !== "ready" || this.reminderOpening()) {
      return;
    }
    this.closeOverlays();
    this.reminderOpening.set(true);
    const meetingId = this.meetingId();
    const privacyEpoch = this.reminders.privacyEpoch();
    let title = "";
    try {
      // Re-read through the canonical gated IPC instead of trusting the page's
      // potentially stale title. A lock during the await either masks this
      // response or advances privacyEpoch through the global invalidation.
      const detail = await this.ipc.getMeetingDetail(meetingId);
      if (
        !this.destroyed &&
        this.meetingId() === meetingId &&
        detail &&
        !detail.locked &&
        this.reminders.privacyEpoch() === privacyEpoch
      ) {
        title = detail.meeting.title ?? "";
      }
    } catch {
      // The opaque source id is still safe and submission re-gates it.
    } finally {
      if (!this.destroyed) {
        this.reminderOpening.set(false);
      }
    }
    if (
      !this.destroyed &&
      this.meetingId() === meetingId &&
      this.reminderListenerState() === "ready"
    ) {
      this.reminders.openCreate({
        source: { kind: "meeting", id: meetingId, title },
      });
    }
  }

  convertDefault(): void {
    if (!this.converting()) {
      this.convert.emit(null);
    }
  }

  toggleTemplates(): void {
    this.menuOpen.set(false);
    this.templateOpen.update((value) => !value);
  }

  chooseTemplate(id: string | null): void {
    this.templateOpen.set(false);
    if (!this.converting()) {
      this.convert.emit(id);
    }
  }

  toggleMenu(): void {
    this.templateOpen.set(false);
    this.menuOpen.update((value) => !value);
  }

  private closeMenu(): void {
    this.menuOpen.set(false);
  }

  pickRename(): void { this.closeMenu(); this.rename.emit(); }
  pickCopyMarkdown(): void { this.closeMenu(); this.copyMd.emit(); }
  pickSaveMarkdown(): void { this.closeMenu(); this.saveMd.emit(); }
  pickSavePdf(): void { this.closeMenu(); this.savePdf.emit(); }
  pickExportCanvas(): void { this.closeMenu(); this.exportCanvas.emit(); }
  pickSaveAudio(): void { this.closeMenu(); this.saveAudio.emit(); }
  pickLinkGraph(): void { this.closeMenu(); this.linkGraph.emit(); }
  pickDelete(): void { this.closeMenu(); this.delete.emit(); }
  pickExportMaster(kind: "mic" | "sys"): void {
    this.closeMenu();
    this.exportMaster.emit(kind);
  }

  pickMove(): void {
    this.menuOpen.set(false);
    this.move.emit();
  }

  closeOverlays(): void {
    this.templateOpen.set(false);
    this.menuOpen.set(false);
    if (this.moveOpen()) {
      this.closeMove.emit();
    }
  }

  onDocumentClick(event: MouseEvent): void {
    const target = event.target;
    if (target instanceof Node && this.host.nativeElement.contains(target)) {
      return;
    }
    this.closeOverlays();
  }

  private async loadTemplates(): Promise<void> {
    this.templatesLoading.set(true);
    try {
      this.templates.set(await this.ipc.listNoteTemplates());
    } catch {
      this.templates.set([]);
    } finally {
      this.templatesLoading.set(false);
    }
  }
}
