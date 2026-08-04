import { Injectable, signal } from "@angular/core";
import type { TileConfig, TileKind } from "../core/models";

/** A tile the user picked in the Add-a-tile palette. */
export interface TileChoice {
  kind: TileKind;
  refId?: string;
  title?: string;
  config?: TileConfig;
}

/**
 * Open-state for the Add-a-tile palette, held at the ROOT so the palette is
 * rendered by `app-shell` instead of by the board.
 *
 * WHY THE SHELL AND NOT THE BOARD — five failed fixes bought this. Every earlier
 * attempt kept the palette inside `app-dashboard-view`'s own subtree and then
 * relied on ONE mechanism to lift it back out to the viewport: first a
 * `position: fixed` box with a percentage transform, then a teleport to `<body>`,
 * then the browser's TOP LAYER via `<dialog>.showModal()`. Each of those is a
 * thing an engine can implement differently, each worked in Chromium and in
 * Playwright's WebKit, and each still failed in the packaged WKWebView.
 *
 * Every overlay in Murmur that has never had this class of bug — quick-search
 * (⌘K), the document preview, the reminder composer, the lock×shares dialog — is
 * a plain `position: fixed; inset: 0` box rendered by `app-shell`, whose only
 * ancestors are `<body>` and `<html>`. Nothing a feature view does to
 * positioning, stacking, compositing or scrolling can reach it, because it is not
 * in that subtree at all. This service puts the palette on exactly that footing:
 * no `<dialog>`, no `showModal()`, no `:modal`, no top layer, no teleport, and no
 * imperative reveal that can throw — a template `@if` and CSS, which is the
 * smallest mechanism that can possibly work.
 *
 * `e2e/dashboards/dashboards-context.spec.ts` pins it: with the board's subtree
 * made a fixed-positioning containing block AND `showModal()` refused, the
 * palette must still land on screen. That test FAILS against every earlier shape.
 */
@Injectable({ providedIn: "root" })
export class TilePaletteService {
  private readonly _open = signal(false);
  readonly open = this._open.asReadonly();

  /** Resolver for the in-flight `request()`, if any. */
  private pending: ((choice: TileChoice | null) => void) | null = null;

  /**
   * Open the palette; resolves with the tile the user picked, or `null` if they
   * dismissed it. A second `request()` supersedes the first — the earlier promise
   * resolves `null` rather than being left hanging forever.
   */
  request(): Promise<TileChoice | null> {
    this.settle(null);
    this._open.set(true);
    return new Promise<TileChoice | null>((resolve) => {
      this.pending = resolve;
    });
  }

  choose(choice: TileChoice): void {
    this._open.set(false);
    this.settle(choice);
  }

  dismiss(): void {
    this._open.set(false);
    this.settle(null);
  }

  private settle(choice: TileChoice | null): void {
    const resolve = this.pending;
    this.pending = null;
    resolve?.(choice);
  }
}
