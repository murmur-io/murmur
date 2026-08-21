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
import { FormsModule } from "@angular/forms";
import { IpcService } from "../../../core/ipc.service";
import type {
  OrgAccess,
  OrgSharePreview,
  OrgSourceShareStatus,
  OrgStatus,
} from "../../../core/models";
import { MurSelectComponent } from "../../../design-system/select/select.component";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";

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
 * The "Add to Org Brain" PREVIEW + PICKER SHEET (Shared Brain v1). A FLOATING
 * overlay over the note/detail, so it is OPAQUE `var(--surface-overlay)` +
 * `backdrop-filter: none` + a strong border + `--shadow-lg` — NEVER the frosted
 * `.card` (trap T3), which would bleed the content behind it through.
 *
 * SELF-CONTAINED: it injects its own {@link IpcService} and owns the whole
 * preview + org-pick sub-state. It loads EVERY org the user belongs to
 * (`orgListStatuses`) into a picker (`<mur-select>`, defaulting to the first) so
 * the user chooses WHICH org to publish to — the CHOSEN `orgId` is threaded
 * through `shareMeetingToOrg` / `shareDocumentToOrg` (fixing the old
 * "shares to the FIRST org" bug). In exactly ONE org it shows the org as a
 * label, no redundant picker.
 *
 * Given the `target` (a meeting or note id) it fetches `previewOrgShare` and
 * renders EXACTLY the outgoing markdown (scrollable), its byte size + chunk
 * count, the PII scrub toggle (default ON) with the scrubbed counts, and a
 * "who can see this" line (chosen org name + member count). Confirm →
 * `shareMeetingToOrg` / `shareDocumentToOrg`; on success it emits `shared` so the
 * host can toast + refresh the badge. Cancel/backdrop/Escape emits `cancelled`.
 *
 * The scrub toggle re-runs `previewOrgShare` (the markdown + counts change with
 * the scrub setting) — the effect is stale-guarded on the current `(target,
 * scrub)` so a late response can't overwrite a newer one.
 */
