import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  computed,
  inject,
} from "@angular/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { RecorderStore } from "../../core/recorder.store";

/**
 * The floating, always-on-top "OS bar" (a second Tauri window summoned with ⌘⇧R).
 * The window itself carries native macOS vibrancy (HudWindow) + a native rounded shadow,
 * so the pill is REAL frosted glass that blurs the desktop behind it. The document is
 * transparent; the CSS only adds a faint tint, border, and the content. Recording state
 * stays in sync with the main window via the backend's EVENT_STATUS broadcast.
 */
@Component({
  selector: "app-floating-bar",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { "(document:keydown.escape)": "hide()" },
  template: `
    <div
      class="bar"
      [class.is-recording]="store.isRecording()"
      [class.is-processing]="isProcessing()"
    >
      @if (store.isRecording()) {
        <span class="orb live" data-tauri-drag-region aria-hidden="true"></span>
        <span class="timer" data-tauri-drag-region>{{ elapsedLabel() }}</span>
        <div class="wave" [style.--level]="store.level()" aria-hidden="true">
          @for (b of bars; track b) {
            <span class="wbar" [style.--i]="b"></span>
          }
        </div>
        <button
          type="button"
          class="circle stop"
          (click)="store.stop()"
          aria-label="Stop recording"
        >
          <span class="sq" aria-hidden="true"></span>
        </button>
      } @else if (isProcessing()) {
        <span class="orb proc" data-tauri-drag-region aria-hidden="true"></span>
        <span class="label proc-label" data-tauri-drag-region>{{
          store.message() || store.stage()
        }}</span>
        <div class="track" aria-hidden="true"><div class="shim"></div></div>
      } @else {
        <span class="grip" data-tauri-drag-region aria-hidden="true">
          <i></i><i></i>
        </span>
        <span
          class="orb ready"
          data-tauri-drag-region
          aria-hidden="true"
        ></span>
        <span class="label ready-label" data-tauri-drag-region>
          Ready to record
        </span>
        <span class="kbd" data-tauri-drag-region aria-hidden="true">⌘⇧R</span>
        <button
          type="button"
          class="circle rec"
          (click)="store.start()"
          [disabled]="store.isBusy()"
          aria-label="Start recording"
        >
          <span class="dot" aria-hidden="true"></span>
        </button>
      }
    </div>
  `,
  styles: [
    `
      :host {
        display: flex;
        align-items: center;
        justify-content: center;
        width: 100vw;
        height: 100vh;
        overflow: hidden;
        background: transparent;
      }

      /* The pill fills the window; native vibrancy provides the frost + shadow. */
      .bar {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        width: 100%;
        height: 100%;
        padding: 0 7px 0 var(--space-4);
        border-radius: 29px;
        border: 1px solid rgba(255, 255, 255, 0.12);
        overflow: hidden;
        /* Faint tint over the vibrancy for text contrast — NOT an opaque fill. */
        background: rgba(14, 14, 20, 0.2);
        box-shadow: var(--glass-highlight);
        animation: bar-pop 280ms var(--ease-spring) both;
        transition:
          border-color var(--transition),
          background var(--transition);
      }
      @keyframes bar-pop {
        from {
          opacity: 0;
          transform: translateY(-10px) scale(0.97);
        }
        to {
          opacity: 1;
          transform: translateY(0) scale(1);
        }
      }
      .bar.is-recording {
        border-color: rgba(255, 122, 92, 0.45);
        background: rgba(28, 14, 14, 0.22);
        box-shadow:
          var(--glass-highlight),
          inset 0 0 24px rgba(255, 122, 92, 0.1);
      }

      /* Drag handle */
      .grip {
        display: inline-flex;
        flex-direction: column;
        gap: 3px;
        padding: var(--space-2) 1px;
        cursor: grab;
      }
      .grip i {
        width: 3px;
        height: 3px;
        border-radius: 50%;
        background: var(--text-muted);
      }

      .label {
        flex: 1;
        min-width: 0;
        font-size: 0.95rem;
        font-weight: 550;
        letter-spacing: -0.01em;
        color: var(--text-primary);
        white-space: nowrap;
        overflow: hidden;
        text-overflow: ellipsis;
        cursor: default;
      }
      .ready-label {
        cursor: grab;
      }
      .proc-label {
        text-transform: capitalize;
      }

      .kbd {
        display: inline-flex;
        align-items: center;
        height: 24px;
        padding: 0 var(--space-2);
        border-radius: var(--radius-sm);
        background: rgba(255, 255, 255, 0.08);
        border: 1px solid var(--border);
        color: var(--text-secondary);
        font-family: var(--font-mono);
        font-size: 0.74rem;
        cursor: grab;
      }

      .timer {
        font-family: var(--font-mono);
        font-size: 0.95rem;
        font-weight: 500;
        font-variant-numeric: tabular-nums;
        letter-spacing: 0.02em;
        color: var(--text-primary);
        min-width: 48px;
        cursor: grab;
      }

      /* Waveform — warm, level-driven */
      .wave {
        flex: 1;
        display: flex;
        align-items: center;
        gap: 3px;
        height: 24px;
      }
      .wbar {
        flex: 1;
        min-width: 2px;
        max-width: 4px;
        height: 100%;
        border-radius: var(--radius-pill);
        background: var(--live-gradient);
        transform: scaleY(0.16);
        transform-origin: center;
        animation: bwave 1100ms ease-in-out infinite;
        animation-delay: calc(var(--i) * -70ms);
      }
      @keyframes bwave {
        0%,
        100% {
          transform: scaleY(calc(0.14 + var(--level, 0) * 0.55));
        }
        50% {
          transform: scaleY(calc(0.32 + var(--level, 0) * 1.15));
        }
      }

      /* Processing shimmer */
      .track {
        flex: none;
        width: 72px;
        height: 4px;
        border-radius: var(--radius-pill);
        background: rgba(255, 255, 255, 0.1);
        overflow: hidden;
      }
      .shim {
        height: 100%;
        width: 40%;
        border-radius: var(--radius-pill);
        background: var(--accent-gradient);
        animation: bshim 1.3s ease-in-out infinite;
      }
      @keyframes bshim {
        0% {
          transform: translateX(-120%);
        }
        100% {
          transform: translateX(320%);
        }
      }

      /* Circular action buttons (sized to the slimmer pill) */
      .circle {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 44px;
        height: 44px;
        min-width: 44px;
        border: none;
        border-radius: 50%;
        cursor: pointer;
        transition:
          transform var(--transition-fast),
          filter var(--transition),
          box-shadow var(--transition);
      }
      .circle:active {
        transform: scale(0.95);
      }
      .circle:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .circle.rec {
        background: var(--accent-gradient);
        box-shadow: 0 4px 16px rgba(110, 118, 255, 0.5);
      }
      .circle.rec:hover:not(:disabled) {
        transform: scale(1.06);
        filter: brightness(1.08);
      }
      .circle.rec:disabled {
        opacity: 0.4;
        cursor: not-allowed;
      }
      .circle.rec .dot {
        width: 15px;
        height: 15px;
        border-radius: 50%;
        background: #fff;
      }
      .circle.stop {
        background: var(--live-gradient);
        box-shadow: 0 4px 14px rgba(255, 94, 120, 0.45);
      }
      .circle.stop:hover {
        transform: scale(1.06);
        filter: brightness(1.08);
      }
      .circle.stop:focus-visible {
        box-shadow: 0 0 0 3px rgba(255, 122, 92, 0.6);
      }
      .circle.stop .sq {
        width: 14px;
        height: 14px;
        border-radius: 4px;
        background: #fff;
      }

      /* Status orbs */
      .orb {
        width: 12px;
        height: 12px;
        min-width: 12px;
        border-radius: 50%;
        position: relative;
      }
      .orb.ready {
        background: var(--accent);
        box-shadow: 0 0 12px rgba(110, 118, 255, 0.8);
      }
      .orb.ready::after {
        content: "";
        position: absolute;
        inset: -5px;
        border-radius: 50%;
        border: 1.5px solid var(--accent);
        opacity: 0.5;
        animation: borb 2.4s ease-in-out infinite;
      }
      @keyframes borb {
        0%,
        100% {
          transform: scale(1);
          opacity: 0.5;
        }
        50% {
          transform: scale(1.5);
          opacity: 0;
        }
      }
      .orb.live {
        background: var(--live);
        box-shadow: 0 0 14px rgba(255, 122, 92, 0.9);
        animation: blive 1.4s ease-in-out infinite;
      }
      @keyframes blive {
        0%,
        100% {
          opacity: 1;
          transform: scale(1);
        }
        50% {
          opacity: 0.55;
          transform: scale(0.82);
        }
      }
      .orb.proc {
        border: 2px solid rgba(255, 255, 255, 0.18);
        border-top-color: var(--accent);
        animation: bspin 0.8s linear infinite;
      }
      @keyframes bspin {
        to {
          transform: rotate(360deg);
        }
      }
    `,
  ],
})
export class FloatingBarComponent implements OnInit {
  readonly store = inject(RecorderStore);

  readonly bars = Array.from({ length: 30 }, (_, i) => i);

  readonly isProcessing = computed(
    () => this.store.isBusy() && !this.store.isRecording(),
  );

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
