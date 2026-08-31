import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
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

import { ErrorCopyService } from "../../../core/copy/error-copy.service";
import { IpcService } from "../../../core/ipc.service";
import type {
  ContainerSharePreview,
  ContainerShareStatus,
  OrgAccess,
  OrgStatus,
} from "../../../core/models";
import { MurSelectComponent } from "../../../design-system/select/select.component";
import { SharedWorkspaceService } from "../../../services/shared-workspace.service";

/** The local container this sheet is publishing. */
export interface ContainerShareTarget {
  /** The local `folders.id`. */
  id: string;
  name: string;
  /** `"project"` renders as "Workspace"; anything else as "Folder". */
  level: "project" | "folder";
}

/**
 * The "Share to Org" sheet for a whole Workspace or Folder — the container twin of
 * {@link OrgShareSheetComponent}, in the same grammar so the two read as one
 * feature.
 *
 * A FLOATING overlay, so it is OPAQUE `var(--surface-overlay)` +
 * `backdrop-filter: none` + a strong border + `--shadow-lg` — never the frosted
 * `.card` (trap T3), which would bleed the sidebar through it.
 *
 * What it must say out loud, because a container share is not a single note:
 * how many notes, meetings and sub-folders will go; what is deliberately being
 * LEFT BEHIND (sealed descendants, dashboards); and that transcripts and audio
 * never travel. A share of forty items is also forty round-trips, so the
 * progress bar is determinate, driven by the backend's content-free
 * `{done, total}` event rather than a spinner that cannot say how far it got.
 */
