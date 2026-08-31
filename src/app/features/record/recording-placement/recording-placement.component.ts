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
import { AskHistoryPrivacyBarrierService } from "../../../core/ask-history-privacy-barrier.service";
import { WorkspaceService } from "../../workspace/workspace.service";
import {
  flattenRecordingDestinations,
  type RecordingDestination,
} from "./recording-destinations";

const LAST_DESTINATION_KEY = "murmur.recording.lastDestination";

/** One focused final result: open the saved meeting or file it without leaving. */
@Component({
  selector: "app-recording-placement",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  host: { "(document:keydown.escape)": "closePicker()" },
  templateUrl: "./recording-placement.component.html",
  styleUrl: "./recording-placement.component.scss",
})
export class RecordingPlacementComponent {
  private readonly ipc = inject(IpcService);
  private readonly workspace = inject(WorkspaceService);
  private readonly privacyBarrier = inject(AskHistoryPrivacyBarrierService);
  private readonly destroyRef = inject(DestroyRef);
  readonly destinationsLoading = this.workspace.loading;

  readonly meetingId = input<string | null>(null);
  readonly exportedPath = input<string | null>(null);

  private readonly _pickerOpen = signal(false);
  readonly pickerOpen = this._pickerOpen.asReadonly();
  private readonly _filing = signal(false);
  readonly filing = this._filing.asReadonly();
  private readonly _currentFolderId = signal<string | null>(null);
  private readonly _placementState = signal<
    "idle" | "loading" | "resolved" | "masked" | "unavailable"
  >("idle");
  readonly placementState = this._placementState.asReadonly();
  readonly placementMasked = computed(
    () => this._placementState() === "masked",
  );
  private readonly _error = signal<string | null>(null);
  readonly error = this._error.asReadonly();
  private readonly _lastAttempt = signal<RecordingDestination | null>(null);
  readonly query = signal("");
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
    this._pickerOpen.set(false);
    this._filing.set(false);
    this._currentFolderId.set(null);
    this._placementState.set(meetingId ? "loading" : "idle");
    this._error.set(null);
    this._lastAttempt.set(null);
    this.query.set("");
    if (meetingId) {
      void this.resolveCurrentPlacement(meetingId, request);
    }
  });

  /** Load destinations only for the collapsed post-final control. */
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
      // stays calm, and retries only on request" is the oracle. The explicit
      // "Refresh locations" button still calls `reload()` directly.
      this.destinationLoadMeetingId = meetingId;
      void this.workspace.ensureLoaded();
    }
  });

  readonly destinations = computed<RecordingDestination[]>(() => {
    const rows = flattenRecordingDestinations(this.workspace.forest());
    const rememberedId = readLastDestination();
    if (!rememberedId) return rows;
    const remembered = rows.find(
      (destination) => destination.id === rememberedId && !destination.blocked,
    );
    return remembered
      ? [
          remembered,
          ...rows.filter((destination) => destination !== remembered),
        ]
      : rows;
  });

  readonly visibleDestinations = computed(() => {
    const needle = this.query().trim().toLowerCase();
    return needle
      ? this.destinations().filter((destination) =>
          destination.label.toLowerCase().includes(needle),
        )
      : this.destinations();
  });

  readonly filedIn = computed<RecordingDestination | null>(() => {
    const folderId = this._currentFolderId();
    return folderId === null
      ? null
      : (this.destinations().find(
          (destination) => destination.id === folderId,
        ) ?? null);
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

  togglePicker(): void {
    if (!this._filing() && !this.placementMasked()) {
      this._pickerOpen.update((open) => !open);
      this._error.set(null);
    }
  }

  closePicker(): void {
    if (!this._filing()) {
      this._pickerOpen.set(false);
      this._error.set(null);
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

  retryDestinations(): void {
    if (!this.workspace.loading()) {
      void this.workspace.reload();
    }
  }

  async file(destination: RecordingDestination): Promise<void> {
    const meetingId = this.meetingId();
    const privacyGeneration = this.privacyGeneration;
    if (
      !meetingId ||
      destination.blocked ||
      this._filing() ||
      this.placementMasked()
    )
      return;

    this._lastAttempt.set(destination);
    this._error.set(null);
    this._filing.set(true);
    try {
      await this.workspace.moveItem("meeting", meetingId, destination.id);
      if (
        meetingId !== this.meetingId() ||
        privacyGeneration !== this.privacyGeneration
      )
        return;
      const request = ++this.placementRequest;
      this._placementState.set("loading");
      await this.resolveCurrentPlacement(meetingId, request);
      if (
        meetingId !== this.meetingId() ||
        privacyGeneration !== this.privacyGeneration
      )
        return;
      writeLastDestination(destination.id);
      this._pickerOpen.set(false);
      this.query.set("");
    } catch {
      if (
        meetingId === this.meetingId() &&
        privacyGeneration === this.privacyGeneration
      ) {
        this._error.set(
          `Couldn’t move this recording to ${destination.label}. Nothing was lost.`,
        );
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

  retry(): void {
    const destination = this._lastAttempt();
    if (destination) void this.file(destination);
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
        this._pickerOpen.set(false);
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
        this._pickerOpen.set(false);
        this._error.set(null);
        this._lastAttempt.set(null);
        this.query.set("");
        this._placementState.set("masked");
        return;
      }
      this._currentFolderId.set(detail.meeting.folderId ?? null);
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
    this._pickerOpen.set(false);
    this._filing.set(false);
    this._error.set(null);
    this._lastAttempt.set(null);
    this.query.set("");
    this._placementState.set(meetingId ? "masked" : "idle");
    if (meetingId) {
      void this.resolveCurrentPlacement(meetingId, request);
    }
  }
}

function readLastDestination(): string | null {
  try {
    return localStorage.getItem(LAST_DESTINATION_KEY);
  } catch {
    return null;
  }
}

function writeLastDestination(id: string): void {
  try {
    localStorage.setItem(LAST_DESTINATION_KEY, id);
  } catch {
    // Filing succeeds even when remembering the convenience choice is unavailable.
  }
}
