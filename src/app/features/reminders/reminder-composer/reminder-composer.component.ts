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
  signal,
  viewChild,
} from "@angular/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { IpcService } from "../../../core/ipc.service";
import type {
  ReminderDraft,
  ReminderSourceUpdatedPayload,
  ReminderRepeatUnit,
  SourceKind,
  SourceRef,
} from "../../../core/models";
import { SourcePickerComponent } from "../../../design-system/source-picker/source-picker.component";
import { RemindersStore } from "../reminders.store";
import {
  ReminderComposerService,
  type ReminderComposerRequest,
} from "./reminder-composer.service";

const DEFAULT_DUE_OFFSET_MS = 60 * 60 * 1000;
const MAX_SOURCES = 20;
const REMINDER_SOURCE_KINDS = ["meeting", "note"] as const;
const FOCUSABLE_SELECTOR = [
  "button:not([disabled])",
  "a[href]",
  "input:not([disabled])",
  "select:not([disabled])",
  "textarea:not([disabled])",
  '[tabindex]:not([tabindex="-1"])',
].join(",");
const OVERLAY_FOCUSABLE_SELECTOR = FOCUSABLE_SELECTOR.split(",")
  .map((selector) => `.sp-overlay ${selector}`)
  .join(",");

function localDateParts(epoch: number): { date: string; time: string } {
  const value = new Date(epoch);
  const year = String(value.getFullYear()).padStart(4, "0");
  const month = String(value.getMonth() + 1).padStart(2, "0");
  const day = String(value.getDate()).padStart(2, "0");
  const hours = String(value.getHours()).padStart(2, "0");
  const minutes = String(value.getMinutes()).padStart(2, "0");
  return { date: `${year}-${month}-${day}`, time: `${hours}:${minutes}` };
}

function matchesInvalidation(
  source: Pick<SourceRef, "kind" | "id">,
  invalidation: ReminderSourceUpdatedPayload,
): boolean {
  return source.kind === invalidation.kind && source.id === invalidation.id;
}

