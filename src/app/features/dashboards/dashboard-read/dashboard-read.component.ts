import { ChangeDetectionStrategy, Component, input, output } from "@angular/core";
import type { SourceRef } from "../../../core/models";
import { MurIconComponent } from "../../../design-system/icon/icon.component";
import type {
  BoardProjection,
  DashboardLens,
} from "../dashboard-projection";

@Component({
  selector: "app-dashboard-read",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurIconComponent],
  templateUrl: "./dashboard-read.component.html",
  styleUrl: "./dashboard-read.component.scss",
})
export class DashboardReadComponent {
  readonly lens = input.required<DashboardLens>();
  readonly projection = input.required<BoardProjection>();
  readonly openSource = output<SourceRef>();

  open(source: SourceRef | null): void {
    if (source) this.openSource.emit(source);
  }

  formatDate(iso: string): string {
    const timestamp = Date.parse(iso);
    if (Number.isNaN(timestamp)) return "time unavailable";
    return new Date(timestamp).toLocaleDateString(undefined, {
      day: "numeric",
      month: "short",
      year: "numeric",
    });
  }
}
