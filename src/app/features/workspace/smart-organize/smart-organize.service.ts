import { Injectable, Injector, afterNextRender, computed, inject, signal } from "@angular/core";

import { AskHistoryPrivacyBarrierService } from "../../../core/ask-history-privacy-barrier.service";
import { IpcService } from "../../../core/ipc.service";
import type {
  ContainerLevel,
  SmartOrganizeApplyResult,
  SmartOrganizeItemKind,
  SmartOrganizePlan,
  SmartOrganizePlanRequest,
  SmartOrganizeRule,
} from "../../../core/models";
import { DestinationMoveService } from "../../../shared/destination-move/destination-move.service";
import { WorkspaceService } from "../workspace.service";

export interface SmartOrganizePlace {
  readonly containerId: string | null;
  readonly level: ContainerLevel | null;
  readonly label: string;
  readonly breadcrumb: string;
}

@Injectable({ providedIn: "root" })
export class SmartOrganizeService {
  private readonly ipc = inject(IpcService);
  private readonly privacyBarrier = inject(AskHistoryPrivacyBarrierService);
  private readonly destinationMove = inject(DestinationMoveService);
  private readonly workspace = inject(WorkspaceService);
  private readonly injector = inject(Injector);
  private trigger: HTMLElement | null = null;

  private epoch = 0;
  readonly open = signal(false);
  readonly source = signal<SmartOrganizePlace | null>(null);
  readonly destination = signal<SmartOrganizePlace | null>(null);
  readonly includeDescendants = signal(true);
  readonly noteKind = signal(true);
  readonly meetingKind = signal(true);
  readonly rule = signal<SmartOrganizeRule>("byDay");
  readonly planning = signal(false);
  readonly applying = signal(false);
  readonly plan = signal<SmartOrganizePlan | null>(null);
  readonly receipt = signal<SmartOrganizeApplyResult | null>(null);
  readonly error = signal<string | null>(null);
  readonly uncertain = signal(false);
  readonly excluded = signal<ReadonlySet<string>>(new Set());
  readonly expandedSkipped = signal(false);
  readonly pageOffset = signal(0);
  readonly relationPageRange = computed(() => {
    const plan = this.plan();
    if (this.rule() !== "byRelation" || !plan || plan.totalScanned === 0) return null;
    return `Recordings ${this.pageOffset() + 1}–${Math.min(this.pageOffset() + 50, plan.totalScanned)}`;
  });

  readonly effectiveKinds = computed<readonly SmartOrganizeItemKind[]>(() => {
    if (this.rule() === "byRelation") return ["meeting"];
    const kinds: SmartOrganizeItemKind[] = [];
    if (this.noteKind()) kinds.push("note");
    if (this.meetingKind()) kinds.push("meeting");
    return kinds;
  });

  readonly effectiveDestination = computed(() => {
    const source = this.source();
    return source?.containerId ? source : this.destination();
  });

  readonly request = computed<SmartOrganizePlanRequest | null>(() => {
    const source = this.source();
    const destination = this.effectiveDestination();
    const kinds = this.effectiveKinds();
    if (!source || !destination?.containerId || kinds.length === 0) return null;
    return {
      sourceContainerId: source.containerId,
      includeDescendants: this.includeDescendants(),
      kinds: [...kinds],
      rule: this.rule(),
      destinationParentId: destination.containerId,
      ...(this.rule() === "byRelation" && this.pageOffset() > 0 ? { pageOffset: this.pageOffset() } : {}),
    };
  });

