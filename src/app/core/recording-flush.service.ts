import { Injectable } from "@angular/core";

/**
 * A flush provider — the embedded companion note editor registers one of these
 * while it is mounted during a recording. `flush()` MUST force any pending
 * (debounced) save to the backend NOW and resolve only once that DB write has
 * completed. It must never reject (the recorder finalize path awaits it and a
 * flush failure must not block Stop).
 */
export type FlushProvider = () => Promise<void>;

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
 * while mounted; `RecorderStore.stop()` awaits {@link flush} up front. The delete-if-
 * empty predicate is left intact (it is correct) — the flush makes the DB current
 * before emptiness is ever evaluated.
 *
 * A root singleton so the ONE live editor instance and the recorder store share it.
 */
@Injectable({ providedIn: "root" })
export class RecordingFlushService {
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
   * DURABLY flush the registered companion editor's pending edits and resolve once
   * the DB write has completed. A no-op (resolves immediately) when no editor is
   * registered. Never rejects: a flush failure is swallowed here so the recorder's
   * Stop path can always proceed to `stop_recording` — the editor's own save chain
   * has already surfaced any real error to the user, and blocking Stop would be worse
   * than a best-effort flush.
   */
  async flush(): Promise<void> {
    const provider = this.provider;
    if (!provider) {
      return;
    }
    try {
      await provider();
    } catch {
      // Never block Stop on a flush failure — the editor's save chain owns error
      // surfacing; this is a best-effort last write before finalize.
    }
  }
}
