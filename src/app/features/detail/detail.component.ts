import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  inject,
  signal,
} from "@angular/core";
import { ActivatedRoute, RouterLink } from "@angular/router";
import { IpcService } from "../../core/ipc.service";
import type { MeetingDetail } from "../../core/models";

@Component({
  selector: "app-detail",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  template: `
    <section class="detail">
      <a routerLink="/library" class="back">← Meetings</a>

      @if (detail(); as d) {
        <h2>{{ d.meeting.title || "(untitled)" }}</h2>
        <p class="meta">
          {{ d.meeting.startedAt }} · {{ d.meeting.durationS }}s ·
          {{ d.meeting.status }}
        </p>

        <div class="actions">
          <button
            type="button"
            (click)="resummarize(d.meeting.id)"
            [disabled]="busy()"
          >
            Re-summarize
          </button>
          @if (msg()) {
            <span class="msg">{{ msg() }}</span>
          }
        </div>

        <h3>Note</h3>
        @if (d.note; as note) {
          @if (note.exportedPath) {
            <p class="path">{{ note.exportedPath }}</p>
          }
          <pre class="preview">{{ note.markdown }}</pre>
        } @else {
          <p class="empty">No note yet.</p>
        }

        <h3>Transcript</h3>
        @if (d.segments.length) {
          <ul class="segs">
            @for (s of d.segments; track s.idx) {
              <li>
                <span class="t">[{{ fmt(s.startS) }}]</span> {{ s.text }}
              </li>
            }
          </ul>
        } @else {
          <p class="empty">No transcript.</p>
        }
      } @else if (loading()) {
        <p>Loading…</p>
      } @else {
        <p class="empty">Meeting not found.</p>
      }
    </section>
  `,
  styles: [
    `
      .detail {
        max-width: 820px;
      }
      .back {
        display: inline-block;
        margin-bottom: 0.5rem;
        opacity: 0.7;
        text-decoration: none;
        color: inherit;
      }
      .meta {
        font-size: 0.85rem;
        opacity: 0.65;
      }
      .actions {
        display: flex;
        align-items: center;
        gap: 0.75rem;
        margin: 0.5rem 0 1rem;
      }
      .msg {
        font-size: 0.85rem;
        opacity: 0.8;
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
      .segs {
        list-style: none;
        padding: 0;
        margin: 0;
        font-size: 0.9rem;
      }
      .segs li {
        padding: 0.15rem 0;
      }
      .segs .t {
        opacity: 0.5;
        font-variant-numeric: tabular-nums;
        margin-right: 0.4rem;
      }
    `,
  ],
})
export class DetailComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly route = inject(ActivatedRoute);

  readonly detail = signal<MeetingDetail | null>(null);
  readonly loading = signal(true);
  readonly busy = signal(false);
  readonly msg = signal("");

  async ngOnInit(): Promise<void> {
    const id = this.route.snapshot.paramMap.get("id");
    if (!id) {
      this.loading.set(false);
      return;
    }
    try {
      this.detail.set(await this.ipc.getMeetingDetail(id));
    } finally {
      this.loading.set(false);
    }
  }

  async resummarize(id: string): Promise<void> {
    this.busy.set(true);
    this.msg.set("Re-summarizing…");
    try {
      await this.ipc.resummarize(id);
      this.detail.set(await this.ipc.getMeetingDetail(id));
      this.msg.set("Done.");
    } catch (e) {
      this.msg.set("Error: " + String(e));
    } finally {
      this.busy.set(false);
    }
  }

  /** Seconds → m:ss for the transcript timestamps. */
  fmt(s: number): string {
    const m = Math.floor(s / 60);
    const sec = Math.floor(s % 60);
    return `${m}:${sec.toString().padStart(2, "0")}`;
  }
}
