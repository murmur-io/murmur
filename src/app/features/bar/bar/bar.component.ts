import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  OnInit,
  computed,
  effect,
  inject,
  signal,
} from "@angular/core";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { RecorderStore } from "../../../core/recorder.store";
import { MicMuteToggleComponent } from "../../record/mic-mute-toggle/mic-mute-toggle.component";
import { flattenRecordingDestinations } from "../../record/recording-placement/recording-destinations";
import { WorkspaceService } from "../../workspace/workspace.service";

/**
 * The floating, always-on-top "OS bar" (a second Tauri window summoned with ⌘⇧R).
 * The window itself carries native macOS vibrancy (HudWindow) + a native rounded shadow,
 * so the pill is REAL frosted glass that blurs the desktop behind it. The document is
 * transparent; the CSS only adds a faint tint, border, and the content. Recording state
 * stays in sync with the main window via the backend's EVENT_STATUS broadcast.
 */
@Component({
  selector: "app-floating-bar",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MicMuteToggleComponent],
  host: { "(document:keydown.escape)": "hide()" },
  templateUrl: "./bar.component.html",
  styleUrl: "./bar.component.scss",
})
export class FloatingBarComponent implements OnInit {
  readonly store = inject(RecorderStore);
  private readonly workspace = inject(WorkspaceService);
  private readonly destroyRef = inject(DestroyRef);

  private focusUnlisten: UnlistenFn | null = null;
  private destroyed = false;

  /** Every floating-bar visit starts explicitly Unfiled; prior choices never preselect. */
  readonly selectedDestinationId = signal<string | null>(null);

  readonly destinations = computed(() =>
    flattenRecordingDestinations(this.workspace.forest()),
  );

  /** Refuse a stale selection if a lock/privacy refresh removes or blocks it. */
  readonly destinationReady = computed(() => {
    const selectedId = this.selectedDestinationId();
    return (
      selectedId === null ||
      this.destinations().some(
        (destination) =>
          destination.id === selectedId && !destination.blocked,
      )
    );
  });

  /** Forget an id the canonical privacy-gated forest no longer authorizes. */
  private readonly _resetUnavailableDestination = effect(() => {
    if (!this.destinationReady()) {
      this.selectedDestinationId.set(null);
    }
  });

  readonly bars = Array.from({ length: 30 }, (_, i) => i);
  /** Slimmed waveform shown beside the caption so both fit on one row. */
  readonly compactBars = Array.from({ length: 8 }, (_, i) => i);

  readonly isProcessing = computed(
    () => this.store.isBusy() && !this.store.isRecording(),
  );

  /**
   * The tail of the latest partial transcript — the most recent words, capped so
   * the buffer stays small. A leading ellipsis marks earlier text; CSS clips any
   * residual overflow on one line. Empty while idle/silent (caption then hidden).
   */
  readonly caption = computed(() => {
    const text = this.store.liveCaption().trim().replace(/\s+/g, " ");
    if (!text) return "";
    const tail = 72;
    return text.length > tail ? "…" + text.slice(text.length - tail) : text;
  });

  readonly elapsedLabel = computed(() => {
    const s = this.store.elapsed();
    const m = Math.floor(s / 60);
    return `${m}:${(s % 60).toString().padStart(2, "0")}`;
  });

  constructor() {
    // This window must be see-through so only the frosted pill shows. Force the document
    // transparent immediately (don't wait on the app-shell effect); `color-scheme: dark`
    // otherwise paints an opaque black canvas over the native vibrancy.
    document.documentElement.style.background = "transparent";
    document.documentElement.style.colorScheme = "normal";
    document.body.style.background = "transparent";
    document.body.classList.add("bar-shell");

    this.destroyRef.onDestroy(() => {
      this.destroyed = true;
      this.focusUnlisten?.();
      this.focusUnlisten = null;
    });
  }

  async ngOnInit(): Promise<void> {
    // Rust keeps this webview alive while the bar is hidden. Register the native
    // focus witness before doing longer store initialization, then load once for
    // the first render. Every native show path ends in `set_focus()`.
    void this.registerFocusLifecycle();
    void this.workspace.reload();
    await this.store.init();
  }

  private async registerFocusLifecycle(): Promise<void> {
    try {
      const unlisten = await getCurrentWindow().onFocusChanged(({ payload }) => {
        if (!payload) return;
        // A shown bar is a fresh choice, even though Angular itself was never
        // remounted. Reload is unconditional so create/rename/reparent/delete
        // operations performed in the main window cannot leave this picker stale.
        // WorkspaceService's generation guard drops any older response that races
        // this focus-triggered read.
        this.selectedDestinationId.set(null);
        void this.workspace.reload();
      });
      if (this.destroyed) {
        unlisten();
      } else {
        this.focusUnlisten = unlisten;
      }
    } catch {
      // A normal browser has no native window event surface. The initial reload
      // still keeps browser/dev rendering usable; Tauri grants this API to `bar`.
    }
  }

  selectDestination(event: Event): void {
    const id = (event.target as HTMLSelectElement).value;
    this.selectedDestinationId.set(id || null);
  }

  async startSelected(): Promise<void> {
    if (this.store.isBusy() || !this.destinationReady()) {
      return;
    }
    await this.store.start(this.selectedDestinationId());
    if (this.store.isRecording()) {
      this.selectedDestinationId.set(null);
    }
  }

  /** Dismiss the floating bar (Escape). */
  hide(): void {
    void getCurrentWindow().hide();
  }
}
