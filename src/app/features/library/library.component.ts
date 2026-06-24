import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  inject,
  signal,
} from "@angular/core";
import { RouterLink } from "@angular/router";
import { IpcService } from "../../core/ipc.service";
import type { Meeting } from "../../core/models";

@Component({
  selector: "app-library",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink],
  template: `
    <section class="library">
      <h2>Meetings</h2>
      @if (loading()) {
        <p>Loading…</p>
      } @else if (meetings().length === 0) {
        <p class="empty">No meetings yet — record one from the Record tab.</p>
      } @else {
        <ul class="list">
          @for (m of meetings(); track m.id) {
            <li>
              <a [routerLink]="['/meeting', m.id]">
                <span class="title">{{ m.title || "(untitled)" }}</span>
                <span class="meta"
                  >{{ m.startedAt }} · {{ statusLabel(m.status) }}</span
                >
              </a>
            </li>
          }
        </ul>
      }
    </section>
  `,
  styles: [
    `
      .library {
        max-width: 760px;
      }
      .list {
        list-style: none;
        padding: 0;
        margin: 0;
      }
      .list li a {
        display: flex;
        flex-direction: column;
        gap: 0.15rem;
        padding: 0.6rem 0.5rem;
        border-bottom: 1px solid rgba(128, 128, 128, 0.2);
        text-decoration: none;
        color: inherit;
      }
      .list li a:hover {
        background: rgba(128, 128, 128, 0.08);
      }
      .title {
        font-weight: 600;
      }
      .meta {
        font-size: 0.8rem;
        opacity: 0.65;
      }
    `,
  ],
})
export class LibraryComponent implements OnInit {
  private readonly ipc = inject(IpcService);

  readonly meetings = signal<Meeting[]>([]);
  readonly loading = signal(true);

  async ngOnInit(): Promise<void> {
    try {
      this.meetings.set(await this.ipc.listMeetings());
    } finally {
      this.loading.set(false);
    }
  }

  statusLabel(s: string): string {
    return s.charAt(0) + s.slice(1).toLowerCase();
  }
}
