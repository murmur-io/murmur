import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  computed,
  effect,
  inject,
  input,
  signal,
} from "@angular/core";
import { RouterLink } from "@angular/router";

import { IpcService } from "../../../core/ipc.service";
import type { ContainerNode } from "../../../core/models";
import { AskHistoryPrivacyBarrierService } from "../../../core/ask-history-privacy-barrier.service";
import { DestinationMoveService } from "../../../shared/destination-move/destination-move.service";
import { WorkspaceService } from "../../workspace/workspace.service";

interface CurrentRecordingLocation {
  readonly id: string;
  readonly label: string;
  readonly locked: boolean;
}

/** One focused final result: open the saved meeting or file it without leaving. */
@Component({
  selector: "app-recording-placement",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  templateUrl: "./recording-placement.component.html",
  styleUrl: "./recording-placement.component.scss",
})
export class RecordingPlacementComponent {
  private readonly ipc = inject(IpcService);
  private readonly workspace = inject(WorkspaceService);
  private readonly privacyBarrier = inject(AskHistoryPrivacyBarrierService);
  private readonly destinationMove = inject(DestinationMoveService);
  private readonly destroyRef = inject(DestroyRef);

  readonly meetingId = input<string | null>(null);
  readonly exportedPath = input<string | null>(null);

  private readonly _filing = signal(false);
  readonly filing = this._filing.asReadonly();
  private readonly _currentFolderId = signal<string | null>(null);
  private readonly _meetingTitle = signal<string | null>(null);
  private readonly _placementState = signal<
    "idle" | "loading" | "resolved" | "masked" | "unavailable"
  >("idle");
  readonly placementState = this._placementState.asReadonly();
  readonly placementMasked = computed(
    () => this._placementState() === "masked",
  );
  private readonly _error = signal<string | null>(null);
  readonly error = this._error.asReadonly();
  private placementRequest = 0;
  private privacyGeneration = 0;
  private destinationLoadMeetingId: string | null = null;

  constructor() {
    // Lock/privacy events are not replayed. Scrub synchronously and invalidate
    // every older detail/filing continuation before a canonical gated re-read.
    const unregister = this.privacyBarrier.registerInvalidator(() =>
      this.maskForPrivacyChange(),
    );
    this.destroyRef.onDestroy(unregister);
  }

  /**
   * A meeting change is a new review visit. Resolve its actual parent from the
   * gated canonical meeting reader; never infer it from a floating-bar choice
   * that belongs to another webview and may no longer be current.
   */
  private readonly _resetPerMeeting = effect(() => {
    const meetingId = this.meetingId();
    const request = ++this.placementRequest;
    this._filing.set(false);
    this._currentFolderId.set(null);
    this._meetingTitle.set(null);
    this._placementState.set(meetingId ? "loading" : "idle");
    this._error.set(null);
    if (meetingId) {
      void this.resolveCurrentPlacement(meetingId, request);
    }
  });

  /** Load the cached forest only to render the collapsed current-location summary. */
  private readonly _load = effect(() => {
    const meetingId = this.meetingId();
    if (!meetingId) {
      this.destinationLoadMeetingId = null;
      return;
    }
    if (
      !this.workspace.loaded() &&
      !this.workspace.loading() &&
      this.destinationLoadMeetingId !== meetingId
    ) {
      // `ensureLoaded()`, not `reload()`, and keyed on `loaded()` rather than
      // emptiness: the sidebar's two tree instances also ask for the forest at
      // boot, and an empty forest is a legitimate RESULT — so an emptiness
      // guard re-read a forest that had just been read and come back empty.
      // `recording-placement.spec.ts`'s "an empty destination forest loads once,
      // stays calm, and retries only on request" is the existing cache oracle.
      this.destinationLoadMeetingId = meetingId;
      void this.workspace.ensureLoaded();
    }
  });

  readonly filedIn = computed<CurrentRecordingLocation | null>(() => {
    const folderId = this._currentFolderId();
    return folderId === null
      ? null
      : findCurrentLocation(this.workspace.forest(), folderId);
  });

  readonly placementUnavailable = computed(
    () =>
      this._placementState() === "unavailable" ||
      (this._placementState() === "resolved" &&
        this._currentFolderId() !== null &&
        !this.filedIn() &&
        !this.workspace.loading()),
  );