@Component({
  selector: "app-container-share-sheet",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [FormsModule, MurSelectComponent],
  templateUrl: "./container-share-sheet.component.html",
  styleUrl: "./container-share-sheet.component.scss",
})
export class ContainerShareSheetComponent {
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);
  private readonly destroyRef = inject(DestroyRef);
  private readonly errorCopy = inject(ErrorCopyService);
  private readonly shared = inject(SharedWorkspaceService);

  /** The Workspace or Folder being published. */
  readonly target = input.required<ContainerShareTarget>();

  /** Emitted after a successful publish (the host toasts + refreshes). */
  readonly sharedOut = output<void>();
  /** Emitted on cancel / Escape — nothing left the device. */
  readonly cancelled = output<void>();

  private readonly panel = viewChild<ElementRef<HTMLDivElement>>("panel");

  private readonly _orgs = signal<OrgStatus[]>([]);
  readonly orgs = this._orgs.asReadonly();
  readonly orgsLoading = signal(true);
  readonly orgsError = signal<string | null>(null);
  readonly selectedOrgId = signal("");

  readonly selectedOrg = computed(
    () => this._orgs().find((o) => o.orgId === this.selectedOrgId()) ?? null,
  );
  readonly singleOrg = computed(() => this._orgs().length === 1);

  /** Whether the regex PII scrub is on — default ON, per the redaction policy. */
  readonly scrub = signal(true);
  /** Member access for EVERY document in this container. View-only fails closed. */
  readonly access = signal<OrgAccess>("view");

  private readonly _preview = signal<ContainerSharePreview | null>(null);
  readonly preview = this._preview.asReadonly();
  readonly loading = signal(false);
  readonly previewError = signal<string | null>(null);

  readonly sharing = signal(false);
  readonly shareError = signal<string | null>(null);

  /** Items attempted so far and in total, for the determinate bar. */
  readonly progressDone = signal(0);
  readonly progressTotal = signal(0);
  readonly progressPercent = computed(() => {
    const total = this.progressTotal();
    return total > 0 ? Math.round((this.progressDone() / total) * 100) : 0;
  });

  /** "Workspace" or "Folder" — the word the user sees for this container. */
  readonly noun = computed(() =>
    this.target().level === "project" ? "Workspace" : "folder",
  );

  /** The container's existing share in the CHOSEN org, if any. */
  readonly existingShare = computed<ContainerShareStatus | null>(() => {
    const orgId = this.selectedOrgId();
    const folderId = this.target().id;
    return (
      this.shared
        .containerShares()
        .find(
          (share) => share.orgId === orgId && share.folderId === folderId,
        ) ?? null
    );
  });

  readonly alreadyShared = computed(() => this.existingShare() !== null);

  readonly audienceLabel = computed(() => {
    const org = this.selectedOrg();
    if (!org) {
      return "";
    }
    const noun = org.memberCount === 1 ? "member" : "members";
    return `${org.memberCount} ${noun} of ${org.name}`;
  });

  /** The one-line "what will go" summary. */
  readonly contentsLabel = computed(() => {
    const preview = this._preview();
    if (!preview) {
      return "";
    }
    const parts: string[] = [];
    if (preview.noteCount > 0) {
      parts.push(`${preview.noteCount} ${preview.noteCount === 1 ? "note" : "notes"}`);
    }
    if (preview.meetingCount > 0) {
      parts.push(
        `${preview.meetingCount} ${preview.meetingCount === 1 ? "meeting note" : "meeting notes"}`,
      );
    }
    if (preview.folderCount > 0) {
      parts.push(
        `${preview.folderCount} ${preview.folderCount === 1 ? "sub-folder" : "sub-folders"}`,
      );
    }
    return parts.length > 0 ? parts.join(" · ") : "Nothing to publish yet";
  });

  /** True when the container holds nothing publishable. */
  readonly isEmpty = computed(() => {
    const preview = this._preview();
    return (
      preview !== null &&
      preview.noteCount === 0 &&
      preview.meetingCount === 0 &&
      preview.folderCount === 0
    );
  });

  private progressUnlisten: (() => void) | null = null;

  constructor() {
    void this.loadOrgs();

    // Re-preview whenever the target or the chosen org changes. An async
    // IPC-on-signal-change effect, stale-guarded on the captured pair so a late
    // response for an org the user has since switched away from is dropped.
    effect(
      () => {
        const target = this.target();
        const orgId = this.selectedOrgId();
        void this.loadPreview(target.id, orgId);
      },
      { injector: this.injector },
    );

    void this.ipc
      .onContainerShareProgress((done, total) => {
        this.progressDone.set(done);
        this.progressTotal.set(total);
      })
      .then((un) => {
        this.progressUnlisten = un;
      })
      .catch(() => {
        /* no Tauri host: the bar simply stays at zero */
      });
    this.destroyRef.onDestroy(() => this.progressUnlisten?.());

    afterNextRender(() => this.panel()?.nativeElement.focus(), {
      injector: this.injector,
    });
  }

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

  private previewSeq = 0;

  private async loadPreview(folderId: string, orgId: string): Promise<void> {
    if (!orgId) {
      return;
    }
    const seq = ++this.previewSeq;
    this.loading.set(true);
    this.previewError.set(null);
    try {
      const preview = await this.ipc.previewContainerShare(orgId, folderId);
      if (seq === this.previewSeq) {
        this._preview.set(preview);
      }
    } catch (e) {
      if (seq === this.previewSeq) {
        this._preview.set(null);
        this.previewError.set(this.errorCopy.humanize(e));
      }
    } finally {
      if (seq === this.previewSeq) {
        this.loading.set(false);
      }
    }
  }

  protected setAccess(next: OrgAccess): void {
    this.access.set(next);
  }

  protected onScrubChange(event: Event): void {
    this.scrub.set((event.target as HTMLInputElement).checked);
  }

  protected async confirm(): Promise<void> {
    if (this.sharing() || !this.selectedOrgId()) {
      return;
    }
    this.sharing.set(true);
    this.shareError.set(null);
    this.progressDone.set(0);
    this.progressTotal.set(this._preview()?.totalItems ?? 0);
    try {
      await this.shared.share(
        this.selectedOrgId(),
        this.target().id,
        this.access(),
        this.scrub(),
      );
      this.sharedOut.emit();
    } catch (e) {
      this.shareError.set(this.errorCopy.humanize(e));
    } finally {
      this.sharing.set(false);
    }
  }

  /** Re-permission an already-shared container, walking every document under it. */
  protected async applyAccess(): Promise<void> {
    if (this.sharing()) {
      return;
    }
    this.sharing.set(true);
    this.shareError.set(null);
    try {
      await this.shared.setAccess(
        this.selectedOrgId(),
        this.target().id,
        this.access(),
      );
      this.sharedOut.emit();
    } catch (e) {
      this.shareError.set(this.errorCopy.humanize(e));
    } finally {
      this.sharing.set(false);
    }
  }

  protected async stopSharing(): Promise<void> {
    if (this.sharing()) {
      return;
    }
    this.sharing.set(true);
    this.shareError.set(null);
    try {
      await this.shared.unshare(this.selectedOrgId(), this.target().id);
      this.sharedOut.emit();
    } catch (e) {
      this.shareError.set(this.errorCopy.humanize(e));
    } finally {
      this.sharing.set(false);
    }
  }

  protected cancel(): void {
    this.cancelled.emit();
  }
}
