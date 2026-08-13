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
import type { UnlistenFn } from "@tauri-apps/api/event";
import { IpcService } from "../../../core/ipc.service";
import type { ReminderSuggestionView, SourceKind } from "../../../core/models";
import { DebounceService } from "../../../services/debounce.service";
import { ReminderComposerService } from "../reminder-composer/reminder-composer.service";
import { RemindersStore } from "../reminders.store";

const MAX_VISIBLE_SUGGESTIONS = 3;
const REVISION_AUDIT_DEBOUNCE_MS = 850;
let nextSmartCardInstance = 0;
const SUGGESTION_DATE = new Intl.DateTimeFormat(undefined, {
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

interface SuggestionVm {
  suggestion: ReminderSuggestionView;
  dueLabel: string | null;
}

/**
 * Review-only contextual reminder surface. Auditing may stage suggestions but
 * never creates a reminder; every promotion opens the shared composer first.
 */
@Component({
  selector: "app-smart-reminder-card",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./smart-reminder-card.component.html",
  styleUrl: "./smart-reminder-card.component.scss",
})
export class SmartReminderCardComponent {
  private readonly ipc = inject(IpcService);
  private readonly composer = inject(ReminderComposerService);
  private readonly store = inject(RemindersStore);
  private readonly debounce = inject(DebounceService);
  private readonly destroyRef = inject(DestroyRef);

  readonly sourceKind = input.required<SourceKind>();
  readonly sourceId = input.required<string>();
  readonly sourceTitle = input("");
  /** Keys re-audit to the currently rendered canonical revision. */
  readonly sourceRevision = input<string | number | null>(null);
  /**
   * Whether this card owns the generic "New reminder" affordance. Meeting
   * detail promotes that action into its command bar and disables it here so
   * the idle strip cannot duplicate the primary action.
   */
  readonly showCreateAction = input(true);

  private readonly _suggestions = signal<ReminderSuggestionView[]>([]);
  readonly suggestions = this._suggestions.asReadonly();
  readonly loading = signal(false);
  readonly error = signal<string | null>(null);
  readonly busy = signal<ReadonlySet<string>>(new Set());
  readonly listenerReady = signal(false);
  private requestSequence = 0;
  private currentIdentity = "";
  private readonly debounceKey = `smart-reminder-audit-${++nextSmartCardInstance}`;
  private sourceUpdateUnlisten: UnlistenFn | null = null;
  private visibilityInvalidatedUnlisten: UnlistenFn | null = null;
  private destroyed = false;

  readonly rows = computed<SuggestionVm[]>(() =>
    this._suggestions()
      .slice(0, MAX_VISIBLE_SUGGESTIONS)
      .map((suggestion) => ({
        suggestion,
        dueLabel:
          suggestion.suggestedDueAt === null
            ? null
            : SUGGESTION_DATE.format(new Date(suggestion.suggestedDueAt)),
      })),
  );

  /**
   * Whether this surface has earned the full frosted card.
   *
   * Deliberately excludes BOTH `loading()` and `error()`. Now that this renders
   * ABOVE the document body, anything that inflates it after mount pushes the
   * user's text down mid-interaction — a real layout shift, not a cosmetic one.
   * An audit failure is the common async case (the command rejects a second
   * after paint), and a failure to fetch *suggestions* does not justify moving
   * the note under the caret. The error still renders, as one compact row
   * inside the strip.
   *
   * This is not hypothetical: growing on error broke
   * e2e/notes/link-picker.spec.ts on webkit, where the body shifted between the
   * slash menu opening and the "Link to note" click landing.
   */
  readonly hasContent = computed(() => this.rows().length > 0);

  constructor() {
    // The listener is established before the first audit is allowed to run.
    // Therefore a canonical write can never land in the mount→audit gap without
    // either being observed by this event or being included in the initial read.
    void this.installUpdateListeners();

    effect(() => {
      if (!this.listenerReady()) {
        return;
      }
      const kind = this.sourceKind();
      const id = this.sourceId();
      // These reads are dependencies only. Never cache the parent's title:
      // listener registration has no replay, so a lock event could have landed
      // before readiness. Only a post-registration gated audit may return a
      // title that is safe to put into the composer.
      this.sourceTitle();
      this.sourceRevision();
      this.store.revision();
      const identity = `${kind}:${id}`;

      if (identity !== this.currentIdentity) {
        this.currentIdentity = identity;
        this.invalidatePending();
        void this.audit(kind, id);
        return;
      }

      // The effect reruns only when one of the dependencies above changed.
      this.invalidatePending();
      this.scheduleAudit(kind, id);
    });
    this.destroyRef.onDestroy(() => {
      this.destroyed = true;
      this.sourceUpdateUnlisten?.();
      this.sourceUpdateUnlisten = null;
      this.visibilityInvalidatedUnlisten?.();
      this.visibilityInvalidatedUnlisten = null;
      this.invalidatePending();
    });
  }

  newReminder(): void {
    if (!this.listenerReady()) {
      return;
    }
    const suggestionSource = this._suggestions()[0]?.source;
    this.composer.openCreate({
      source: suggestionSource ?? {
        kind: this.sourceKind(),
        id: this.sourceId(),
        // The parent title is deliberately never trusted here. If the gated
        // audit produced no suggestion, keep only the opaque anchor; submit
        // re-gates it and the canonical list can resolve a visible title.
        title: "",
      },
    });
  }

  editAndCreate(suggestion: ReminderSuggestionView): void {
    this.composer.openSuggestion(suggestion);
  }

  async dismiss(suggestionId: string): Promise<void> {
    if (this.busy().has(suggestionId)) {
      return;
    }
    this.setBusy(suggestionId, true);
    this.error.set(null);
    try {
      await this.ipc.dismissReminderSuggestion(suggestionId);
      this._suggestions.update((rows) =>
        rows.filter((row) => row.id !== suggestionId),
      );
    } catch {
      this.error.set("Couldn’t dismiss this suggestion. It may have changed.");
    } finally {
      this.setBusy(suggestionId, false);
    }
  }

  retry(): void {
    if (!this.listenerReady()) {
      return;
    }
    void this.audit(this.sourceKind(), this.sourceId());
  }

  /**
   * Register directly on every mounted card, including a detached/open meeting
   * tab. Failure is browser-mock-safe: it cannot block the initial gated audit.
   */
  private async installUpdateListeners(): Promise<void> {
    try {
      await Promise.all([
        this.installSourceUpdateListener(),
        this.installVisibilityInvalidatedListener(),
      ]);
      if (!this.destroyed) {
        this.listenerReady.set(true);
      }
    } catch {
      if (!this.destroyed) {
        this.invalidatePending();
        this.error.set(
          "Smart reminder suggestions aren’t available securely right now.",
        );
      }
    }
  }

  private async installSourceUpdateListener(): Promise<void> {
    const unlisten = await this.ipc.onReminderSourceUpdated((payload) => {
      const kind = this.sourceKind();
      const id = this.sourceId();
      if (
        this.destroyed ||
        !payload ||
        !matchesSourceKind(payload.kind) ||
        payload.kind !== kind ||
        payload.id !== id
      ) {
        return;
      }
      this.invalidatePending();
      this.scheduleAudit(kind, id);
    });
    if (this.destroyed) {
      unlisten();
      return;
    }
    this.sourceUpdateUnlisten = unlisten;
  }

  private async installVisibilityInvalidatedListener(): Promise<void> {
    const unlisten = await this.ipc.onReminderVisibilityInvalidated(() => {
      if (this.destroyed) {
        return;
      }
      const kind = this.sourceKind();
      const id = this.sourceId();
      this.invalidatePending();
      this.scheduleAudit(kind, id);
    });
    if (this.destroyed) {
      unlisten();
      return;
    }
    this.visibilityInvalidatedUnlisten = unlisten;
  }

  private scheduleAudit(kind: SourceKind, id: string): void {
    if (this.destroyed) {
      return;
    }
    this.debounce.schedule(
      this.debounceKey,
      () => void this.audit(kind, id),
      REVISION_AUDIT_DEBOUNCE_MS,
    );
  }

  private async audit(kind: SourceKind, id: string): Promise<void> {
    if (!this.listenerReady()) {
      return;
    }
    const sequence = ++this.requestSequence;
    this.loading.set(true);
    this.error.set(null);
    try {
      const rows = await this.ipc.auditReminderSuggestions({ kind, id });
      if (sequence === this.requestSequence) {
        this._suggestions.set(rows.slice(0, MAX_VISIBLE_SUGGESTIONS));
      }
    } catch {
      if (sequence === this.requestSequence) {
        this._suggestions.set([]);
        this.error.set(
          "Smart reminder suggestions aren’t available right now.",
        );
      }
    } finally {
      if (sequence === this.requestSequence) {
        this.loading.set(false);
      }
    }
  }

  /** Immediately drops source-derived rows and makes every in-flight reply stale. */
  private invalidatePending(): void {
    this.debounce.cancel(this.debounceKey);
    this.requestSequence += 1;
    this._suggestions.set([]);
    this.loading.set(false);
    this.error.set(null);
  }

  private setBusy(id: string, value: boolean): void {
    this.busy.update((current) => {
      const next = new Set(current);
      if (value) {
        next.add(id);
      } else {
        next.delete(id);
      }
      return next;
    });
  }
}

function matchesSourceKind(value: unknown): value is SourceKind {
  return value === "meeting" || value === "note";
}
