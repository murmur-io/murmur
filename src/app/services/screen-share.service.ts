import { DestroyRef, Injectable, inject } from "@angular/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { FoldersService } from "./folders.service";
import { ToastService } from "./toast.service";

/** Backend event fired the instant a screen share begins (privacy panic signal). */
export const EVENT_SCREEN_SHARE_STARTED = "murmur://screen-share-started";

/**
 * Privacy guard: when the OS begins a screen share, the backend has ALREADY
 * re-sealed every session-unlocked folder (zeroizing the cached key). This
 * service reacts on the front end — it reloads the folder tree so the UI drops
 * back to the locked state the disk now reflects, and surfaces a calm toast so
 * the user understands why their private folders just re-locked.
 *
 * Init-once: `init()` is idempotent (a second call is a no-op while a listener
 * is live). The Tauri unlisten handle is torn down via `DestroyRef.onDestroy`.
 */
@Injectable({ providedIn: "root" })
export class ScreenShareService {
  private readonly folders = inject(FoldersService);
  private readonly toast = inject(ToastService);
  private readonly destroyRef = inject(DestroyRef);

  /** Live event-listener handle (null when not yet initialised / torn down). */
  private unlisten: UnlistenFn | null = null;
  /** Guards against overlapping init() calls before the listen() resolves. */
  private initializing = false;

  constructor() {
    this.destroyRef.onDestroy(() => {
      this.unlisten?.();
      this.unlisten = null;
    });
  }

  /**
   * Begin listening for the screen-share signal. Call once at app start (e.g.
   * from the root component). Re-entrant-safe: a second call while a listener is
   * live (or being registered) does nothing.
   */
  async init(): Promise<void> {
    if (this.unlisten || this.initializing) {
      return;
    }
    this.initializing = true;
    try {
      this.unlisten = await listen(EVENT_SCREEN_SHARE_STARTED, () => {
        void this.onScreenShareStarted();
      });
    } finally {
      this.initializing = false;
    }
  }

  /**
   * React to a screen share starting: the backend already relocked, so we just
   * resync the tree from disk and tell the user. The toast is informational; the
   * security action happened in the backend before this event fired.
   */
  private async onScreenShareStarted(): Promise<void> {
    await this.folders.load();
    this.toast.info("Locked your private folders — screen sharing started");
  }
}
