import {
  ChangeDetectionStrategy,
  Component,
  inject,
  input,
  output,
  signal,
} from "@angular/core";
import { IpcService } from "../../../core/ipc.service";
import type { VerifyFindingDto } from "../../../core/models";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";

/**
 * VERIFY WITH JIRA — on-demand deterministic check of the note's ticket claims against LIVE Jira
 * (docs/research/2026-07-05-connectors-live-vs-rag.md). On-demand ONLY (an explicit click = the
 * egress consent moment is visible); results render as a list; "Add markers to note" persists the
 * non-destructive > blockquote markers through the gated backend command.
 */
@Component({
  selector: "app-verify-panel",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./verify-panel.component.html",
  styleUrl: "./verify-panel.component.scss",
})
export class VerifyPanelComponent {
  private readonly ipc = inject(IpcService);
  private readonly errorCopy = inject(ErrorCopyService);

  readonly meetingId = input.required<string>();
  /** Fires after markers are applied so the parent reloads the note. */
  readonly noteChanged = output<void>();

  readonly running = signal(false);
  readonly applied = signal(false);
  readonly findings = signal<VerifyFindingDto[] | null>(null);
  readonly error = signal<string | null>(null);

  glyph(v: VerifyFindingDto["verdict"]): string {
    return v === "confirmed" ? "✓" : v === "conflict" ? "⧗" : "⚠";
  }

  async verify(): Promise<void> {
    if (this.running()) {
      return;
    }
    this.running.set(true);
    this.error.set(null);
    this.applied.set(false);
    try {
      this.findings.set(await this.ipc.verifyNoteSources(this.meetingId()));
    } catch (e) {
      this.error.set(this.errorCopy.humanize(e));
    } finally {
      this.running.set(false);
    }
  }

  async apply(): Promise<void> {
    const f = this.findings();
    if (!f || this.running()) {
      return;
    }
    this.running.set(true);
    try {
      await this.ipc.applyNoteVerifyMarkers(this.meetingId(), f);
      this.applied.set(true);
      this.noteChanged.emit();
    } catch (e) {
      this.error.set(this.errorCopy.humanize(e));
    } finally {
      this.running.set(false);
    }
  }
}
