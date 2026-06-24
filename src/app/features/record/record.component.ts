import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  computed,
  inject,
  signal,
} from "@angular/core";
import { RecorderStore } from "../../core/recorder.store";
import { IpcService } from "../../core/ipc.service";
import type { AppConfigDto } from "../../core/models";

@Component({
  selector: "app-record",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { "(document:keydown)": "onKey($event)" },
  template: `
    <section class="record">
      @if (vaultMissing()) {
        <div class="banner is-warning" role="alert">
          <span class="banner-icon" aria-hidden="true">!</span>
          <span>
            Set your Obsidian <strong>vault folder</strong> in Settings before
            recording — notes can't be saved without it.
          </span>
        </div>
      } @else if (modelPresent() === false) {
        <div class="banner is-accent model-banner" role="alert">
          <span class="banner-icon" aria-hidden="true">↓</span>
          <div class="model-banner-body">
            <p class="model-banner-title">Transcription model needed</p>
            <p class="model-banner-text">
              Whisper runs on-device. Download it once to enable recording.
            </p>
            @if (modelDownloadError(); as derr) {
              <p class="model-banner-error">{{ derr }}</p>
            }
            <button
              type="button"
              class="btn btn-primary"
              (click)="downloadModel()"
              [disabled]="downloadingModel()"
            >
              {{ downloadingModel() ? "Downloading…" : "Download model" }}
            </button>
          </div>
        </div>
      }

      <!-- ── The morphing recording bar (the hero) ───────────────────────── -->
      <div class="stage" [class.live]="store.isRecording()">
        @if (store.isRecording()) {
          <div class="rec-bar is-recording" role="status">
            <span class="orb live" aria-hidden="true"></span>
            <span class="timer">{{ elapsedLabel() }}</span>
            <div
              class="wave"
              [style.--level]="store.level()"
              aria-hidden="true"
            >
              @for (b of bars; track b) {
                <span class="wbar" [style.--i]="b"></span>
              }
            </div>
            <button
              type="button"
              class="stop-btn"
              (click)="store.stop()"
              aria-label="Stop recording"
            >
              <span class="stop-ico" aria-hidden="true"></span>
            </button>
          </div>
        } @else if (isProcessing()) {
          <div class="rec-bar is-processing" role="status">
            <span class="orb proc" aria-hidden="true"></span>
            <span class="proc-label">{{
              store.message() || store.stage()
            }}</span>
            <div class="proc-track" aria-hidden="true">
              <div class="proc-shimmer"></div>
            </div>
          </div>
        } @else {
          <button
            type="button"
            class="rec-bar is-ready"
            (click)="store.start()"
            [disabled]="!canRecord()"
          >
            <span class="orb ready" aria-hidden="true"></span>
            <span class="ready-text">
              {{
                store.stage() === "done" ? "Record again" : "Ready to record"
              }}
            </span>
            <span class="kbd" aria-hidden="true">⌘R</span>
          </button>
        }

        <p class="stage-hint">{{ hint() }}</p>
      </div>

      @if (store.error(); as err) {
        <div class="banner is-danger" role="alert">
          <span class="banner-icon" aria-hidden="true">!</span>
          <span>{{ err }}</span>
        </div>
      }

      <!-- ── Last note ───────────────────────────────────────────────────── -->
      <div class="last-note">
        <h3>Last note</h3>
        @if (store.lastNote(); as note) {
          <div class="card note-card">
            @if (note.exportedPath) {
              <p class="path">{{ note.exportedPath }}</p>
            }
            <pre class="preview">{{ note.markdown }}</pre>
          </div>
        } @else {
          <div class="card empty-card">
            <span class="empty-mark" aria-hidden="true"></span>
            <p class="empty">
              No note yet — press <span class="kbd-inline">⌘R</span> or hit the
              bar to capture your first meeting.
            </p>
          </div>
        }
      </div>
    </section>
  `,
  styles: [
    `
      .record {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
        animation: rise 420ms var(--transition) both;
      }

      /* --- Model-download banner --- */
      .banner-icon {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 24px;
        height: 24px;
        min-width: 24px;
        border-radius: 50%;
        background: rgba(255, 255, 255, 0.08);
        font-weight: 700;
        font-size: 0.85rem;
        line-height: 1;
      }
      .model-banner-body {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .model-banner-title {
        margin: 0;
        font-weight: 600;
        color: var(--text-primary);
      }
      .model-banner-text {
        margin: 0;
        color: var(--text-secondary);
        font-size: 0.875rem;
      }
      .model-banner-error {
        margin: 0;
        color: var(--danger);
        font-size: 0.85rem;
      }
      .model-banner .btn {
        align-self: flex-start;
        margin-top: var(--space-1);
      }

      /* ── Stage: the hero area with an atmospheric glow ─────────────────── */
      .stage {
        position: relative;
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: var(--space-4);
        padding: var(--space-8) var(--space-4) var(--space-7);
      }
      .stage::before {
        content: "";
        position: absolute;
        top: 8%;
        left: 50%;
        width: 460px;
        height: 260px;
        transform: translateX(-50%);
        border-radius: 50%;
        background: radial-gradient(
          closest-side,
          rgba(110, 118, 255, 0.28),
          transparent 72%
        );
        filter: blur(28px);
        opacity: 0.7;
        transition:
          background var(--transition),
          opacity var(--transition);
        pointer-events: none;
      }
      /* When recording, the glow warms + intensifies. */
      .stage.live::before {
        background: radial-gradient(
          closest-side,
          rgba(255, 110, 100, 0.32),
          transparent 72%
        );
        opacity: 0.95;
      }

      /* ── The capsule, shared across states (each state swaps content) ──── */
      .rec-bar {
        position: relative;
        display: flex;
        align-items: center;
        gap: var(--space-3);
        height: 72px;
        padding: 0 var(--space-3) 0 var(--space-5);
        border-radius: var(--radius-pill);
        border: 1px solid var(--glass-border);
        background: rgba(255, 255, 255, 0.05);
        -webkit-backdrop-filter: blur(var(--glass-blur))
          saturate(var(--glass-saturate));
        backdrop-filter: blur(var(--glass-blur)) saturate(var(--glass-saturate));
        box-shadow: var(--shadow-lg), var(--glass-highlight);
        animation: bar-in 360ms var(--ease-spring) both;
        transition:
          width var(--transition),
          border-color var(--transition),
          box-shadow var(--transition),
          transform var(--transition-fast);
      }
      @keyframes bar-in {
        from {
          opacity: 0;
          transform: scale(0.94);
        }
        to {
          opacity: 1;
          transform: scale(1);
        }
      }

      /* Ready — the whole capsule is the button. */
      .rec-bar.is-ready {
        width: 340px;
        max-width: 100%;
        cursor: pointer;
        color: var(--text-primary);
        font-family: inherit;
      }
      .rec-bar.is-ready:hover:not(:disabled) {
        transform: translateY(-2px);
        border-color: var(--border-strong);
        box-shadow:
          0 32px 80px rgba(0, 0, 0, 0.6),
          0 0 0 1px var(--accent-soft),
          var(--glass-highlight);
      }
      .rec-bar.is-ready:active:not(:disabled) {
        transform: translateY(0);
      }
      .rec-bar.is-ready:focus-visible {
        outline: none;
        box-shadow:
          0 0 0 3px var(--accent-ring),
          var(--shadow-lg);
      }
      .rec-bar.is-ready:disabled {
        opacity: 0.45;
        cursor: not-allowed;
      }
      .ready-text {
        flex: 1;
        text-align: left;
        font-size: 1.0625rem;
        font-weight: 550;
        letter-spacing: -0.01em;
      }
      .kbd {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        height: 28px;
        padding: 0 var(--space-3);
        border-radius: var(--radius-sm);
        background: rgba(255, 255, 255, 0.07);
        border: 1px solid var(--border);
        color: var(--text-secondary);
        font-family: var(--font-mono);
        font-size: 0.8rem;
        font-weight: 500;
      }

      /* Recording — warm, alive. */
      .rec-bar.is-recording {
        width: 520px;
        max-width: 100%;
        border-color: rgba(255, 122, 92, 0.4);
        box-shadow: var(--live-glow), var(--shadow-lg), var(--glass-highlight);
      }
      .timer {
        font-family: var(--font-mono);
        font-size: 1.0625rem;
        font-weight: 500;
        font-variant-numeric: tabular-nums;
        letter-spacing: 0.02em;
        color: var(--text-primary);
        min-width: 56px;
      }
      .wave {
        flex: 1;
        display: flex;
        align-items: center;
        gap: 3px;
        height: 36px;
        padding: 0 var(--space-2);
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
        animation: wave 1100ms ease-in-out infinite;
        animation-delay: calc(var(--i) * -78ms);
      }
      @keyframes wave {
        0%,
        100% {
          transform: scaleY(calc(0.14 + var(--level, 0) * 0.55));
        }
        50% {
          transform: scaleY(calc(0.32 + var(--level, 0) * 1.15));
        }
      }

      /* Stop — warm circular button with a rounded square glyph. */
      .stop-btn {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 52px;
        height: 52px;
        min-width: 52px;
        border: none;
        border-radius: 50%;
        background: var(--live-gradient);
        cursor: pointer;
        box-shadow: 0 8px 24px rgba(255, 94, 120, 0.45);
        transition:
          transform var(--transition-fast),
          filter var(--transition);
      }
      .stop-btn:hover {
        filter: brightness(1.08);
        transform: scale(1.04);
      }
      .stop-btn:active {
        transform: scale(0.97);
      }
      .stop-btn:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px rgba(255, 122, 92, 0.6);
      }
      .stop-ico {
        width: 17px;
        height: 17px;
        border-radius: 5px;
        background: #fff;
      }

      /* Processing — cool, calm, working. */
      .rec-bar.is-processing {
        width: 420px;
        max-width: 100%;
      }
      .proc-label {
        flex: none;
        color: var(--text-primary);
        font-size: 0.95rem;
        font-weight: 550;
        text-transform: capitalize;
      }
      .proc-track {
        flex: 1;
        height: 4px;
        border-radius: var(--radius-pill);
        background: rgba(255, 255, 255, 0.08);
        overflow: hidden;
      }
      .proc-shimmer {
        height: 100%;
        width: 40%;
        border-radius: var(--radius-pill);
        background: var(--accent-gradient);
        animation: shimmer 1.3s ease-in-out infinite;
      }
      @keyframes shimmer {
        0% {
          transform: translateX(-120%);
        }
        100% {
          transform: translateX(320%);
        }
      }

      /* ── Status orb (left node) ────────────────────────────────────────── */
      .orb {
        position: relative;
        width: 14px;
        height: 14px;
        min-width: 14px;
        border-radius: 50%;
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
        animation: breathe 2.4s ease-in-out infinite;
      }
      @keyframes breathe {
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
        animation: live-pulse 1.4s ease-in-out infinite;
      }
      @keyframes live-pulse {
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
        background: transparent;
        border: 2px solid rgba(255, 255, 255, 0.18);
        border-top-color: var(--accent);
        animation: spin 0.8s linear infinite;
      }
      @keyframes spin {
        to {
          transform: rotate(360deg);
        }
      }

      .stage-hint {
        margin: 0;
        min-height: 1.2em;
        color: var(--text-muted);
        font-size: 0.875rem;
        letter-spacing: -0.005em;
      }

      /* ── Last note ─────────────────────────────────────────────────────── */
      .last-note h3 {
        margin-bottom: var(--space-3);
      }
      .note-card {
        padding: var(--space-4);
      }
      .path {
        margin: 0 0 var(--space-3);
        color: var(--text-muted);
        font-size: 0.8125rem;
        font-family: var(--font-mono);
        word-break: break-all;
      }
      .preview {
        margin: 0;
        white-space: pre-wrap;
        background: rgba(0, 0, 0, 0.22);
        border: 1px solid var(--border-subtle);
        color: var(--text-secondary);
        padding: var(--space-4);
        border-radius: var(--radius-md);
        max-height: 380px;
        overflow: auto;
        font-size: 0.85rem;
        line-height: 1.65;
      }
      .empty-card {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        padding: var(--space-5);
      }
      .empty-mark {
        width: 36px;
        height: 36px;
        min-width: 36px;
        border-radius: 50%;
        background: var(--accent-soft);
        position: relative;
      }
      .empty-mark::after {
        content: "";
        position: absolute;
        inset: 0;
        margin: auto;
        width: 11px;
        height: 11px;
        border-radius: 50%;
        background: var(--accent);
      }
      .empty {
        margin: 0;
        color: var(--text-secondary);
      }
      .kbd-inline {
        font-family: var(--font-mono);
        font-size: 0.8em;
        padding: 1px 6px;
        border-radius: 6px;
        background: rgba(255, 255, 255, 0.07);
        border: 1px solid var(--border);
        color: var(--text-secondary);
      }
    `,
  ],
})
export class RecordComponent implements OnInit {
  readonly store = inject(RecorderStore);
  private readonly ipc = inject(IpcService);

