import { DestroyRef, Injectable, inject } from "@angular/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { MeetingConversationStore } from "../core/meeting-conversation.store";
import { FoldersService } from "./folders.service";
import { ToastService } from "./toast.service";

/** Backend event fired the instant a screen share begins (privacy panic signal). */
export const EVENT_SCREEN_SHARE_STARTED = "murmur://screen-share-started";
export const EVENT_SCREEN_SHARE_RELOCK_FAILED =
  "murmur://screen-share-relock-failed";

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
  private readonly conversation = inject(MeetingConversationStore);
  private readonly destroyRef = inject(DestroyRef);

  /** Live event-listener handles (empty when not yet initialised / torn down). */
  private unlisten: UnlistenFn[] = [];
  /** Guards against overlapping init() calls before the listen() resolves. */
  private initializing = false;

  constructor() {
    this.destroyRef.onDestroy(() => {
      for (const unlisten of this.unlisten) unlisten();
      this.unlisten = [];
    });
  }

  /**
   * Begin listening for the screen-share signal. Call once at app start (e.g.
   * from the root component). Re-entrant-safe: a second call while a listener is
   * live (or being registered) does nothing.
   */
  async init(): Promise<void> {
    if (this.unlisten.length > 0 || this.initializing) {
      return;
    }
    this.initializing = true;
    try {
      const success = await listen(EVENT_SCREEN_SHARE_STARTED, () => {
        void this.onScreenShareStarted();
      });
      try {
        const failure = await listen(EVENT_SCREEN_SHARE_RELOCK_FAILED, () => {
          void this.onScreenShareRelockFailed();
        });
        this.unlisten = [success, failure];
      } catch (error) {
        success();
        throw error;
      }
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
    // Drop the proactive recall card FIRST (synchronously): its title can come
    // from a meeting the backend just re-sealed, and the record screen is
    // exactly the surface now being shared. Not a dismissal — the backend
    // re-gates visibility, so the hint may legitimately resurface later.
    this.conversation.clearHint();
    await this.folders.load();
    this.toast.info("Locked your private folders — screen sharing started");
  }

  /**
   * Physical cleanup hit a loss-safety conflict after the backend had already revoked every gated
   * read and hidden Murmur's main window. Keep the UI cache empty and report an explicit alarm —
   * never reuse the normal success copy for a partially secured vault.
   */
  private async onScreenShareRelockFailed(): Promise<void> {
    this.conversation.clearHint();
    await this.folders.load();
    this.toast.danger(
      "Could not secure every vault export — Murmur was hidden. Stop screen sharing and resolve the edited file.",
      0,
    );
  }
}
