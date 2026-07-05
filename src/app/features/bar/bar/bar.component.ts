import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  computed,
  inject,
} from "@angular/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { RecorderStore } from "../../../core/recorder.store";
import { MicMuteToggleComponent } from "../../record/mic-mute-toggle/mic-mute-toggle.component";

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
  }

  async ngOnInit(): Promise<void> {
    await this.store.init();
  }

  /** Dismiss the floating bar (Escape). */
  hide(): void {
    void getCurrentWindow().hide();
  }
}
