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
      <a routerLink="/library" class="back">
        <span class="back-arrow" aria-hidden="true">←</span>
        <span>Meetings</span>
      </a>

      @if (detail(); as d) {
        <header class="head">
          <div class="head-text">
            <h2>{{ d.meeting.title || "(untitled)" }}</h2>
            <div class="meta">
              <span class="pill" [class]="statusPillClass(d.meeting.status)">
                <span class="pill-dot"></span>
                {{ d.meeting.status }}
              </span>
              <span class="meta-sep" aria-hidden="true">·</span>
              <span class="meta-item">{{ d.meeting.startedAt }}</span>
              <span class="meta-sep" aria-hidden="true">·</span>
              <span class="meta-item">{{ d.meeting.durationS }}s</span>
            </div>
          </div>

          <div class="actions">
            <button
              type="button"
              class="btn btn-primary"
              (click)="resummarize(d.meeting.id)"
              [disabled]="busy()"
            >
              Re-summarize
            </button>
            @if (msg()) {
              <span class="msg">{{ msg() }}</span>
            }
          </div>
        </header>

        <section class="block">
          <h3>Note</h3>
          @if (d.note; as note) {
            <article class="card note-card">
              @if (note.exportedPath) {
                <p class="path">{{ note.exportedPath }}</p>
              }
              <pre class="note-body">{{ note.markdown }}</pre>
            </article>
          } @else {
            <div class="card empty-card">
              <p class="empty">No note yet.</p>
            </div>
          }
        </section>

        <section class="block">
          <h3>Transcript</h3>
          @if (d.segments.length) {
            <div class="card transcript-card">
              <ul class="segs">
                @for (s of d.segments; track s.idx) {
                  <li class="seg">
                    <span class="seg-time">{{ fmt(s.startS) }}</span>
                    <span class="seg-text">{{ s.text }}</span>
                  </li>
                }
              </ul>
            </div>
          } @else {
            <div class="card empty-card">
              <p class="empty">No transcript.</p>
            </div>
          }
        </section>
      } @else if (loading()) {
        <p class="text-secondary">Loading…</p>
      } @else {
        <div class="card empty-card">
          <p class="empty">Meeting not found.</p>
        </div>
      }
    </section>
  `,
  styles: [
    `
      .detail {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
      }

      /* --- Back link --- */
      .back {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        align-self: flex-start;
        color: var(--text-secondary);
        font-size: 0.875rem;
        font-weight: 550;
      }
      .back:hover {
        color: var(--text-primary);
      }
      .back:focus-visible {
        outline: none;
        color: var(--text-primary);
        border-radius: var(--radius-sm);
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .back-arrow {
        font-size: 1rem;
        line-height: 1;
      }

      /* --- Header: title, status + meta, primary action --- */
      .head {
        display: flex;
        flex-wrap: wrap;
        align-items: flex-start;
        justify-content: space-between;
        gap: var(--space-4);
      }
      .head-text {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        min-width: 0;
      }
      .head h2 {
        margin: 0;
      }
      .meta {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: var(--space-2);
        color: var(--text-muted);
        font-size: 0.8125rem;
      }
      .meta-item {
        color: var(--text-muted);
      }
      .meta-sep {
        color: var(--text-muted);
      }

      .actions {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: var(--space-3);
      }
      .msg {
        color: var(--text-secondary);
        font-size: 0.85rem;
      }

      /* --- Section blocks --- */
      .block {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .block h3 {
        margin: 0;
      }

      /* --- Note card (readable measure + line-height) --- */
      .note-card {
        padding: var(--space-5);
      }
      .path {
        margin: 0 0 var(--space-4);
        color: var(--text-muted);
        font-size: 0.8125rem;
        font-family: var(--font-mono);
        word-break: break-all;
      }
      .note-body {
        margin: 0;
        white-space: pre-wrap;
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
        color: var(--text-secondary);
        padding: var(--space-4);
        border-radius: var(--radius-md);
        max-height: 360px;
        overflow: auto;
        font-size: 0.9rem;
        line-height: 1.7;
      }

      /* --- Transcript list --- */
      .transcript-card {
        padding: var(--space-2) var(--space-5);
        max-height: 480px;
        overflow: auto;
      }
      .segs {
        list-style: none;
        padding: 0;
        margin: 0;
      }
      .seg {
        display: flex;
        gap: var(--space-3);
        padding: var(--space-3) 0;
        border-bottom: 1px solid var(--border-subtle);
        font-size: 0.9rem;
        line-height: 1.6;
      }
      .seg:last-child {
        border-bottom: none;
      }
      .seg-time {
        flex: none;
        color: var(--text-muted);
        font-family: var(--font-mono);
        font-size: 0.8125rem;
        font-variant-numeric: tabular-nums;
        padding-top: 0.1em;
      }
      .seg-text {
        color: var(--text-secondary);
        min-width: 0;
      }

      /* --- Empty wells --- */
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

  /** Maps a meeting status to a status-pill state modifier (presentation only). */
  statusPillClass(status: string): string {
    switch (status) {
      case "RECORDING":
      case "ERROR":
        return "is-danger";
      case "TRANSCRIBED":
      case "SUMMARIZED":
        return "is-accent";
      case "EXPORTED":
        return "is-success";
      default:
        return "";
    }
  }
}
