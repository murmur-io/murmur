import { ChangeDetectionStrategy, Component, computed, input } from "@angular/core";

export type BannerKind = "info" | "success" | "warning" | "danger";

/**
 * Design System — <mur-banner kind="danger">…</mur-banner>: the status strip.
 * Rides the global .banner primitive classes on the host element.
 */
@Component({
  selector: "mur-banner",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: {
    class: "banner",
    "[class.is-info]": 'kind() === "info"',
    "[class.is-success]": 'kind() === "success"',
    "[class.is-warning]": 'kind() === "warning"',
    "[class.is-danger]": 'kind() === "danger"',
    role: "alert",
  },
  templateUrl: "./banner.component.html",
  styleUrl: "./banner.component.scss",
})
export class MurBannerComponent {
  readonly kind = input<BannerKind>("info");
  readonly glyph = computed(() =>
    this.kind() === "danger" || this.kind() === "warning" ? "!" : "i",
  );
}
