import {
  ChangeDetectionStrategy,
  Component,
  input,
  output,
} from "@angular/core";

/** The meeting's four right-docked tool drawers. */
export type DetailTool = "actions" | "live" | "smart" | "ask";

/** One entry in the {@link DetailToolsComponent} switcher. */
export interface DetailToolDef {
  id: DetailTool;
  /** The control's accessible name — kept EXACT: e2e locators match on it. */
  label: string;
  /** The longer `title` tooltip; never the accessible name. */
  hint: string;
  /** Render `label` visibly beside the glyph instead of only to a reader. */
  showLabel?: boolean;
}

/**
 * The meeting's TOOL switcher (Action items · Live context · Smart reminders ·
 * Ask), beside the view switcher it mirrors.
 *
 * These four were four independent ghost buttons, which misdescribed what they
 * do: the shell holds ONE `_openDrawer` signal of type
 * `"ask" | "actions" | "smart" | "live" | null`, so at most one drawer is ever
 * open and choosing a second retires the first. That is a segmented control,
 * and drawing it as four separate toggles both cost four separate hit areas in
 * an already crowded command row and hid the exclusivity from the user until
 * they clicked and watched something else close.
 *
 * They are NOT tabs and NOT a radio group: `null` (nothing open) is a real
 * state, and clicking the active item returns to it. So each item stays a
 * plain button carrying `aria-expanded` — exactly the semantics it had as a
 * standalone toggle, which is also what keeps every existing locator working.
 *
 * Purely presentational: the shell owns `openDrawer` and its per-drawer open
 * side effects (Ask focuses its composer), and this component only reports
 * which item was pressed.
 */
@Component({
  selector: "app-detail-tools",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./detail-tools.component.html",
  styleUrl: "./detail-tools.component.scss",
})
export class DetailToolsComponent {
  /** The tools to render (order = display order). */
  readonly tools = input.required<DetailToolDef[]>();
  /** The open drawer, or `null` when every drawer is closed. */
  readonly active = input.required<DetailTool | null>();
  /** Fired with the pressed tool id; the shell applies its own toggle. */
  readonly toolChange = output<DetailTool>();
}
