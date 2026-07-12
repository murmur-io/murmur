import { ChangeDetectionStrategy, Component, input, output } from "@angular/core";
import { RouterLink, RouterLinkActive } from "@angular/router";
import { FolderDropDirective } from "../../features/folders/folder-drop.directive";
import { MurIconComponent, type ShellIcon } from "../icon/icon.component";

/**
 * Design System — <mur-sidebar-section>: the EXPANDABLE nav-section shell
 * (header row + projected tree body) shared by the main sidebar's "Notes" and
 * "Meetings" sections (extracted 2026-07-12 after four rounds of style drift
 * between two independently-authored copies of this exact same UI pattern).
 *
 * OWNS: the header row — nav icon + label + `routerLink`/`routerLinkActive`
 * (the header IS the "all items" affordance: clicking it navigates to the
 * section's list route AND emits `headerSelect` so the caller can clear any
 * folder filter — the separate "All meetings"/"All notes" root row this
 * component used to render was removed 2026-07-12 as a redundant extra layer
 * duplicating exactly that), the compact "+" add-folder icon, and the
 * expand/collapse chevron. Content-projects (`<ng-content>`) the
 * feature-specific folder-TREE BODY — that part is genuinely different per
 * feature (different backend/IPC, create/rename/delete/lock/unlock/
 * drag-drop) and stays in `MeetingsSidebarTreeComponent`/
 * `NotesSidebarTreeComponent`; only the shell around it is one component.
 *
 * The header row optionally accepts a dropped note (`enableHeaderDrop` +
 * `headerDropNote`) — dropping a meeting onto the "Meetings" header files it
 * back to the vault root (the drop target moved here with the removed root
 * row; the natural Notion/Obsidian pattern). Notes' header isn't a drop
 * target (Notes has no drag source). Importing `FolderDropDirective` here is
 * a deliberate, scoped exception to design-system/feature layering (mirrors
 * `AppShellComponent` already importing the feature tree components
 * directly) — the alternative was losing the drag-to-vault-root affordance.
 *
 * Usage:
 * ```html
 * <mur-sidebar-section
 *   routePath="/library" icon="meetings" label="Meetings"
 *   addFolderLabel="New meeting folder"
 *   [expanded]="meetingsTreeOpen()" [enableHeaderDrop]="true"
 *   (toggleExpanded)="toggleMeetingsTree()" (addFolder)="newMeetingFolder()"
 *   (headerSelect)="folders.selectFolder(null)"
 *   (headerDropNote)="onDropToRoot($event)"
 * >
 *   <app-meetings-sidebar-tree [sectionActive]="isMeetingsRoute()" />
 * </mur-sidebar-section>
 * ```
 */
@Component({
  selector: "mur-sidebar-section",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterLink, RouterLinkActive, MurIconComponent, FolderDropDirective],
  templateUrl: "./sidebar-section.component.html",
  styleUrl: "./sidebar-section.component.scss",
})
export class MurSidebarSectionComponent {
  /** The nav link's target route (also drives the header's `routerLinkActive` pill). */
  readonly routePath = input.required<string>();
  /** The header row's nav glyph. */
  readonly icon = input.required<ShellIcon>();
  /** The header row's visible label (e.g. "Meetings"). */
  readonly label = input.required<string>();
  /** The "+" icon's accessible name (e.g. "New meeting folder") — kept distinct per section for a11y. */
  readonly addFolderLabel = input<string>("New folder");

  /** Whether the projected tree body is expanded. */
  readonly expanded = input(false);
  /** Whether the header accepts a dropped note (Meetings only — see the class doc). */
  readonly enableHeaderDrop = input(false);

  /** The chevron was clicked — the caller owns the persisted expand/collapse signal. */
  readonly toggleExpanded = output<void>();
  /** The "+" icon was clicked — the caller forwards this into its tree body's create-folder flow. */
  readonly addFolder = output<void>();
  /**
   * The header link was clicked — fires ALONGSIDE the routerLink navigation
   * so the caller can clear its folder filter (header = "all items" now that
   * the root row is gone).
   */
  readonly headerSelect = output<void>();
  /** A note was dropped onto the header (only fires when `enableHeaderDrop` is true). */
  readonly headerDropNote = output<string>();
}
