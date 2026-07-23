import { DestroyRef, Injectable, inject } from "@angular/core";

const STOP_FLUSH_DEADLINE_MS = 2_000;

/**
 * A flush provider — the embedded companion note editor registers one of these
 * while it is mounted during a recording. `flush()` MUST force any pending
 * (debounced) save to the backend NOW. The service waits for it only within a
 * bounded deadline; failure or a wedged provider must never block Stop. The boolean
 * result is a durability witness: only `true` allows the backend to delete an
 * apparently-empty companion stub.
 */
export type FlushProvider = () => Promise<boolean>;

/**
 * FLUSH-BEFORE-FINALIZE seam (root-cause fix, 2026-07-17).
 *
 * The recording panel's "Note" tab hosts the embedded {@link NoteEditorComponent},
 * which persists the meeting's companion note via a 600ms-DEBOUNCED autosave. The
 * recorder's Stop path (`RecorderStore.stop()`) calls `stop_recording` IMMEDIATELY,
 * and `stop_recording` deletes the companion note if it is still empty
 * (`delete_companion_note_if_empty`). So a user who types in the Note tab and clicks
 * Stop within the debounce window lost their prose: the debounced save hadn't landed,
 * the DB body was still the empty eager-created stub, the delete-if-empty fired, and
 * the late `save_note_text` then hit `no note …` — the text vanished from the note,
 * the vault, AND the summary.
 *
 * This service is the decoupling seam that lets `RecorderStore` (which lives in
 * `core/` and MUST NOT import a feature component — that would be an import cycle)
 * DURABLY flush the live companion editor's pending edits BEFORE it calls
 * `stop_recording`. The embedded editor {@link register}s its `flushPendingSave`
 * while mounted; `RecorderStore.stop()` awaits {@link flush} up front and passes its
 * boolean durability witness to Rust. The backend evaluates delete-if-empty only for
 * an explicit `true`; timeout/failure preserves the row for a possible late save.
 *
 * A root singleton so the ONE live editor instance and the recorder store share it.
 */
@Injectable({ providedIn: "root" })
export class RecordingFlushService {
  private readonly destroyRef = inject(DestroyRef);
  private readonly timers = new Set<ReturnType<typeof setTimeout>>();

  constructor() {
    this.destroyRef.onDestroy(() => {
      for (const timer of this.timers) clearTimeout(timer);
      this.timers.clear();
    });
  }
  /**
   * The currently-registered flush provider (the live embedded companion editor),
   * or null when no such editor is mounted. At most one is registered at a time —
   * the recording panel keeps a SINGLE embedded editor instance mounted for the
   * whole recording (it hides, not destroys, the Note tab), so there is exactly one
   * editor to flush at Stop and no in-flight destroy-flush ambiguity.
   */
  private provider: FlushProvider | null = null;

  /**
   * Register the live companion editor's flush. Returns an unregister function the
   * editor MUST call on teardown so a destroyed editor never gets flushed. Idempotent
   * on unregister — only clears the slot if it still holds THIS provider (so a
   * remount that registers a new provider before an old teardown runs is not
   * clobbered).
   */
  register(provider: FlushProvider): () => void {
    this.provider = provider;
    return () => {
      if (this.provider === provider) {
        this.provider = null;
      }
    };
  }

  /**
   * Ask the registered editor to durably flush, but wait at most two seconds. Returns
   * `true` only when the local webview has a registered provider and it explicitly
   * confirmed a durable save before the deadline. A fulfilled `false` is still a
   * failed witness (for example, the editor exhausted its bounded save retry). When
   * no provider is registered, this also returns `false`: another Murmur webview may
   * own the editor, so absence here is not a global durability proof.
   * Never rejects: failure, timeout, or a wedged provider returns `false`, causing
   * Stop to preserve the companion stub while a late save remains free to land safely.
   */
  async flush(): Promise<boolean> {
    const provider = this.provider;
    if (!provider) {
      return false;
    }
    let timer: ReturnType<typeof setTimeout> | undefined;
    try {
      const deadline = new Promise<boolean>((resolve) => {
        timer = setTimeout(() => resolve(false), STOP_FLUSH_DEADLINE_MS);
        this.timers.add(timer);
      });
      return await Promise.race([
        provider().then(
          (completed) => completed,
          () => false,
        ),
        deadline,
      ]);
    } catch {
      return false;
    } finally {
      if (timer !== undefined) {
        clearTimeout(timer);
        this.timers.delete(timer);
      }
    }
  }
}
