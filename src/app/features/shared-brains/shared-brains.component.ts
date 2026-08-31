import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  signal,
} from "@angular/core";

import type { ItemKind, OrgItemHeader } from "../../core/models";
import { IpcService } from "../../core/ipc.service";
import { TabsService } from "../../core/tabs.service";
import { MurIconComponent } from "../../design-system/icon/icon.component";
import { MurRowMenuComponent } from "../../design-system/row-menu/row-menu.component";
import { OrgBrainService } from "../../services/org-brain.service";
import { ToastService } from "../../services/toast.service";
import { workspaceDestinations } from "../workspace/workspace-destination";
import type { WorkspaceDestination } from "../workspace/workspace-destination";
import { WorkspaceMoveSheetComponent } from "../workspace/workspace-move-sheet/workspace-move-sheet.component";
import { WorkspaceService } from "../workspace/workspace.service";

type SharedKindFilter = "all" | "meeting" | "note";

interface SharedBrainRow {
  readonly key: string;
  readonly orgId: string;
  readonly orgName: string;
  readonly item: OrgItemHeader;
  readonly kind: "meeting" | "note" | "unclassified";
  readonly displayDate: string;
  readonly sortAt: number;
}

/** Dedicated top-level browser for received and authored organization replicas. */
@Component({
  selector: "app-shared-brains",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurIconComponent, MurRowMenuComponent, WorkspaceMoveSheetComponent],
  templateUrl: "./shared-brains.component.html",
  styleUrl: "./shared-brains.component.scss",
})
export class SharedBrainsComponent {
  private readonly ipc = inject(IpcService);
  private readonly tabs = inject(TabsService);
  private readonly toast = inject(ToastService);
  protected readonly orgBrain = inject(OrgBrainService);
  protected readonly workspace = inject(WorkspaceService);

  readonly activeOrgId = signal<string>("all");
  readonly kindFilter = signal<SharedKindFilter>("all");
  readonly addRequest = signal<SharedBrainRow | null>(null);
  readonly addBusy = signal(false);
  readonly addError = signal<string | null>(null);

  readonly targets = computed(() => {
    const destinations = workspaceDestinations(this.workspace.forest());
    return this.addRequest()?.item.ownedSource
      ? destinations
      : destinations.filter(({ container }) => !container.locked);
  });
  readonly addRequestItem = computed(() => {
    const row = this.addRequest();
    return row
      ? {
          kind: row.kind,
          title: row.item.title || "Untitled",
        }
      : null;
  });
  readonly addActionVerb = computed<"Move" | "Add a copy">(() =>
    this.addRequest()?.item.ownedSource ? "Move" : "Add a copy",
  );

  readonly rows = computed<SharedBrainRow[]>(() => {
    const activeOrgId = this.activeOrgId();
    const kindFilter = this.kindFilter();
    return this.orgBrain
      .orgs()
      .filter((org) => activeOrgId === "all" || org.orgId === activeOrgId)
      .flatMap((org) =>
        (this.orgBrain.orgItems()[org.orgId] ?? []).map((item) => {
          const sortAt = Date.parse(item.createdAt);
          return {
            key: `${org.orgId}:${item.itemId}`,
            orgId: org.orgId,
            orgName: org.name,
            item,
            kind:
              item.kind === "meeting"
                ? "meeting"
                : item.kind === "document"
                  ? "note"
                  : "unclassified",
            displayDate: this.formatDate(item.createdAt),
            sortAt: Number.isNaN(sortAt) ? 0 : sortAt,
          } satisfies SharedBrainRow;
        }),
      )
      .filter((row) => kindFilter === "all" || row.kind === kindFilter)
      .sort((left, right) => right.sortAt - left.sortAt);
  });

  readonly listEmpty = computed(() => this.rows().length === 0);

  constructor() {
    // Route entry is deliberately local-only. Existing explicit refresh
    // surfaces still call loadOrgs(), but simply opening Shared Brains must not
    // turn navigation into network egress.
    void this.orgBrain.loadLocalOrgs();
    if (this.workspace.forestEmpty()) {
      void this.workspace.reload();
    }
  }

  selectOrg(orgId: string): void {
    this.activeOrgId.set(orgId);
  }

  selectKind(kind: SharedKindFilter): void {
    this.kindFilter.set(kind);
  }

  openRow(row: SharedBrainRow): void {
    const owned = row.item.ownedSource;
    if (owned?.kind === "meeting") {
      void this.tabs.openMeeting(owned.id, row.item.title || "Meeting");
    } else if (owned?.kind === "document") {
      void this.tabs.openNote(owned.id, row.item.title || "Note");
    } else {
      void this.tabs.openOrgItem(
        row.item.itemId,
        row.item.title || "Shared item",
      );
    }
  }

  async openAddToSpace(row: SharedBrainRow): Promise<void> {
    // Legacy org rows without a trusted source kind can still be opened in
    // their read-only viewer, but the backend intentionally refuses to invent
    // whether they should become a local meeting or note.
    if (row.kind === "unclassified") {
      return;
    }
    if (this.workspace.forestEmpty()) {
      await this.workspace.reload();
    }
    this.addError.set(null);
    this.addRequest.set(row);
  }

  closeAddToSpace(): void {
    if (!this.addBusy()) {
      this.addRequest.set(null);
      this.addError.set(null);
    }
  }

  async addToSpace(target: WorkspaceDestination): Promise<void> {
    const row = this.addRequest();
    if (!row || this.addBusy()) {
      return;
    }
    this.addBusy.set(true);
    this.addError.set(null);
    try {
      const owned = row.item.ownedSource;
      let result: { kind: "meeting" | "note"; id: string };
      if (owned) {
        const kind: ItemKind = owned.kind === "document" ? "note" : "meeting";
        await this.workspace.moveItem(kind, owned.id, target.container.id);
        result = { kind, id: owned.id };
      } else {
        result = await this.ipc.addOrgItemToContainer(
          row.item.itemId,
          target.container.id,
        );
        await this.workspace.reload();
      }
      this.addRequest.set(null);
      const completedVerb = owned ? "Moved" : "Added a copy of";
      this.toast.success(
        `${completedVerb} “${row.item.title || "Untitled"}” to ${target.label}`,
      );
      if (result.kind === "meeting") {
        await this.tabs.openMeeting(result.id, row.item.title || "Meeting");
      } else {
        await this.tabs.openNote(result.id, row.item.title || "Note");
      }
    } catch (error) {
      this.addError.set(this.readableError(error));
    } finally {
      this.addBusy.set(false);
    }
  }

  private formatDate(value: string): string {
    const date = new Date(value);
    if (Number.isNaN(date.getTime())) {
      return "Date unavailable";
    }
    return new Intl.DateTimeFormat(undefined, {
      dateStyle: "medium",
      timeStyle: "short",
    }).format(date);
  }

  private readableError(error: unknown): string {
    const raw =
      typeof error === "string"
        ? error
        : error && typeof error === "object" && "message" in error
          ? String((error as { message: unknown }).message)
          : "";
    const normalized = raw.replace(/^invalid argument:\s*/i, "").trim();
    return normalized
      ? normalized.slice(0, 240)
      : "Couldn’t add this shared item to the selected Workspace. Please try again.";
  }
}
