import { Injectable, signal } from "@angular/core";
import type {
  ReminderSourceView,
  ReminderSuggestionView,
  ReminderView,
  SourceRef,
} from "../../../core/models";

export type ReminderComposerRequest =
  | {
      key: number;
      mode: "create";
      title: string;
      dueAt: number | null;
      sources: SourceRef[];
    }
  | {
      key: number;
      mode: "edit";
      reminder: ReminderView;
    }
  | {
      key: number;
      mode: "suggestion";
      suggestion: ReminderSuggestionView;
    };

/** One root-provided request channel for the app-wide reminder composer. */
@Injectable({ providedIn: "root" })
export class ReminderComposerService {
  private readonly _request = signal<ReminderComposerRequest | null>(null);
  readonly request = this._request.asReadonly();
  private readonly _listenerState = signal<"pending" | "ready" | "failed">(
    "pending",
  );
  readonly listenerState = this._listenerState.asReadonly();
  private readonly _privacyEpoch = signal(0);
  readonly privacyEpoch = this._privacyEpoch.asReadonly();
  private nextKey = 0;

  markListenersReady(): void {
    this._listenerState.set("ready");
  }

  markListenersFailed(): void {
    this._listenerState.set("failed");
  }

  markVisibilityInvalidated(): void {
    this._privacyEpoch.update((epoch) => epoch + 1);
  }

  openCreate(options?: {
    title?: string;
    dueAt?: number | null;
    source?: ReminderSourceView | SourceRef;
  }): void {
    const source = options?.source;
    this._request.set({
      key: ++this.nextKey,
      mode: "create",
      title: options?.title ?? "",
      dueAt: options?.dueAt ?? null,
      sources: source ? [source] : [],
    });
  }

  openEdit(reminder: ReminderView): void {
    this._request.set({
      key: ++this.nextKey,
      mode: "edit",
      reminder,
    });
  }

  openSuggestion(suggestion: ReminderSuggestionView): void {
    this._request.set({
      key: ++this.nextKey,
      mode: "suggestion",
      suggestion,
    });
  }

  close(): void {
    this._request.set(null);
  }
}
