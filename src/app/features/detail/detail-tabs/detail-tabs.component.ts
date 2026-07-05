import {
  ChangeDetectionStrategy,
  Component,
  input,
  output,
} from "@angular/core";

/** The three note-detail sections. Extensible: add an id + a `tabs` entry + a shell `@if`. */
export type DetailTab = "note" | "audio" | "share";

/** One tab entry for the {@link DetailTabsComponent} bar. */
export interface DetailTabDef {
  id: DetailTab;
  label: string;
}

/**
 * The page-scale segmented tab bar (Note · Audio · Share) that sits above the
 * active panel. Purely presentational: the shell owns the `active` signal and
 * re-renders the matching panel on `tabChange`. Reuses the global `.tabbar` /
 * `.seg` / `.seg-btn` primitives so this component ships almost no CSS.
 */
@Component({
  selector: "app-detail-tabs",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./detail-tabs.component.html",
  styleUrl: "./detail-tabs.component.scss",
})
export class DetailTabsComponent {
  /** The tabs to render (order = display order). */
  readonly tabs = input.required<DetailTabDef[]>();
  /** The currently-active tab id. */
  readonly active = input.required<DetailTab>();
  /** Fired with the clicked tab id (the shell flips its `activeTab` signal). */
  readonly tabChange = output<DetailTab>();
}
