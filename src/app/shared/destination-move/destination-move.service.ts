import { Injectable, signal } from "@angular/core";

import type { DestinationPickerAnchorKind } from "../../core/models";

export type DestinationMoveKind =
  | "meeting"
  | "note"
  | "document"
  | "task"
  | "dashboard"
  | "container"
  | "shared";

export interface DestinationMoveRequest {
  readonly kind: DestinationMoveKind;
  readonly id: string;
  readonly title: string;
  readonly currentContainerId?: string | null;
  readonly actionLabel?: "Move" | "Add a copy" | "Keep";
  readonly anchorKind?: DestinationPickerAnchorKind;
  readonly orgId?: string;
  readonly sharedTargetKind?: "container" | "doc";
  readonly execute?: (
    containerId: string | null,
    confirmedEncryptionBoundary: boolean,
  ) => void | Promise<void>;
  readonly afterMove?: () => void | Promise<void>;
}

export interface DestinationMoveResult {
  readonly moved: boolean;
  readonly containerId?: string | null;
}

interface ActiveMove {
  readonly request: DestinationMoveRequest;
  readonly trigger: HTMLElement | null;
  readonly resolve: (result: DestinationMoveResult) => void;
}

/** Serializes every Move affordance through the one global hierarchy picker. */
@Injectable({ providedIn: "root" })
export class DestinationMoveService {
  private readonly _active = signal<ActiveMove | null>(null);
  readonly active = this._active.asReadonly();
  private detached: Pick<ActiveMove, "resolve"> | null = null;

  open(request: DestinationMoveRequest, event?: Event): Promise<DestinationMoveResult> {
    if (this._active() || this.detached) return Promise.resolve({ moved: false });
    const clicked = event?.currentTarget instanceof HTMLElement ? event.currentTarget :
      document.activeElement instanceof HTMLElement
        ? document.activeElement
        : null;
    const menu = clicked?.closest<HTMLElement>('[role="menu"]');
    const owner = menu?.id
      ? document.querySelector<HTMLElement>(`[aria-controls="${CSS.escape(menu.id)}"]`)
      : null;
    const trigger = owner ?? menu?.parentElement?.querySelector<HTMLElement>('[aria-haspopup="menu"]')
      ?? clicked?.closest('details')?.querySelector<HTMLElement>('summary')
      ?? clicked;
    return new Promise((resolve) => {
      this._active.set({ request, trigger, resolve });
    });
  }

  /** Hide private UI without pretending that an in-flight write was cancelled. */
  detach(): void {
    const active = this._active();
    if (active) this.detached = { resolve: active.resolve };
    this._active.set(null);
  }

  finish(result: DestinationMoveResult): void {
    const active = this._active();
    if (!active) {
      this.detached?.resolve(result);
      this.detached = null;
      return;
    }
    this._active.set(null);
    active.resolve(result);
    active.trigger?.focus({ preventScroll: true });
  }
}
