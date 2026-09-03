import type { DestroyRef } from "@angular/core";
import type { UnlistenFn } from "@tauri-apps/api/event";

/**
 * Subscribe to an IPC event stream and release it on destroy — correctly across the `await`.
 *
 * WHY THIS EXISTS (2026-09-02 audit, L5). Every call site wrote the same shape by hand:
 *
 * ```ts
 * this.unlistenX = await this.ipc.onX(cb);
 * this.destroyRef.onDestroy(() => this.unlistenX?.());   // ← too late
 * ```
 *
 * and it is wrong twice over when the view is destroyed while that `await` is in flight. The
 * resumed continuation calls `DestroyRef.onDestroy` on a dead view, which throws **NG0911** ("View
 * has already been destroyed"); and because the registration never happened, the handle the await
 * just produced is never released — a live event subscription on a destroyed view, feeding
 * callbacks that write signals nobody renders.
 *
 * The order is the whole fix: register the cleanup FIRST, synchronously, then await. If destruction
 * wins the race, the flag is already set and the late handle is released immediately instead of
 * being stored on a corpse. This mirrors `record.component.ts`, which had to learn it the hard way
 * after an interval kept polling `detect_meeting_app` forever on a dead view.
 *
 * Returns the handle when the subscription is live, or `null` when the view was destroyed first —
 * so a caller that keeps its own field stores `null` rather than a handle it can never use.
 */
export async function subscribeUntilDestroyed(
  destroyRef: DestroyRef,
  subscribe: () => Promise<UnlistenFn>,
): Promise<UnlistenFn | null> {
  let destroyed = false;
  let unlisten: UnlistenFn | null = null;
  // Synchronous, before any await: this is the ordering the whole helper exists to guarantee.
  destroyRef.onDestroy(() => {
    destroyed = true;
    unlisten?.();
    unlisten = null;
  });
  const handle = await subscribe();
  if (destroyed) {
    handle();
    return null;
  }
  unlisten = handle;
  return handle;
}
