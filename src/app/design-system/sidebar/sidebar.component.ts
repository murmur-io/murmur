import { ChangeDetectionStrategy, Component } from "@angular/core";

/**
 * Design System — <mur-sidebar>: THE floating liquid-glass rail panel (Apple
 * TV chrome). One look for every rail: the primary sidebar, Settings sections
 * and the Meetings folder tree all project their content into it. The glass
 * itself comes from the global .drill-rail class (kept global because the
 * shell chrome must be styleable before Angular boots — the WKWebView FOUC
 * fix documented in app-shell).
 */
@Component({
  selector: "mur-sidebar",
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: { class: "drill-rail" },
  templateUrl: "./sidebar.component.html",
  styleUrl: "./sidebar.component.scss",
})
export class MurSidebarComponent {}
