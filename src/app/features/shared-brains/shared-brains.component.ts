import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  signal,
} from "@angular/core";

import type { OrgItemHeader } from "../../core/models";
import { IpcService } from "../../core/ipc.service";
import { TabsService } from "../../core/tabs.service";
import { MurIconComponent } from "../../design-system/icon/icon.component";
import { MurRowMenuComponent } from "../../design-system/row-menu/row-menu.component";
import { OrgBrainService } from "../../services/org-brain.service";
import { DestinationMoveService } from "../../shared/destination-move/destination-move.service";
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
  imports: [MurIconComponent, MurRowMenuComponent],
  templateUrl: "./shared-brains.component.html",
  styleUrl: "./shared-brains.component.scss",
})
export class SharedBrainsComponent {
  private readonly ipc = inject(IpcService);
  private readonly tabs = inject(TabsService);
  protected readonly orgBrain = inject(OrgBrainService);
  protected readonly workspace = inject(WorkspaceService);
  private readonly destinationMove = inject(DestinationMoveService);

  readonly activeOrgId = signal<string>("all");
  readonly kindFilter = signal<SharedKindFilter>("all");
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

  async openAddToSpace(row: SharedBrainRow, event?: Event): Promise<void> {
    // Legacy org rows without a trusted source kind can still be opened in
    // their read-only viewer, but the backend intentionally refuses to invent
    // whether they should become a local meeting or note.
    if (row.kind === "unclassified") {
      return;
    }
    const owned = row.item.ownedSource;
    if (owned && owned.movable !== true) return;
    let movedItem: { kind: "meeting" | "note"; id: string } | undefined;
    void this.destinationMove.open({
      kind: owned ? (owned.kind === "document" ? "note" : "meeting") : "shared",
      anchorKind: owned?.kind === "document" ? "note" : (owned?.kind ?? "org"),
      id: owned?.id ?? row.item.itemId,
      title: row.item.title || "Untitled",
      actionLabel: owned ? "Move" : "Add a copy",
      execute: async (containerId, confirmedEncryptionBoundary) => {
        const owned = row.item.ownedSource;
        if (owned) {
          const kind = owned.kind === "document" ? "note" : "meeting";
          if (kind === "meeting") {
            await this.ipc.moveNote(
              owned.id,
              containerId,
              confirmedEncryptionBoundary,
            );
          } else {
            if (!containerId) throw new Error("The Notes root is unavailable.");
            await this.ipc.moveNoteDoc(
              owned.id,
              containerId,
              confirmedEncryptionBoundary,
            );
          }
          movedItem = { kind, id: owned.id };
        } else {
          if (!containerId) throw new Error("Choose a Workspace or folder for this shared item.");
          movedItem = await this.ipc.addOrgItemToContainer(
            row.item.itemId,
            containerId,
          );
        }
      },
      afterMove: async () => {
        if (!movedItem) return;
        if (movedItem.kind === "meeting") {
          await this.tabs.openMeeting(movedItem.id, row.item.title || "Meeting");
        } else {
          await this.tabs.openNote(movedItem.id, row.item.title || "Note");
        }
      },
    }, event);
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
}
