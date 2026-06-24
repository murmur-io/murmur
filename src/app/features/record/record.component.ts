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
        <p class="warn" role="alert">
          ⚠️ Set your Obsidian <strong>vault folder</strong> in Settings before
          recording — notes can't be saved without it.
        </p>
      } @else if (modelMissing()) {
        <p class="warn" role="alert">
          ⚠️ No <strong>Whisper model</strong> configured. You can record, but
          transcription needs a model — set its path in Settings (or place a
          default model in the app's models folder).
        </p>
      }

      <div class="controls">
        @if (!store.isRecording()) {
          <button
            type="button"
            (click)="store.start()"
            [disabled]="store.isBusy() || vaultMissing()"
          >
            Record
          </button>
        } @else {
          <button type="button" (click)="store.stop()">Stop</button>
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

      <p class="status">
        <strong>{{ store.stage() }}</strong>
        @if (store.message()) {
          <span> — {{ store.message() }}</span>
        }
      </p>

      @if (store.error(); as err) {
        <p class="error">{{ err }}</p>
      }

      <h3>Last note</h3>
      @if (store.lastNote(); as note) {
        @if (note.exportedPath) {
          <p class="path">{{ note.exportedPath }}</p>
        }
        <pre class="preview">{{ note.markdown }}</pre>
      } @else {
        <p class="empty">No note yet.</p>
      }
    </section>
  `,
  styles: [
    `
      .record {
        max-width: 760px;
      }
      .warn {
        background: rgba(241, 196, 15, 0.15);
        border: 1px solid rgba(241, 196, 15, 0.5);
        border-radius: 6px;
        padding: 0.5rem 0.75rem;
        font-size: 0.9rem;
      }
      .controls button {
        font-size: 1rem;
        padding: 0.5rem 1.25rem;
      }
      .meter {
        margin-top: 0.75rem;
        height: 8px;
        width: 100%;
        max-width: 320px;
        background: rgba(128, 128, 128, 0.2);
        border-radius: 4px;
        overflow: hidden;
      }
      .meter-fill {
        height: 100%;
        background: #27ae60;
        transition: width 80ms linear;
      }
      .status {
        margin-top: 1rem;
      }
      .error {
        color: #c0392b;
      }
      .preview {
        white-space: pre-wrap;
        background: rgba(128, 128, 128, 0.12);
        padding: 0.75rem;
        border-radius: 6px;
        max-height: 360px;
        overflow: auto;
      }
      .path {
        opacity: 0.7;
        font-size: 0.85rem;
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

  /** 0..100 width for the mic level bar. */
  readonly levelPct = computed(() =>
    Math.round(Math.max(0, Math.min(1, this.store.level())) * 100),
  );

  async ngOnInit(): Promise<void> {
    await this.store.init();
    this.config.set(await this.ipc.getConfig());
  }
}
