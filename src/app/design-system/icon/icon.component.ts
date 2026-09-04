import { ChangeDetectionStrategy, Component, input } from "@angular/core";

/** Every glyph the shell chrome can render (nav + quick actions + chrome). */
export type ShellIcon =
  // The Murmur brand mark itself — the five-bar waveform from
  // `src-tauri/icons/icon.svg`, drawn in `currentColor` so the rail tile paints
  // it in the user's accent instead of shipping a second, fixed-gradient copy.
  | "murmur"
  | "record"
  | "meetings"
  | "notes"
  | "reminders"
  | "tasks"
  | "dashboards"
  | "analytics"
  | "graph"
  | "people"
  | "brain"
  | "ask"
  | "settings"
  | "search"
  | "spaces"
  | "shared-brains"
  | "browse"
  | "history"
  | "plus"
  | "note-add"
  | "folder"
  | "folder-add"
  | "move"
  | "rename"
  // Edit / read the same document — the note editor's mode toggle. `rename` is a
  // pencil ON a baseline (retitle a thing); `edit` is the bare pencil (change the
  // thing's contents), and `eye` is its read-only counterpart.
  | "edit"
  | "eye"
  | "trash"
  | "unlock"
  | "check"
  | "chevron-right"
  | "sidebar"
  | "topbar"
  | "sun"
  | "moon"
  | "display"
  // Dashboard tile marks. A board tile identifies its KIND by a glyph, and the
  // glyph set lives here with every other icon rather than inline in one feature.
  | "document"
  | "drift"
  | "numbers"
  | "pulse"
  | "promises"
  | "lock"
  // Developer mode — the diagnostics surface behind the Settings toggle.
  // `developer` is the terminal chevron+caret; `logs` is a stack of log lines.
  | "developer"
  | "logs"
  // Relationship marks. `link` is the two-ring chain (create a relation);
  // `close` is the plain X every dismissible overlay needs. Both live here rather
  // than inline in one feature, so the next surface that needs them reuses one glyph.
  | "link"
  | "close";

/**
 * Design System — one inline-SVG glyph, shared by the floating sidebar, the
 * pill bar and any future chrome so the icon set lives in exactly one place.
 */
@Component({
  selector: "mur-icon",
  changeDetection: ChangeDetectionStrategy.OnPush,
  host: {
    "[attr.data-icon]": "icon()",
  },
  templateUrl: "./icon.component.html",
  styleUrl: "./icon.component.scss",
})
export class MurIconComponent {
  readonly icon = input.required<ShellIcon>();
}