  readonly ruleSentence = computed(() => {
    const source = this.source();
    const destination = this.effectiveDestination();
    if (!source) return "Choose the Workspace, folder, or Not classified scope to organize.";
    const depth = source.containerId === null
      ? null
      : this.includeDescendants()
        ? source.level === "project" ? "the whole workspace" : "this folder and its subfolders"
        : source.level === "project" ? "this workspace only" : "this folder only";
    const place = `“${source.breadcrumb}”${depth ? ` (${depth})` : ""}`;
    if (this.rule() === "byRelation") {
      return `Group recordings in ${place} only when at least two in the current batch point directly to the same note, document, or explicitly linked folder. Manual links, wikilinks, and accepted semantic links count; suggestions and companion links do not. Recordings with more than one possible group stay put for review. Each preview considers up to 50 recordings; use the next batch to review more. New folders will be created in “${destination?.breadcrumb ?? "choose a destination"}”.`;
    }
    const kinds = this.effectiveKinds();
    const basis = kinds.length === 2
      ? "recordings by the day they started and notes by the day they were created"
      : kinds[0] === "meeting" ? "recordings by the day they started" : "notes by the day they were created";
    return `Put ${basis} from ${place} into date folders under “${destination?.breadcrumb ?? "choose a destination"}”. Dates use this Mac’s local calendar when this preview is made; Apply reuses those exact dates.`;
  });

  readonly selectedIds = computed(() => {
    const excluded = this.excluded();
    return (this.plan()?.buckets ?? []).flatMap((bucket) =>
      bucket.items.filter((item) => !excluded.has(item.itemId)).map((item) => item.itemId),
    );
  });
  readonly selectedCount = computed(() => this.selectedIds().length);
  readonly busy = computed(() => this.planning() || this.applying());

  show(event?: Event): void {
    this.trigger = event?.currentTarget instanceof HTMLElement ? event.currentTarget : null;
    this.epoch += 1;
    this.open.set(true);
    this.error.set(null);
    void this.workspace.ensureLoaded();
  }

  async chooseSource(event?: Event): Promise<void> {
    const anchor = this.pickerAnchor();
    if (!anchor) {
      this.error.set("Create a Workspace before choosing an organization scope.");
      return;
    }
    const epoch = this.epoch;
    const result = await this.destinationMove.open({
      kind: "scope",
      id: anchor.id,
      title: "Smart organize scope",
      currentContainerId: this.source()?.containerId ?? anchor.id,
      anchorKind: "container",
      allowUnclassified: true,
      actionLabel: "Keep",
    }, event);
    if (epoch !== this.epoch || !this.open() || !result.selected) return;
    this.changeInput();
    this.source.set({
      containerId: result.containerId ?? null,
      level: result.level ?? null,
      label: result.label ?? "Not classified",
      breadcrumb: result.breadcrumb ?? result.label ?? "Not classified",
    });
    if (result.containerId) this.destination.set(null);
    else {
      this.includeDescendants.set(false);
      this.noteKind.set(false);
      this.meetingKind.set(true);
    }
  }

  async chooseDestination(event?: Event): Promise<void> {
    const anchor = this.pickerAnchor();
    if (!anchor) return;
    const epoch = this.epoch;
    const result = await this.destinationMove.open({
      kind: "scope",
      id: anchor.id,
      title: "Smart organize destination",
      currentContainerId: this.destination()?.containerId ?? anchor.id,
      anchorKind: "container",
      allowUnclassified: false,
      actionLabel: "Keep",
    }, event);
    if (epoch !== this.epoch || !this.open() || !result.selected || !result.containerId) return;
    this.changeInput();
    this.destination.set({
      containerId: result.containerId,
      level: result.level ?? null,
      label: result.label ?? "Folder",
      breadcrumb: result.breadcrumb ?? result.label ?? "Folder",
    });
  }

  setKind(kind: SmartOrganizeItemKind, checked: boolean): void {
    this.changeInput();
    (kind === "note" ? this.noteKind : this.meetingKind).set(checked);
  }

  setRule(rule: SmartOrganizeRule): void {
    this.changeInput();
    this.rule.set(rule);
    if (rule === "byRelation") {
      this.noteKind.set(false);
      this.meetingKind.set(true);
    }
  }
  setDepth(value: boolean): void { this.changeInput(); this.includeDescendants.set(value); }

  toggleItem(itemId: string): void {
    if (this.applying()) return;
    this.excluded.update((current) => {
      const next = new Set(current);
      if (next.has(itemId)) next.delete(itemId);
      else next.add(itemId);
      return next;
    });
  }

  toggleBucket(itemIds: readonly string[]): void {
    if (this.applying()) return;
    const allSelected = itemIds.every((id) => !this.excluded().has(id));
    this.excluded.update((current) => {
      const next = new Set(current);
      for (const id of itemIds) {
        if (allSelected) next.add(id);
        else next.delete(id);
      }
      return next;
    });
  }

