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
import type { OrgSharePreview } from "../../../core/models";

/**
 * Which local source this sheet is publishing to the org brain — a recorded
 * MEETING or an authored NOTE. Exactly one of the two ids is set; the sheet
 * routes the preview + confirm to the matching command.
 */
export interface OrgShareTarget {
  kind: "meeting" | "note";
  /** The meeting/document id. */
  id: string;
}

/**
 * The "Add to Org Brain" PREVIEW SHEET (Shared Brain v1). A FLOATING overlay over
 * the note/detail, so it is OPAQUE `var(--surface-overlay)` + `backdrop-filter:
 * none` + a strong border + `--shadow-lg` — NEVER the frosted `.card` (trap T3),
 * which would bleed the content behind it through.
 *
 * SELF-CONTAINED: it injects its own {@link IpcService} and owns the whole preview
 * sub-state. Given the `target` (a meeting or note id) + the org name/member count,
 * it fetches `previewOrgShare` and renders EXACTLY the outgoing markdown (scrollable),
 * its byte size + chunk count, the PII scrub toggle (default ON) with the scrubbed
 * counts, and a "who can see this" line (org name + member count). Confirm →
 * `shareMeetingToOrg` / `shareDocumentToOrg`; on success it emits `shared` so the
 * host can toast + refresh the badge. Cancel/backdrop/Escape emits `cancelled`.
 *
 * The scrub toggle re-runs `previewOrgShare` (the markdown + counts change with the
 * scrub setting) — the effect is stale-guarded on the current `(target, scrub)` so a
 * late response can't overwrite a newer one.
 */
@Component({
  selector: "app-org-share-sheet",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./org-share-sheet.component.html",
  styleUrl: "./org-share-sheet.component.scss",
})
export class OrgShareSheetComponent {
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);

  /** The local source (meeting / note) being published. */
  readonly target = input.required<OrgShareTarget>();
  /** The org name — shown in the "who can see this" line. */
  readonly orgName = input.required<string>();
  /** The org member count — shown in the "who can see this" line. */
  readonly memberCount = input.required<number>();

  /** Emitted after a successful publish (host toasts + refreshes the badge). */
  readonly shared = output<void>();
  /** Emitted on cancel / backdrop / Escape — nothing left the device. */
  readonly cancelled = output<void>();

  private readonly panel = viewChild<ElementRef<HTMLDivElement>>("panel");

  /** Whether the regex PII scrub is on (default ON per the redaction policy). */
  readonly scrub = signal(true);

  /** The current preview (null while loading / on error). */
  private readonly _preview = signal<OrgSharePreview | null>(null);
  readonly preview = this._preview.asReadonly();

  /** True while a `previewOrgShare` fetch is in flight. */
  readonly loading = signal(false);
  /** A preview-load error to surface inline (e.g. `Locked`, no consent yet). */
  readonly previewError = signal<string | null>(null);

  /** True while the confirm `shareMeetingToOrg`/`shareDocumentToOrg` call is in flight. */
  readonly sharing = signal(false);
  /** A share-confirm error to surface inline. */
  readonly shareError = signal<string | null>(null);

  /** "who can see this" line: N members of the named org. */
  readonly audienceLabel = computed(
    () =>
      `${this.memberCount()} ${this.memberCount() === 1 ? "member" : "members"} of ${this.orgName()}`,
  );

  /** True when the scrub removed anything at the current setting (drives the counts row). */
  readonly scrubbedAny = computed(() => {
    const s = this._preview()?.scrubbed;
    return !!s && s.emails + s.phones + s.cards > 0;
  });

  constructor() {
    // Fetch (or re-fetch) the preview whenever the target or the scrub toggle
    // changes. Async IPC-on-signal-change effect (T1) — writes loading/error/
    // preview, stale-guarded on the captured (id, scrub) so a late response
    // from a stale scrub setting can't clobber a newer one.
    effect(
      () => {
        const t = this.target();
        const scrub = this.scrub();
        void this.loadPreview(t, scrub);
      },
      { injector: this.injector },
    );

    // Land focus in the sheet so Escape works + it's announced.
    afterNextRender(() => this.panel()?.nativeElement.focus(), {
      injector: this.injector,
    });
  }

  /** Fetch the outgoing-share preview for `(target, scrub)`, stale-guarded. */
  private async loadPreview(t: OrgShareTarget, scrub: boolean): Promise<void> {
    this.previewError.set(null);
    this.loading.set(true);
    try {
      const p = await this.ipc.previewOrgShare(
        t.kind === "meeting"
          ? { meetingId: t.id, scrub }
          : { documentId: t.id, scrub },
      );
      // Stale-guard: drop the response if the target/scrub moved on under us.
      if (this.target().id !== t.id || this.scrub() !== scrub) {
        return;
      }
      this._preview.set(p);
    } catch (e) {
      if (this.target().id !== t.id || this.scrub() !== scrub) {
        return;
      }
      this.previewError.set(this.friendlyError(String(e)));
      this._preview.set(null);
    } finally {
      if (this.target().id === t.id && this.scrub() === scrub) {
        this.loading.set(false);
      }
    }
  }

  /** Flip the scrub toggle (the effect re-fetches the preview). */
  onScrubChange(event: Event): void {
    this.scrub.set((event.target as HTMLInputElement).checked);
  }

  /** Confirm the publish. Routes to the meeting/note command; on success emits `shared`. */
  async confirm(): Promise<void> {
    if (this.sharing() || this.loading() || !this._preview()) {
      return;
    }
    const t = this.target();
    const scrub = this.scrub();
    this.shareError.set(null);
    this.sharing.set(true);
    try {
      if (t.kind === "meeting") {
        await this.ipc.shareMeetingToOrg(t.id, scrub);
      } else {
        await this.ipc.shareDocumentToOrg(t.id, scrub);
      }
      this.shared.emit();
    } catch (e) {
      this.shareError.set(this.friendlyError(String(e)));
    } finally {
      this.sharing.set(false);
    }
  }

  /** Cancel — nothing left the device. */
  cancel(): void {
    this.cancelled.emit();
  }

  /** A `Locked`/consent error → a plain message; else the raw backend error. */
  private friendlyError(raw: string): string {
    if (/Locked/i.test(raw)) {
      return "This item is locked — unlock its folder before adding it to the org brain.";
    }
    if (/consent/i.test(raw)) {
      return "You haven't allowed org sharing yet. Grant org-sharing consent in Settings → Organization.";
    }
    return raw;
  }

  /** Presentational: a byte count → a compact size label. */
  formatBytes(bytes: number): string {
    if (bytes < 1024) {
      return `${bytes} B`;
    }
    if (bytes < 1024 * 1024) {
      return `${(bytes / 1024).toFixed(1)} KB`;
    }
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  }
}
