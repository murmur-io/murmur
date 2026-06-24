import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  computed,
  inject,
} from "@angular/core";
import { RecorderStore } from "../../core/recorder.store";

@Component({
  selector: "app-record",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <section class="record">
      <div class="controls">
        @if (!store.isRecording()) {
          <button
            type="button"
            (click)="store.start()"
            [disabled]="store.isBusy()"
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

  /** 0..100 width for the mic level bar. */
  readonly levelPct = computed(() =>
    Math.round(Math.max(0, Math.min(1, this.store.level())) * 100),
  );

  async ngOnInit(): Promise<void> {
    await this.store.init();
  }
}
