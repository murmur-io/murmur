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
 * The meeting's view switcher (Note · Audio · Share) — ICON-ONLY, living in the
 * command row beside the title rather than in a band of its own (2026-09-13).
 * Purely presentational: the shell owns the `active` signal and re-renders the
 * matching panel on `tabChange`.
 *
 * It used to render the global `.tabbar` / `.seg` primitives, i.e. the SMALL
 * segmented control stretched to the full page width — which gave a navigation
 * strip the visual weight of content. Each label survives as `.sr-only` text,
 * so the accessible name is unchanged and only the painted form differs.
 */
@Component({
  selector: "app-detail-tabs",
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
