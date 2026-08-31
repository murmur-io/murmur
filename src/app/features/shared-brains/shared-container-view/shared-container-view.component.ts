import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
} from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import { ActivatedRoute, Router } from "@angular/router";
import { map } from "rxjs";

import type { SharedContainerNode, SharedItemRow } from "../../../core/models";
import { MurEmptyStateComponent } from "../../../design-system/empty-state/empty-state.component";
import { MurIconComponent } from "../../../design-system/icon/icon.component";
import { SharedWorkspaceService } from "../../../services/shared-workspace.service";

/**
 * One RECEIVED container's page — a Workspace or folder somebody in the org shared.
 *
 * Deliberately read-only STRUCTURE at every access level. "Can edit" is about a
 * document's text, which the org viewer already owns; renaming, moving or
 * deleting what is inside belongs to whoever shared it, so those affordances
 * never appear here. Offering them would promise a capability the relay refuses.
 */
@Component({
  selector: "app-shared-container-view",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurEmptyStateComponent, MurIconComponent],
  templateUrl: "./shared-container-view.component.html",
  styleUrl: "./shared-container-view.component.scss",
})
export class SharedContainerViewComponent {
  private readonly router = inject(Router);
  private readonly route = inject(ActivatedRoute);
  private readonly shared = inject(SharedWorkspaceService);

  // The route params as signals, so navigating between two shared containers
  // re-resolves in place. The router does not bind params to inputs in this app
  // (`withComponentInputBinding` is not provided), which is the same reason the
  // org item viewer reads them this way.
  private readonly orgId = toSignal(
    this.route.paramMap.pipe(map((p) => p.get("orgId"))),
    { initialValue: this.route.snapshot.paramMap.get("orgId") },
  );
  private readonly containerId = toSignal(
    this.route.paramMap.pipe(map((p) => p.get("containerId"))),
    { initialValue: this.route.snapshot.paramMap.get("containerId") },
  );

  readonly loading = this.shared.loading;

  /** The node itself, found anywhere in the received forest. */
  readonly node = computed<SharedContainerNode | null>(() => {
    const orgId = this.orgId();
    const containerId = this.containerId();
    if (!orgId || !containerId) {
      return null;
    }
    const find = (nodes: SharedContainerNode[]): SharedContainerNode | null => {
      for (const node of nodes) {
        if (node.orgId === orgId && node.containerId === containerId) {
          return node;
        }
        const inChild = find(node.folders);
        if (inChild) {
          return inChild;
        }
      }
      return null;
    };
    const brains = this.shared.sharedBrains();
    return (
      find(this.shared.spaces()) ?? (brains ? find(brains.folders) : null)
    );
  });

  readonly accessLabel = computed(() =>
    this.node()?.access === "edit" ? "Can edit" : "View only",
  );

  readonly noun = computed(() =>
    this.node()?.level === "folder" ? "folder" : "Workspace",
  );

  constructor() {
    if (this.shared.sharedBrains() === null) {
      void this.shared.load();
    }
  }

  protected openItem(item: SharedItemRow): void {
    void this.router.navigate(["/org-item", item.itemId]);
  }

  protected openFolder(folder: SharedContainerNode): void {
    void this.router.navigate(["/shared", folder.orgId, folder.containerId]);
  }

  protected itemTitle(item: SharedItemRow): string {
    return item.title.trim() || "Untitled";
  }

  protected itemMeta(item: SharedItemRow): string {
    const kind = item.kind === "meeting" ? "Meeting" : "Note";
    return `${kind} · ${item.authorHint}`;
  }
}
