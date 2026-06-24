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
            <p class="model-banner-title">Whisper model needed</p>
            <p class="model-banner-text">
              Transcription runs on-device. Download the model once to enable
              recording.
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
              @if (downloadingModel()) {
                Downloading…
              } @else {
                Download model (~150 MB)
              }
            </button>
          </div>
        </div>
      }

      <div class="hero card">
        <div class="hero-status">
          <span class="pill" [class]="statusPillClass()">
            <span class="pill-dot"></span>
            {{ store.stage() }}
          </span>
          @if (store.message()) {
            <span class="hero-message">{{ store.message() }}</span>
          }
        </div>

        <div class="controls">
          @if (!store.isRecording()) {
            <button
              type="button"
              class="record-btn"
              (click)="store.start()"
              [disabled]="
                store.isBusy() ||
                vaultMissing() ||
                modelPresent() === false ||
                downloadingModel()
              "
            >
              <span class="record-icon" aria-hidden="true"></span>
              <span>Record</span>
            </button>
          } @else {
            <button
              type="button"
              class="record-btn is-recording"
              (click)="store.stop()"
            >
              <span class="record-icon stop" aria-hidden="true"></span>
              <span>Stop</span>
            </button>
          }
        </div>

        @if (store.isRecording()) {
          <div
            class="meter"
            role="progressbar"
            aria-label="Microphone level"
            [attr.aria-valuenow]="levelPct()"
            aria-valuemin="0"
            aria-valuemax="100"
          >
            <div class="meter-fill" [style.width.%]="levelPct()"></div>
          </div>
        }
      </div>

      @if (store.error(); as err) {
        <div class="banner is-danger" role="alert">
          <span class="banner-icon" aria-hidden="true">!</span>
          <span>{{ err }}</span>
        </div>
      }

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
            <p class="empty">
              No note yet — hit Record to capture your first meeting.
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

      /* --- Hero recording surface --- */
      .hero {
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: var(--space-5);
        padding: var(--space-7) var(--space-5);
        border-radius: var(--radius-xl);
        text-align: center;
      }
      .hero-status {
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: var(--space-2);
        min-height: 28px;
      }
      .hero-message {
        color: var(--text-secondary);
        font-size: 0.875rem;
      }

      .controls {
        display: flex;
        justify-content: center;
      }

      /* The primary Record control — large, inviting, accent-filled. */
      .record-btn {
        display: inline-flex;
        flex-direction: column;
        align-items: center;
        justify-content: center;
        gap: var(--space-2);
        width: 168px;
        height: 168px;
        border: none;
        border-radius: 50%;
        background: var(--accent-gradient);
        color: var(--text-on-accent);
        font-family: inherit;
        font-size: 1.0625rem;
        font-weight: 600;
        letter-spacing: -0.01em;
        cursor: pointer;
        box-shadow: var(--shadow-accent);
        transition:
          transform var(--transition),
          box-shadow var(--transition),
          filter var(--transition);
      }
      .record-btn:hover:not(:disabled) {
        transform: translateY(-2px);
        filter: brightness(1.06);
        box-shadow: 0 14px 40px rgba(110, 91, 255, 0.5);
      }
      .record-btn:active:not(:disabled) {
        transform: translateY(0);
      }
      .record-btn:focus-visible {
        outline: none;
        box-shadow:
          0 0 0 4px var(--accent-ring),
          var(--shadow-accent);
      }
      .record-btn:disabled {
        opacity: 0.4;
        cursor: not-allowed;
        box-shadow: none;
        filter: grayscale(0.4);
      }

      .record-icon {
        width: 22px;
        height: 22px;
        border-radius: 50%;
        background: var(--text-on-accent);
      }
      .record-icon.stop {
        border-radius: var(--radius-sm);
      }

      /* While recording: soft glowing pulse to signal "live". */
      .record-btn.is-recording {
        animation: record-pulse 2s ease-in-out infinite;
      }
      @keyframes record-pulse {
        0%,
        100% {
          box-shadow:
            0 0 0 0 rgba(110, 91, 255, 0.45),
            var(--shadow-accent);
        }
        50% {
          box-shadow:
            0 0 0 18px rgba(110, 91, 255, 0),
            var(--shadow-accent);
        }
      }

      /* --- Mic level meter --- */
      .meter {
        height: 8px;
        width: 100%;
        max-width: 320px;
        background: rgba(255, 255, 255, 0.08);
        border-radius: var(--radius-pill);
        overflow: hidden;
      }
      .meter-fill {
        height: 100%;
        background: var(--accent-gradient);
        border-radius: var(--radius-pill);
        transition: width 80ms linear;
      }

      /* --- Last note --- */
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
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
        color: var(--text-secondary);
        padding: var(--space-4);
        border-radius: var(--radius-md);
        max-height: 360px;
        overflow: auto;
        font-size: 0.85rem;
        line-height: 1.6;
      }
      .empty-card {
        padding: var(--space-5);
      }
      .empty {
        margin: 0;
        color: var(--text-muted);
      }
    `,
  ],
})
export class RecordComponent implements OnInit {
  readonly store = inject(RecorderStore);
  private readonly ipc = inject(IpcService);

  /** Latest settings snapshot, refreshed on entry, used for the readiness guard (SF-4). */
  private readonly config = signal<AppConfigDto | null>(null);

  /** A vault folder is mandatory — export fails without it, so block recording. */
  readonly vaultMissing = computed(() => {
    const c = this.config();
    return !c || !c.vaultPath || c.vaultPath.trim() === "";
  });

  /** A model is needed for transcription; soft warning (a default may exist on disk). */
  readonly modelMissing = computed(() => {
    const c = this.config();
    return !c || !c.whisperModelPath || c.whisperModelPath.trim() === "";
  });

  /**
   * Real Whisper-model presence (replaces the config-path heuristic for gating).
   * `null` = not yet checked, `true`/`false` = detected via ipc.modelPresent().
   */
  readonly modelPresent = signal<boolean | null>(null);

  /** True while a download is in-flight — disables Record + the download button. */
  readonly downloadingModel = signal(false);

  /** Surfaced if ipc.downloadModel() rejects. */
  readonly modelDownloadError = signal<string | null>(null);

  /** 0..100 width for the mic level bar. */
  readonly levelPct = computed(() =>
    Math.round(Math.max(0, Math.min(1, this.store.level())) * 100),
  );

  /** Maps the current stage to a status-pill state modifier. */
  readonly statusPillClass = computed(() => {
    switch (this.store.stage()) {
      case "recording":
        return "is-danger";
      case "transcribing":
      case "summarizing":
      case "exporting":
        return "is-accent";
      case "done":
        return "is-success";
      case "error":
        return "is-danger";
      default:
        return "";
    }
  });

  async ngOnInit(): Promise<void> {
    await this.store.init();
    this.config.set(await this.ipc.getConfig());
    this.modelPresent.set(await this.ipc.modelPresent());
  }

  /** Download the default Whisper model, then re-check presence and clear on success. */
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