  bucketSelectedCount(itemIds: readonly string[]): number {
    const excluded = this.excluded();
    return itemIds.filter((id) => !excluded.has(id)).length;
  }

  async preview(): Promise<void> {
    const request = this.request();
    if (!request || this.busy()) return;
    const epoch = ++this.epoch;
    this.planning.set(true);
    this.error.set(null);
    try {
      if (!(await this.privacyBarrier.ensureReady()) || epoch !== this.epoch) return;
      const plan = await this.ipc.planSmartOrganize(request);
      if (epoch !== this.epoch) {
        void this.ipc.discardSmartOrganizePlan(plan.planId).catch(() => undefined);
        return;
      }
      this.plan.set(plan);
      this.excluded.set(new Set());
    } catch (cause) {
      if (epoch === this.epoch) this.error.set(this.message(cause, "Couldn’t build the preview."));
    } finally {
      if (epoch === this.epoch) this.planning.set(false);
    }
  }

  async apply(): Promise<void> {
    const plan = this.plan();
    const selectedItemIds = this.selectedIds();
    if (!plan || selectedItemIds.length === 0 || this.busy()) return;
    const epoch = ++this.epoch;
    this.applying.set(true);
    this.error.set(null);
    try {
      if (!(await this.privacyBarrier.ensureReady()) || epoch !== this.epoch) return;
      const receipt = await this.ipc.applySmartOrganizePlan(plan.planId, selectedItemIds);
      if (epoch !== this.epoch) return;
      this.plan.set(null);
      this.excluded.set(new Set());
      this.uncertain.set(false);
      this.pageOffset.set(0);
      this.receipt.set(receipt);
      await this.workspace.reload();
    } catch (cause) {
      if (epoch !== this.epoch) return;
      this.plan.set(null);
      this.excluded.set(new Set());
      this.pageOffset.set(0);
      this.uncertain.set(true);
      this.error.set(`${this.message(cause, "The result is uncertain.")} Preview again before applying anything else.`);
      await this.workspace.reload();
    } finally {
      if (epoch === this.epoch) this.applying.set(false);
    }
  }

  previewAgain(): void { this.pageOffset.set(0); this.receipt.set(null); this.error.set(null); }

  async previewNextBatch(): Promise<void> {
    const next = this.plan()?.nextPageOffset;
    if (next == null || this.busy()) return;
    this.changeInput();
    this.pageOffset.set(next);
    await this.preview();
  }

  editChoices(): void {
    if (this.busy()) return;
    this.changeInput();
  }

  close(): void {
    if (this.applying()) return;
    this.scrub();
    const trigger = this.trigger;
    this.trigger = null;
    if (trigger) afterNextRender(() => trigger.focus({ preventScroll: true }), { injector: this.injector });
  }

  /** Synchronous privacy boundary; async discards are cleanup, never authorization. */
  scrub(): void {
    const planId = this.plan()?.planId;
    this.epoch += 1;
    this.open.set(false);
    this.planning.set(false);
    this.applying.set(false);
    this.uncertain.set(false);
    this.pageOffset.set(0);
    this.source.set(null);
    this.destination.set(null);
    this.plan.set(null);
    this.receipt.set(null);
    this.error.set(null);
    this.excluded.set(new Set());
    if (planId) void this.ipc.discardSmartOrganizePlan(planId).catch(() => undefined);
  }

  private changeInput(): void {
    this.pageOffset.set(0);
    const planId = this.plan()?.planId;
    this.plan.set(null);
    this.receipt.set(null);
    this.excluded.set(new Set());
    this.error.set(null);
    if (planId) void this.ipc.discardSmartOrganizePlan(planId).catch(() => undefined);
  }

  private message(cause: unknown, fallback: string): string {
    const raw = cause instanceof Error ? cause.message : typeof cause === "string" ? cause : "";
    return raw.trim().slice(0, 240) || fallback;
  }

  private pickerAnchor() {
    return this.workspace.forest().find(
      (node) => node.level === "project" && !node.locked && !node.unlocked && !node.isRoot,
    );
  }
}