@Component({
  selector: "app-reminder-composer",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [SourcePickerComponent],
  host: {
    "(document:keydown)": "onDocumentKeydown($event)",
    "(document:mousedown)": "rememberPointerTarget($event)",
  },
  templateUrl: "./reminder-composer.component.html",
  styleUrl: "./reminder-composer.component.scss",
})
export class ReminderComposerComponent {
  readonly service = inject(ReminderComposerService);
  private readonly store = inject(RemindersStore);
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);
  private readonly destroyRef = inject(DestroyRef);

  private readonly titleInput =
    viewChild<ElementRef<HTMLInputElement>>("titleInput");
  private readonly panel = viewChild<ElementRef<HTMLElement>>("panel");

  readonly title = signal("");
  readonly details = signal("");
  readonly date = signal("");
  readonly time = signal("");
  readonly repeats = signal(false);
  readonly repeatEvery = signal(1);
  readonly repeatUnit = signal<ReminderRepeatUnit>("weeks");
  readonly sources = signal<SourceRef[]>([]);
  readonly busy = signal(false);
  readonly error = signal<string | null>(null);
  readonly allowedKinds = REMINDER_SOURCE_KINDS;
  private destroyed = false;
  private sourceUpdateUnlisten: UnlistenFn | null = null;
  private visibilityInvalidatedUnlisten: UnlistenFn | null = null;
  private hydratedRequestKey: number | null = null;
  private restoreFocusTo: HTMLElement | null = null;
  private lastPointerTarget: HTMLElement | null = null;
  readonly listenerState = signal<"pending" | "ready" | "failed">("pending");

  readonly heading = computed(() => {
    const request = this.service.request();
    if (request?.mode === "edit") {
      return "Edit reminder";
    }
    if (request?.mode === "suggestion") {
      return "Review Smart reminder";
    }
    return "New reminder";
  });

  readonly submitLabel = computed(() => {
    const request = this.service.request();
    if (this.busy()) {
      return request?.mode === "edit" ? "Saving…" : "Creating…";
    }
    return request?.mode === "edit" ? "Save changes" : "Create reminder";
  });

  readonly sourceLimitReached = computed(
    () => this.sources().length >= MAX_SOURCES,
  );

  readonly valid = computed(() => {
    const title = this.title().trim();
    const due = this.dueEpoch();
    const recurrenceValid =
      !this.repeats() ||
      (Number.isInteger(this.repeatEvery()) &&
        this.repeatEvery() >= 1 &&
        this.repeatEvery() <= 365);
    return (
      title.length > 0 &&
      title.length <= 240 &&
      due !== null &&
      recurrenceValid &&
      this.sources().length <= MAX_SOURCES
    );
  });

  constructor() {
    void this.installUpdateListeners();
    this.destroyRef.onDestroy(() => {
      this.destroyed = true;
      this.sourceUpdateUnlisten?.();
      this.sourceUpdateUnlisten = null;
      this.visibilityInvalidatedUnlisten?.();
      this.visibilityInvalidatedUnlisten = null;
    });

    effect(() => {
      const request = this.service.request();
      if (!request) {
        return;
      }
      const listenerState = this.listenerState();
      if (listenerState === "pending") {
        // Event registration has no replay. A request created in this interval
        // cannot be proven newer than a lock event that might have been missed.
        // Purge it immediately: a hung registration must not retain its title
        // indefinitely in the root composer-service signal.
        this.purgeInvalidatedRequest();
        return;
      }
      if (listenerState === "failed") {
        this.purgeInvalidatedRequest();
        return;
      }
      this.hydrate(request);
      afterNextRender(() => this.titleInput()?.nativeElement.focus(), {
        injector: this.injector,
      });
    });
  }

  private async installUpdateListeners(): Promise<void> {
    try {
      await Promise.all([
        this.installSourceUpdateListener(),
        this.installVisibilityInvalidatedListener(),
      ]);
      if (!this.destroyed) {
        // Listener registration has no replay. A request that already exists at
        // this boundary may have been created before a lock event that landed
        // while registration was pending. Purge synchronously before publishing
        // `ready`; no request/event task can interleave between these two lines.
        // The effect's pending-state purge remains a second line of defence, but
        // cannot be the authority because Angular schedules effects.
        this.purgeInvalidatedRequest();
        this.listenerState.set("ready");
      }
    } catch {
      if (!this.destroyed) {
        // A composer request must not retain or hydrate source-derived text
        // when this renderer cannot observe a later lock transition.
        this.listenerState.set("failed");
        this.purgeInvalidatedRequest();
      }
    }
  }

  private async installSourceUpdateListener(): Promise<void> {
    const unlisten = await this.ipc.onReminderSourceUpdated((payload) => {
      if (
        this.destroyed ||
        !payload ||
        (payload.kind !== "meeting" && payload.kind !== "note") ||
        typeof payload.id !== "string"
      ) {
        return;
      }
      const request = this.service.request();
      const requestSources =
        request?.mode === "edit"
          ? request.reminder.sources
          : request?.mode === "suggestion"
            ? [request.suggestion.source]
            : (request?.sources ?? []);
      if (
        !this.sources().some((source) =>
          matchesInvalidation(source, payload),
        ) &&
        !requestSources.some((source) => matchesInvalidation(source, payload))
      ) {
        return;
      }

      // The event is content-free but authoritative: close and purge
      // synchronously so neither an open modal nor retained component signals
      // can keep a sealed source title alive.
      this.purgeInvalidatedRequest();
    });
    if (this.destroyed) {
      unlisten();
      return;
    }
    this.sourceUpdateUnlisten = unlisten;
  }

  private purgeInvalidatedRequest(): void {
    this.closeCurrentRequest();
    this.title.set("");
    this.details.set("");
    this.date.set("");
    this.time.set("");
    this.repeats.set(false);
    this.repeatEvery.set(1);
    this.repeatUnit.set("weeks");
    this.sources.set([]);
    this.busy.set(false);
    this.error.set(null);
  }

  private async installVisibilityInvalidatedListener(): Promise<void> {
    const unlisten = await this.ipc.onReminderVisibilityInvalidated(() => {
      if (!this.destroyed) {
        this.purgeInvalidatedRequest();
      }
    });
    if (this.destroyed) {
      unlisten();
      return;
    }
    this.visibilityInvalidatedUnlisten = unlisten;
  }

  close(): void {
    if (!this.busy()) {
      this.closeCurrentRequest();
    }
  }

  rememberPointerTarget(event: MouseEvent): void {
    if (!this.service.request() && event.target instanceof HTMLElement) {
      this.lastPointerTarget = event.target;
    }
  }

  onDocumentKeydown(event: KeyboardEvent): void {
    if (!this.service.request() || this.listenerState() !== "ready") {
      return;
    }
    if (event.key === "Escape" && !event.defaultPrevented) {
      event.preventDefault();
      this.close();
      return;
    }
    if (event.key !== "Tab") {
      return;
    }

    const panel = this.panel()?.nativeElement;
    if (!panel) {
      return;
    }
    const focusable = [
      ...panel.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR),
      ...document.querySelectorAll<HTMLElement>(OVERLAY_FOCUSABLE_SELECTOR),
    ].filter((element) => element.getClientRects().length > 0);
    if (focusable.length === 0) {
      event.preventDefault();
      panel.focus();
      return;
    }

    const active = document.activeElement;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (!focusable.includes(active as HTMLElement)) {
      event.preventDefault();
      first.focus();
    } else if (event.shiftKey && active === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus();
    }
  }

  setTitle(value: string): void {
    this.title.set(value);
  }

  setDetails(value: string): void {
    this.details.set(value);
  }

  setDate(value: string): void {
    this.date.set(value);
  }

  setTime(value: string): void {
    this.time.set(value);
  }

  setRepeats(checked: boolean): void {
    this.repeats.set(checked);
  }

  setRepeatEvery(value: string): void {
    this.repeatEvery.set(Number(value));
  }

  setRepeatUnit(value: string): void {
    this.repeatUnit.set(value as ReminderRepeatUnit);
  }

  setSources(sources: SourceRef[]): void {
    this.sources.set(sources.slice(0, MAX_SOURCES));
  }

  async submit(event: Event): Promise<void> {
    event.preventDefault();
    const request = this.service.request();
    const draft = this.toDraft();
    if (!request || !draft || this.busy()) {
      return;
    }
    const submittedKey = request.key;
    this.busy.set(true);
    this.error.set(null);
    try {
      if (request.mode === "edit") {
        await this.store.update(request.reminder.id, draft);
      } else if (request.mode === "suggestion") {
        await this.store.acceptSuggestion(request.suggestion, draft);
      } else {
        await this.store.create(draft);
      }
      if (this.service.request()?.key !== submittedKey) {
        return;
      }
      this.busy.set(false);
      this.closeCurrentRequest();
    } catch {
      if (this.service.request()?.key === submittedKey) {
        this.error.set("Couldn’t save this reminder. Please try again.");
      }
    } finally {
      if (this.service.request()?.key === submittedKey) {
        this.busy.set(false);
      }
    }
  }

  private hydrate(request: ReminderComposerRequest): void {
    if (request.key !== this.hydratedRequestKey) {
      const active = document.activeElement;
      const candidate =
        active instanceof HTMLElement && active !== document.body
          ? active
          : this.lastPointerTarget;
      if (
        candidate &&
        candidate.isConnected &&
        !this.panel()?.nativeElement.contains(candidate)
      ) {
        this.restoreFocusTo = candidate;
      }
      this.lastPointerTarget = null;
      this.hydratedRequestKey = request.key;
    }
    const defaultDue = Date.now() + DEFAULT_DUE_OFFSET_MS;
    if (request.mode === "edit") {
      this.title.set(request.reminder.title);
      this.details.set(request.reminder.details ?? "");
      this.setDue(request.reminder.dueAt);
      this.repeats.set(request.reminder.repeatEvery !== null);
      this.repeatEvery.set(request.reminder.repeatEvery ?? 1);
      this.repeatUnit.set(request.reminder.repeatUnit ?? "weeks");
      this.sources.set(request.reminder.sources);
    } else if (request.mode === "suggestion") {
      this.title.set(request.suggestion.title);
      this.details.set("");
      if (request.suggestion.suggestedDueAt === null) {
        this.date.set("");
        this.time.set("");
      } else {
        this.setDue(request.suggestion.suggestedDueAt);
      }
      this.repeats.set(false);
      this.repeatEvery.set(1);
      this.repeatUnit.set("weeks");
      this.sources.set([request.suggestion.source]);
    } else {
      this.title.set(request.title);
      this.details.set("");
      this.setDue(request.dueAt ?? defaultDue);
      this.repeats.set(false);
      this.repeatEvery.set(1);
      this.repeatUnit.set("weeks");
      this.sources.set(request.sources);
    }
    this.error.set(null);
  }

  private closeCurrentRequest(): void {
    const restoreTarget = this.restoreFocusTo;
    this.service.close();
    this.hydratedRequestKey = null;
    this.restoreFocusTo = null;
    afterNextRender(
      () => {
        if (
          !this.service.request() &&
          restoreTarget?.isConnected &&
          !restoreTarget.hasAttribute("disabled")
        ) {
          restoreTarget.focus();
        }
      },
      { injector: this.injector },
    );
  }

  private setDue(epoch: number): void {
    const parts = localDateParts(epoch);
    this.date.set(parts.date);
    this.time.set(parts.time);
  }

  private dueEpoch(): number | null {
    if (!this.date() || !this.time()) {
      return null;
    }
    const value = new Date(`${this.date()}T${this.time()}`);
    const epoch = value.getTime();
    if (!Number.isFinite(epoch)) {
      return null;
    }
    const parts = localDateParts(epoch);
    return parts.date === this.date() && parts.time === this.time()
      ? epoch
      : null;
  }

  private toDraft(): ReminderDraft | null {
    const dueAt = this.dueEpoch();
    if (!this.valid() || dueAt === null) {
      return null;
    }
    return {
      title: this.title().trim(),
      details: this.details().trim() || null,
      dueAt,
      repeatEvery: this.repeats() ? this.repeatEvery() : null,
      repeatUnit: this.repeats() ? this.repeatUnit() : null,
      sources: this.sources().map((source) => ({
        kind: source.kind as SourceKind,
        id: source.id,
      })),
    };
  }
}