@Component({
  selector: "app-org-share-sheet",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FormsModule, MurSelectComponent],
  templateUrl: "./org-share-sheet.component.html",
  styleUrl: "./org-share-sheet.component.scss",
})
export class OrgShareSheetComponent {
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);
  private readonly errorCopy = inject(ErrorCopyService);

  /** The local source (meeting / note) being published. */
  readonly target = input.required<OrgShareTarget>();

  /** Emitted after a successful publish (host toasts + refreshes the badge). */
  readonly shared = output<void>();
  /** Emitted on cancel / backdrop / Escape — nothing left the device. */
  readonly cancelled = output<void>();

  private readonly panel = viewChild<ElementRef<HTMLDivElement>>("panel");

  // ── Org picker ──────────────────────────────────────────────────────────────
  /** Every org the user belongs to; drives the picker + the audience line. */
  private readonly _orgs = signal<OrgStatus[]>([]);
  readonly orgs = this._orgs.asReadonly();
  /** True while the org list is loading (before the picker can render). */
  readonly orgsLoading = signal(true);
  /** An org-list load error (e.g. offline) — surfaced inline. */
  readonly orgsError = signal<string | null>(null);
  /** The chosen org id (bound to `<mur-select>`); defaults to the first org. */
  readonly selectedOrgId = signal("");

  /** The full OrgStatus for the chosen org (null until the list loads). */
  readonly selectedOrg = computed(
    () => this._orgs().find((o) => o.orgId === this.selectedOrgId()) ?? null,
  );
  /** True when there's exactly one org (show it as a label, no picker). */
  readonly singleOrg = computed(() => this._orgs().length === 1);

  /**
   * Orgs that ALREADY hold a live copy of this source (the double-click BLOCK).
   * Loaded once per open via `orgLiveSharesForSource`; the confirm button is
   * disabled — and the picker marks the org "Already added ✓" — for any org in
   * this set, so a re-click can never publish a duplicate.
   */
  private readonly _liveShares = signal<OrgSourceShareStatus[]>([]);
  /** Set of org ids that already hold a live share of this source. */
  readonly sharedOrgIds = computed(
    () => new Set(this._liveShares().map((s) => s.orgId)),
  );
  /** True when the CURRENTLY-selected org already holds this source. */
  readonly alreadyShared = computed(() =>
    this.sharedOrgIds().has(this.selectedOrgId()),
  );

  /** Whether the regex PII scrub is on (default ON per the redaction policy). */
  readonly scrub = signal(true);
  /** Member access for this document. View-only is the fail-closed default. */
  readonly access = signal<OrgAccess>("view");

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

  /** "who can see this" line: N members of the chosen org. */
  readonly audienceLabel = computed(() => {
    const org = this.selectedOrg();
    if (!org) {
      return "";
    }
    const noun = org.memberCount === 1 ? "member" : "members";
    return `${org.memberCount} ${noun} of ${org.name}`;
  });

  /** True when the scrub removed anything at the current setting (drives the counts row). */
  readonly scrubbedAny = computed(() => {
    const s = this._preview()?.scrubbed;
    return !!s && s.emails + s.phones + s.cards > 0;
  });

  constructor() {
    // Load every org the user belongs to → the picker. Default the selection to
    // the first org so a single-org user is one click from sharing.
    void this.loadOrgs();

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

    // Load which orgs ALREADY hold this source → block a re-share (dup fix). Keyed
    // on the target so it re-reads if the sheet is reused for another source.
    effect(
      () => {
        const t = this.target();
        void this.loadLiveShares(t);
      },
      { injector: this.injector },
    );

    // Land focus in the sheet so Escape works + it's announced.
    afterNextRender(() => this.panel()?.nativeElement.focus(), {
      injector: this.injector,
    });
  }

  /** Load the user's orgs; default the picker to the first (stable order). */
  private async loadOrgs(): Promise<void> {
    this.orgsError.set(null);
    this.orgsLoading.set(true);
    try {
      const list = await this.ipc.orgListStatuses();
      this._orgs.set(list);
      if (list.length && !list.some((o) => o.orgId === this.selectedOrgId())) {
        this.selectedOrgId.set(list[0].orgId);
      }
    } catch (e) {
      this.orgsError.set(this.errorCopy.humanize(e));
    } finally {
      this.orgsLoading.set(false);
    }
  }

  /**
   * Load which orgs already hold a live copy of this source (the re-share block).
   * Best-effort + stale-guarded on the target: a failure just leaves the set empty
   * (never blocks sharing) — the backend idempotency guard is the real safety net.
   */
  private async loadLiveShares(t: OrgShareTarget): Promise<void> {
    try {
      const live = await this.ipc.orgLiveSharesForSource(
        t.kind === "meeting" ? { meetingId: t.id } : { documentId: t.id },
      );
      if (this.target().id !== t.id) {
        return; // stale — the target changed under us
      }
      // Defensive: never let a malformed/non-array response (e.g. an unmocked
      // IPC command resolving to its generic `null` default) null-deref a
      // downstream `.some()`/`.find()` over this signal.
      this._liveShares.set(Array.isArray(live) ? live : []);
    } catch {
      if (this.target().id === t.id) {
        this._liveShares.set([]);
      }
    }
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
      this.previewError.set(this.errorCopy.humanize(e, "org-share"));
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

  /** Choose the precise member capability; management remains author/Owner-only. */
  setAccess(access: OrgAccess): void {
    if (!this.sharing()) {
      this.access.set(access);
    }
  }

  /**
   * Confirm the publish. Threads the CHOSEN org id through the meeting/note
   * command; on success emits `shared`.
   */
  async confirm(): Promise<void> {
    const orgId = this.selectedOrgId();
    if (
      this.sharing() ||
      this.loading() ||
      !this._preview() ||
      !orgId ||
      this.alreadyShared()
    ) {
      // `alreadyShared` blocks a duplicate publish — the note is already in this
      // org and edits sync automatically (re-publish-on-edit), so there is nothing
      // to do. The backend guard is idempotent regardless, but we never even ask.
      return;
    }
    const t = this.target();
    const scrub = this.scrub();
    const access = this.access();
    this.shareError.set(null);
    this.sharing.set(true);
    try {
      if (t.kind === "meeting") {
        await this.ipc.shareMeetingToOrg(t.id, orgId, scrub, access);
      } else {
        await this.ipc.shareDocumentToOrg(t.id, orgId, scrub, access);
      }
      this.shared.emit();
    } catch (e) {
      this.shareError.set(this.errorCopy.humanize(e, "org-share"));
    } finally {
      this.sharing.set(false);
    }
  }

  /** Cancel — nothing left the device. */
  cancel(): void {
    this.cancelled.emit();
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
