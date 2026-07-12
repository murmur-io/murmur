import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { Router } from "@angular/router";
import { TabsService } from "../../core/tabs.service";

/**
 * Browser-style tab strip for Murmur's open meeting/note "document" tabs.
 * Renders whenever {@link TabsService.tabs} is non-empty as a REAL in-flow
 * Apple Liquid Glass tab row — the same `--shell-glass-*`/`--shell-active-*`
 * chrome language `.pill-bar` and the sidebar's active nav item already use.
 *
 * IN-FLOW, NOT FLOATING (fixed 2026-07-12 — was `position: fixed`, a
 * floating overlay that could never make the page underneath aware space
 * was taken, so every route needed its own pixel-guess clearance). This is
 * now a genuine sibling ABOVE `<main class="app-main">` inside
 * `AppShellComponent`'s `.main-col` flex column (see its template) — when
 * tabs are open it PUSHES `.app-main` down by its real rendered height via
 * normal box flow, and contributes zero height when empty (this `@if`).
 * The one remaining wrinkle — drill-down routes (library / notes home / note
 * editor / settings) render their OWN `position: fixed` full-window host,
 * which would paint over this strip regardless of DOM order — is solved on
 * THEIR side: they read `top: var(--tabs-strip-height, 0px)` (set on
 * `<html>` by `AppShellComponent`, driven by this same tab count) instead of
 * `inset: 0`, so they structurally leave this strip's real height uncovered.
 *
 * Injects `TabsService`/`Router` directly (the same pattern `mur-quick-search`
 * uses) rather than threading inputs/outputs through `AppShellComponent` —
 * this component IS the tab strip, not a generic list.
 */
@Component({
  selector: "mur-tab-strip",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./tab-strip.component.html",
  styleUrl: "./tab-strip.component.scss",
})
export class MurTabStripComponent {
  private readonly tabsService = inject(TabsService);
  private readonly router = inject(Router);

  readonly tabs = this.tabsService.tabs;
  readonly activeTabId = this.tabsService.activeTabId;

  /** Activate a tab (click anywhere on it except the close button). */
  activate(id: string): void {
    this.tabsService.activate(id);
  }

  /** Close a tab. `stopPropagation` so it never also activates the tab. */
  close(id: string, event: Event): void {
    event.stopPropagation();
    void this.tabsService.closeTab(id);
  }

  /**
   * Trailing "+" — new note tab. Reuses the EXACT existing note-creation
   * seam (`AppShellComponent.newNote()` / the ⌘N quick-search action / the
   * sidebar "New note" button all do the same one-line navigation): routing
   * to `/notes/new` is what triggers `NoteEditorComponent`'s own
   * `createAndOpen()` effect, which creates the note via `NotesService` and
   * hands off to `TabsService.openNote()` once the real id exists. No new
   * creation logic here — this component injects `Router` directly (the
   * same pattern `mur-quick-search` uses) rather than duplicating the create
   * call or adding an output the shell has to wire.
   */
  newTab(): void {
    void this.router.navigate(["/notes/new"]);
  }
}
