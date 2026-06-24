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
 * Frameless + transparent; reuses RecorderStore, so recording state stays in sync with
 * the main window through the backend's EVENT_STATUS broadcast.
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
        <span class="label" data-tauri-drag-region>{{
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
        padding: var(--space-2) var(--space-3);
      }

      .bar {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        width: 100%;
        height: 84px;
        padding: 0 var(--space-2) 0 var(--space-4);
        border-radius: var(--radius-pill);
        border: 1px solid var(--glass-border);
        background: rgba(20, 20, 28, 0.55);
        -webkit-backdrop-filter: blur(34px) saturate(150%);
        backdrop-filter: blur(34px) saturate(150%);
        box-shadow:
          0 24px 70px rgba(0, 0, 0, 0.6),
          var(--glass-highlight);
        animation: bar-pop 320ms var(--ease-spring) both;
        transition:
          border-color var(--transition),
          box-shadow var(--transition);
      }
      @keyframes bar-pop {
        from {
          opacity: 0;
          transform: translateY(-12px) scale(0.96);
        }
        to {
          opacity: 1;
          transform: translateY(0) scale(1);
        }
      }
      .bar.is-recording {
        border-color: rgba(255, 122, 92, 0.42);
        box-shadow:
          var(--live-glow),
          0 24px 70px rgba(0, 0, 0, 0.6),
          var(--glass-highlight);
      }

      /* Drag handle (two dots) */
      .grip {
        display: inline-flex;
        flex-direction: column;
        gap: 4px;
        padding: var(--space-2) 2px;
        cursor: grab;
      }
      .grip i {
        width: 4px;
        height: 4px;
        border-radius: 50%;
        background: var(--text-muted);
      }

      .label {
        flex: 1;
        font-size: 1rem;
        font-weight: 550;
        letter-spacing: -0.01em;
        color: var(--text-primary);
        cursor: default;
      }
      .label.ready-label {
        cursor: grab;
      }
      .is-processing .label {
        flex: none;
        text-transform: capitalize;
      }

      .kbd {
        display: inline-flex;
        align-items: center;
        height: 26px;
        padding: 0 var(--space-2);
        border-radius: var(--radius-sm);
        background: rgba(255, 255, 255, 0.07);
        border: 1px solid var(--border);
        color: var(--text-secondary);
        font-family: var(--font-mono);
        font-size: 0.78rem;
        cursor: grab;
      }

      .timer {
        font-family: var(--font-mono);
        font-size: 1rem;
        font-weight: 500;
        font-variant-numeric: tabular-nums;
        letter-spacing: 0.02em;
        color: var(--text-primary);
        min-width: 52px;
        cursor: grab;
      }

      /* Waveform — warm, level-driven */
      .wave {
        flex: 1;
        display: flex;
        align-items: center;
        gap: 3px;
        height: 34px;
      }
      .wbar {
        flex: 1;
        min-width: 2px;
        max-width: 5px;
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
        flex: 1;
        height: 4px;
        border-radius: var(--radius-pill);
        background: rgba(255, 255, 255, 0.08);
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

      /* Circular action buttons */
      .circle {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 60px;
        height: 60px;
        min-width: 60px;
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
        box-shadow: var(--shadow-accent);
      }
      .circle.rec:hover:not(:disabled) {
        transform: scale(1.05);
        filter: brightness(1.08);
      }
      .circle.rec:disabled {
        opacity: 0.4;
        cursor: not-allowed;
      }
      .circle.rec .dot {
        width: 18px;
        height: 18px;
        border-radius: 50%;
        background: #fff;
      }
      .circle.stop {
        background: var(--live-gradient);
        box-shadow: 0 8px 24px rgba(255, 94, 120, 0.45);
      }
      .circle.stop:hover {
        transform: scale(1.05);
        filter: brightness(1.08);
      }
      .circle.stop:focus-visible {
        box-shadow: 0 0 0 3px rgba(255, 122, 92, 0.6);
      }
      .circle.stop .sq {
        width: 17px;
        height: 17px;
        border-radius: 5px;
        background: #fff;
      }

      /* Status orbs */
      .orb {
        width: 13px;
        height: 13px;
        min-width: 13px;
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

  readonly bars = Array.from({ length: 32 }, (_, i) => i);

  readonly isProcessing = computed(
    () => this.store.isBusy() && !this.store.isRecording(),
  );

  readonly elapsedLabel = computed(() => {
    const s = this.store.elapsed();
    const m = Math.floor(s / 60);
    return `${m}:${(s % 60).toString().padStart(2, "0")}`;
  });

  async ngOnInit(): Promise<void> {
    await this.store.init();
  }

  /** Dismiss the floating bar (Escape). */
  hide(): void {
    void getCurrentWindow().hide();
  }
}