  readonly locationCopy = computed(() => {
    switch (this._placementState()) {
      case "loading":
        return "Checking location…";
      case "unavailable":
        return "Location unavailable";
      case "resolved":
        if (this._currentFolderId() === null) return "Unfiled";
        return (
          this.filedIn()?.label ??
          (this.workspace.loading()
            ? "Checking location…"
            : "Location unavailable")
        );
      default:
        return "";
    }
  });

  readonly savedCopy = computed(() =>
    this.exportedPath()
      ? "Saved in Murmur and exported to your vault."
      : "Saved safely in Murmur on this Mac.",
  );

  async openMove(event?: Event): Promise<void> {
    const meetingId = this.meetingId();
    const privacyGeneration = this.privacyGeneration;
    if (
      !meetingId ||
      this._filing() ||
      this.placementMasked() ||
      this.filedIn()?.locked === true
    ) {
      return;
    }

    this._error.set(null);
    this._filing.set(true);
    try {
      const { moved } = await this.destinationMove.open({
        kind: "meeting",
        id: meetingId,
        title: this._meetingTitle() || "Recording",
        currentContainerId:
          this._placementState() === "resolved"
            ? this._currentFolderId()
            : undefined,
        actionLabel: "Move",
      }, event);
      if (
        !moved ||
        meetingId !== this.meetingId() ||
        privacyGeneration !== this.privacyGeneration
      ) {
        return;
      }
      const request = ++this.placementRequest;
      this._placementState.set("loading");
      await Promise.all([
        this.workspace.reload(),
        this.resolveCurrentPlacement(meetingId, request),
      ]);
    } catch {
      if (
        meetingId === this.meetingId() &&
        privacyGeneration === this.privacyGeneration
      ) {
        this._error.set("Couldn’t move this recording. Nothing was lost.");
      }
    } finally {
      if (
        meetingId === this.meetingId() &&
        privacyGeneration === this.privacyGeneration
      ) {
        this._filing.set(false);
      }
    }
  }

  retryPlacement(): void {
    const meetingId = this.meetingId();
    if (
      !meetingId ||
      this._placementState() === "loading" ||
      this.workspace.loading()
    )
      return;
    const request = ++this.placementRequest;
    this._placementState.set("loading");
    void this.resolveCurrentPlacement(meetingId, request);
    if (this.workspace.forestEmpty()) {
      void this.workspace.reload();
    }
  }

  private async resolveCurrentPlacement(
    meetingId: string,
    request: number,
  ): Promise<void> {
    try {
      const privacyReady = await this.privacyBarrier.ensureReady();
      if (request !== this.placementRequest || meetingId !== this.meetingId()) {
        return;
      }
      if (!privacyReady) {
        this._currentFolderId.set(null);
        this._meetingTitle.set(null);
        this._placementState.set("masked");
        return;
      }
      const detail = await this.ipc.getMeetingDetail(meetingId);
      if (request !== this.placementRequest || meetingId !== this.meetingId()) {
        return;
      }
      if (!detail) {
        this._placementState.set("unavailable");
        return;
      }
      if (detail.locked) {
        this._currentFolderId.set(null);
        this._meetingTitle.set(null);
        this._error.set(null);
        this._placementState.set("masked");
        return;
      }
      this._currentFolderId.set(detail.meeting.folderId ?? null);
      this._meetingTitle.set(detail.meeting.title);
      this._placementState.set("resolved");
    } catch {
      if (request === this.placementRequest && meetingId === this.meetingId()) {
        this._placementState.set("unavailable");
      }
    }
  }

  private maskForPrivacyChange(): void {
    const meetingId = this.meetingId();
    const request = ++this.placementRequest;
    ++this.privacyGeneration;
    this._currentFolderId.set(null);
    this._meetingTitle.set(null);
    this._filing.set(false);
    this._error.set(null);
    this._placementState.set(meetingId ? "masked" : "idle");
    if (meetingId) {
      void this.resolveCurrentPlacement(meetingId, request);
    }
  }
}

function findCurrentLocation(
  forest: readonly ContainerNode[],
  id: string,
): CurrentRecordingLocation | null {
  const visit = (
    nodes: readonly ContainerNode[],
    ancestors: readonly string[],
  ): CurrentRecordingLocation | null => {
    for (const node of nodes) {
      const labels = [...ancestors, node.name];
      if (node.id === id) {
        return { id, label: labels.join(" / "), locked: node.locked };
      }
      const child = visit(node.folders, labels);
      if (child) {
        return child;
      }
    }
    return null;
  };

  return visit(forest, []);
}
