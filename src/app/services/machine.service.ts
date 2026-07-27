import { Injectable, computed, inject, signal } from "@angular/core";
import { IpcService } from "../core/ipc.service";
import type {
  MachineChangeNudge,
  WhisperModelDto,
  WhisperRecommendationDto,
} from "../core/models";
import { ErrorCopyService } from "../core/copy/error-copy.service";

/**
 * Root-held answer to "what does THIS Mac deserve?" — the machine profile, the
 * whisper catalog, and both recommendation answers, from the single
 * `whisper_recommendation` command.
 *
 * Root-scoped on purpose, for the reason in `angular-zoneless.md` §9: two hosts
 * consume it (the onboarding model step and Settings → Transcription), and both
 * are destroyed on navigate-away. A component-local signal would be wiped to
 * empty on every remount, so the model picker would flash blank before its
 * refetch resolved. A root instance outlives both, so the picker paints its
 * last-known catalog instantly while the (still unconditional) refresh replaces
 * it underneath.
 *
 * Deliberately a THIN holder: it fetches and caches, but never decides. The
 * ladder rungs, the copy for each `reason` variant and the recommendation
 * itself are all authored in Rust, so the frontend cannot drift from the branch
 * that produced them.
 */
@Injectable({ providedIn: "root" })
export class MachineService {
  private readonly ipc = inject(IpcService);
  private readonly errorCopy = inject(ErrorCopyService);

  private readonly _data = signal<WhisperRecommendationDto | null>(null);
  private readonly _loading = signal(false);
  private readonly _error = signal<string | null>(null);
  private readonly _nudge = signal<MachineChangeNudge | null>(null);
  private readonly _nudgeDismissed = signal(false);

  /** The whole backend answer, or `null` until the first refresh resolves. */
  readonly data = this._data.asReadonly();
  readonly loading = this._loading.asReadonly();
  readonly error = this._error.asReadonly();
  /** Whether the machine-change notice was dismissed in this session. */
  readonly nudgeDismissed = this._nudgeDismissed.asReadonly();

  /** The visible catalog, ascending by cost. Empty until the first refresh. */
  readonly models = computed<WhisperModelDto[]>(
    () => this._data()?.models ?? [],
  );

  /** The size that will actually load right now. */
  readonly selectedId = computed(() => this._data()?.selectedId ?? "");

  /** The honest hardware answer — what this Mac deserves, blind to the disk. */
  readonly recommendedId = computed(() => this._data()?.recommendedId ?? "");

  /**
   * Whether the current selection IS the hardware recommendation. Compared
   * against `recommendedId` (not `autoDefaultId`) because that is the claim the
   * "Recommended for this Mac" affordance makes.
   */
  readonly selectionIsRecommended = computed(() => {
    const d = this._data();
    return d != null && d.selectedId === d.recommendedId;
  });

  /**
   * The pending machine-change notice, or `null`. Suppressed for the rest of
   * the session once dismissed, so a refresh racing the dismiss cannot make it
   * reappear.
   */
  readonly machineChange = computed(() =>
    this._nudgeDismissed() ? null : this._nudge(),
  );

  /**
   * Monotonic token guarding against out-of-order responses. Concurrent
   * refreshes are the DESIGNED usage, not an edge case — this service is root
   * scoped and shared by two hosts that each refresh on mount, while a download
   * completing or a model being deleted fires another one. Without the token a
   * slower earlier response lands last and resurrects a stale snapshot: delete a
   * model, and an in-flight mount refresh writes back `downloaded: true` plus a
   * `pendingDownloadBytes` of 0 for the file that was just removed. Mirrors
   * `OrgBrainService.loadSeq`.
   */
  private loadSeq = 0;

  /**
   * Re-read everything from the backend. Call it on mount and after any action
   * that changes what is on disk (a download completing, a model deleted) —
   * otherwise the `downloaded` flags go stale immediately after the one action
   * the screen exists to perform.
   */
  async refresh(): Promise<void> {
    const seq = ++this.loadSeq;
    this._loading.set(true);
    try {
      const data = await this.ipc.whisperRecommendation();
      if (seq !== this.loadSeq) return;
      this._data.set(data);
      this._error.set(null);
    } catch (e) {
      if (seq !== this.loadSeq) return;
      // Keep the last-known catalog on screen rather than blanking it: a failed
      // refresh should degrade to stale-but-useful, never to empty.
      this._error.set(this.errorCopy.humanize(e));
    } finally {
      // `_loading` is gated too: a superseded call clearing it would report
      // "settled" while the newest request is still in flight.
      if (seq === this.loadSeq) this._loading.set(false);
    }
  }

  /**
   * Pull the one-shot machine-change notice. Deliberately a pull: the backend
   * records it in a settings row during `setup`, where an emitted event would
   * be lost because the webview has not called `listen()` yet.
   */
  async refreshMachineChange(): Promise<void> {
    try {
      this._nudge.set(await this.ipc.machineChangeNudge());
    } catch {
      // A nudge is advisory. Failing to read it must never surface an error.
      this._nudge.set(null);
    }
  }

  /** Dismiss the machine-change notice for good. */
  async dismissMachineChange(): Promise<void> {
    this._nudgeDismissed.set(true);
    try {
      await this.ipc.dismissMachineChangeNudge();
    } catch {
      // The local suppression above already hid it for this session; a failed
      // persist means it may return next launch, which is the safe direction.
    }
  }
}
