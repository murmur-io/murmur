import {
  ChangeDetectionStrategy,
  Component,
  effect,
  inject,
  input,
  output,
  signal,
} from "@angular/core";
import { IpcService } from "../../../core/ipc.service";
import type { ContextHit } from "../../../core/models";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";

/**
 * LIVE-CONTEXT affordance (docs/research/2026-07-06-note-and-brain-architecture.md §3 / §6 / §8-Phase 3).
 *
 * A compact "Fetch live context" control mounted right after the note summary (note-panel). It is the
 * explicit connector-enrichment EGRESS moment: "Fetch live context" sends a redacted query out to the
 * consented Jira/Slack/web connectors via the fail-closed registry gate; the returned `ContextHit[]`
 * render as a review preview; "Add to note" persists the reviewed hits and "Clear" strips the block
 * (byte-exact undo via an empty-hits apply). The egress is kept LOUD — a "sends a query out" pill +
 * a one-line hint sit beside the button — consistent with the app's other cloud-egress affordances,
 * without the old full-width warning block.
 *
 * (Lane A — the local, zero-egress "Refresh links" backfill — was removed: links auto-refresh on
 * finalize, and the `Related notes` LIST lives in `app-related-meetings`.)
 *
 * Every write / clear goes through the ALREADY-GATED backend commands (`meeting_is_unlocked` before
 * any egress or write); the panel adds NO new ungated read path — the parent only mounts it for a
 * NON-locked meeting. IPC results land in signals; the parent reloads the note via `noteChanged`.
 */
@Component({
  selector: "app-stage2-panel",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./stage2-panel.component.html",
  styleUrl: "./stage2-panel.component.scss",
})
export class Stage2PanelComponent {
  private readonly ipc = inject(IpcService);
  private readonly errorCopy = inject(ErrorCopyService);

  readonly meetingId = input.required<string>();
  /** Fires after Lane A links / Lane B hits are applied so the parent reloads the note. */
  readonly noteChanged = output<void>();

  // ── Connector enrichment (EGRESS, on-demand) ────────────────────────────
  /** True while any Lane-B IPC (fetch / add / clear) is in flight. */
  readonly running = signal(false);
  /** The reviewed live-context hits — null before a fetch, `[]` when the fetch found nothing. */
  readonly hits = signal<ContextHit[] | null>(null);
  /** Latched after a successful "Add to note" write so the panel can confirm. */
  readonly applied = signal(false);
  /** Latched after a successful "Clear" (strip) so the panel can confirm. */
  readonly cleared = signal(false);
  /** Inline error for the Lane-B path (surfaced in a warning banner). */
  readonly error = signal<string | null>(null);

  /**
   * RESET on meeting change (the load-bearing correctness guard). The parent reuses this SAME
   * component instance across in-place meeting navigation (`detail()` is never nulled, so the
   * `@if (!locked())` mount never tears down) — so without this, a previous meeting's fetched
   * `hits()` (and the applied/cleared/linked confirmations) would survive onto the NEXT meeting and
   * "Add to note" would write meeting A's live-context into meeting B's note (a wrong-note write).
   * Tracking `meetingId()` re-runs this whenever the input id changes, wiping all Lane-A/B preview
   * state. Paired with a per-op stale-id guard in `fetchContext`/`addToNote`/`clearContext` below
   * (belt-and-braces: the reset clears the UI; the guard blocks a write racing a mid-flight nav).
   */
  private readonly _resetOnMeetingChange = effect(() => {
    this.meetingId(); // track
    this.hits.set(null);
    this.applied.set(false);
    this.cleared.set(false);
    this.error.set(null);
  });

  /**
   * The EGRESS moment: fetch live context from the consented connectors.
   * Lands the returned hits in `hits()` for review; the write is a SEPARATE step
   * (`addToNote`). No-op while a Lane-B call is already running.
   */
  async fetchContext(): Promise<void> {
    if (this.running()) {
      return;
    }
    this.running.set(true);
    this.error.set(null);
    this.applied.set(false);
    this.cleared.set(false);
    try {
      this.hits.set(await this.ipc.enrichNoteContext(this.meetingId()));
    } catch (e) {
      this.error.set(this.errorCopy.humanize(e));
    } finally {
      this.running.set(false);
    }
  }

  /**
   * LANE B — persist the reviewed hits as the `> [!context]-` callout. NO egress
   * (the hits were already fetched); then reload the note. No-op with no hits.
   */
  async addToNote(): Promise<void> {
    const h = this.hits();
    if (!h || h.length === 0 || this.running()) {
      return;
    }
    // Capture the target meeting BEFORE the write. If the user navigates mid-flight, the reset
    // effect wipes `hits()`, but this guard is the hard stop that prevents writing THIS meeting's
    // reviewed hits into a DIFFERENT meeting's note (wrong-note write).
    const id = this.meetingId();
    this.running.set(true);
    this.error.set(null);
    try {
      await this.ipc.applyNoteEnrichment(id, h);
      if (id !== this.meetingId()) {
        return; // navigated away mid-write — do not confirm/reload against the new meeting.
      }
      this.applied.set(true);
      this.noteChanged.emit();
    } catch (e) {
      this.error.set(this.errorCopy.humanize(e));
    } finally {
      this.running.set(false);
    }
  }

  /**
   * LANE B — byte-exact undo: apply an EMPTY hit set to strip the context block,
   * then reload the note. NO egress. No-op while a Lane-B call is running.
   */
  async clearContext(): Promise<void> {
    if (this.running()) {
      return;
    }
    const id = this.meetingId();
    this.running.set(true);
    this.error.set(null);
    try {
      await this.ipc.applyNoteEnrichment(id, []);
      if (id !== this.meetingId()) {
        return; // navigated away mid-clear — do not touch the new meeting's state.
      }
      this.hits.set(null);
      this.cleared.set(true);
      this.applied.set(false);
      this.noteChanged.emit();
    } catch (e) {
      this.error.set(this.errorCopy.humanize(e));
    } finally {
      this.running.set(false);
    }
  }
}