  /** Bars in the live waveform (driven by the real mic level signal). */
  readonly bars = Array.from({ length: 28 }, (_, i) => i);

  /** Latest settings snapshot, refreshed on entry — used for the readiness guard. */
  private readonly config = signal<AppConfigDto | null>(null);

  /** A vault folder is mandatory — export fails without it, so block recording. */
  readonly vaultMissing = computed(() => {
    const c = this.config();
    return !c || !c.vaultPath || c.vaultPath.trim() === "";
  });

  /** Real Whisper-model presence (null = checking). */
  readonly modelPresent = signal<boolean | null>(null);
  readonly downloadingModel = signal(false);
  readonly modelDownloadError = signal<string | null>(null);

  /** Busy but not capturing audio → transcribing / summarizing / exporting. */
  readonly isProcessing = computed(
    () => this.store.isBusy() && !this.store.isRecording(),
  );

  /** Whether the Record action is allowed right now. */
  readonly canRecord = computed(
    () =>
      !this.vaultMissing() &&
      this.modelPresent() !== false &&
      !this.downloadingModel() &&
      !this.store.isBusy(),
  );

  /** Elapsed recording time as m:ss. */
  readonly elapsedLabel = computed(() => {
    const s = this.store.elapsed();
    const m = Math.floor(s / 60);
    const sec = (s % 60).toString().padStart(2, "0");
    return `${m}:${sec}`;
  });

  /** Context line beneath the bar. */
  readonly hint = computed(() => {
    if (this.store.isRecording())
      return "Recording — press ⌘R or Stop when done.";
    if (this.isProcessing()) return "Transcribing on-device, then summarizing…";
    if (this.vaultMissing()) return "Set a vault folder in Settings to start.";
    if (this.modelPresent() === false) return "Download the model to start.";
    if (this.store.stage() === "done")
      return "Saved ✓ — your note is in the vault.";
    return "On-device transcription · your audio never leaves this Mac.";
  });

  async ngOnInit(): Promise<void> {
    await this.store.init();
    this.config.set(await this.ipc.getConfig());
    this.modelPresent.set(await this.ipc.modelPresent());
  }

  /** ⌘R / Ctrl+R toggles recording. */
  onKey(e: KeyboardEvent): void {
    if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "r") {
      e.preventDefault();
      if (this.store.isRecording()) {
        void this.store.stop();
      } else if (this.canRecord()) {
        void this.store.start();
      }
    }
  }

  /** Download the Whisper model, then re-check presence. */
  async downloadModel(): Promise<void> {
    this.modelDownloadError.set(null);
    this.downloadingModel.set(true);
    try {
      await this.ipc.downloadModel();
      this.modelPresent.set(await this.ipc.modelPresent());
    } catch (e) {
      this.modelDownloadError.set(String(e));
    } finally {
      this.downloadingModel.set(false);
    }
  }
}
