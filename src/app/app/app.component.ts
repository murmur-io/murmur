import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  afterNextRender,
  effect,
  inject,
} from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import { NavigationEnd, Router, RouterOutlet } from "@angular/router";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { filter, map } from "rxjs";
import { IpcService } from "../core/ipc.service";
import { NavHistoryService } from "../core/nav-history.service";
import { FoldersService } from "../services/folders.service";
import { ChromeService } from "../services/chrome.service";
import { GlassService } from "../services/glass.service";
import { ScreenShareService } from "../services/screen-share.service";
import { ThemeService } from "../services/theme.service";
import { UpdateService } from "../services/update.service";

@Component({
  selector: "app-root",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [RouterOutlet],
  // AppComponent is the bootstrap root, hosted by the STATIC <app-root> in
  // index.html. It renders ONLY <router-outlet>: both the chrome (AppShellComponent
  // as a layout route) and the pages are mounted by the ROUTER. That is
  // deliberate — see `_revealWindow` for the WKWebView FOUC rationale (anything
  // rendered directly in this static-host template is not style-resolved).
  templateUrl: "./app.component.html",
  styleUrl: "./app.component.scss",
})
export class AppComponent implements OnInit {
  private readonly router = inject(Router);
  private readonly ipc = inject(IpcService);
  private readonly folders = inject(FoldersService);
  private readonly screenShare = inject(ScreenShareService);
  // Injected at bootstrap so the theme is applied (in the service constructor)
  // before the main window is revealed — no flash of the wrong theme.
  private readonly theme = inject(ThemeService);
  // Same for the Liquid Glass level (--glass-user-alpha on <html>).
  private readonly glass = inject(GlassService);
  // Same for the accent palette (data-accent on <html>).
  private readonly chrome = inject(ChromeService);
  private readonly updates = inject(UpdateService);
  // Injected at bootstrap purely so it starts observing router events from the
  // FIRST navigation — its "last non-settings route" (used by the settings
  // drill-down "← Murmur" back button) must be recorded before the user reaches
  // settings, so it cannot wait for lazy construction inside SettingsComponent.
  private readonly navHistory = inject(NavHistoryService);

  /** True in the floating-bar window (route /bar) — the app chrome is hidden there. */
  readonly isBar = toSignal(
    this.router.events.pipe(
      filter((e): e is NavigationEnd => e instanceof NavigationEnd),
      map(() => this.router.url.startsWith("/bar")),
    ),
    { initialValue: location.pathname.startsWith("/bar") },
  );

  /** Make the bar window's document transparent (no aurora/grain behind the pill). */
  private readonly _bodyClass = effect(() => {
    document.body.classList.toggle("bar-shell", this.isBar());
  });

  /**
   * WKWebView FOUC fix — hide-until-ready (MAIN window only).
   *
   * In Tauri's WKWebView the app shell rendered as raw UNSTYLED HTML on most
   * cold launches. Root cause (proven by on-device DOM probing): elements
   * rendered in the bootstrap-root template — whose host is the STATIC
   * <app-root> in index.html — are not style-resolved by this webview, even with
   * a matching encapsulation id (or a fully global rule) present in a <style>.
   * Components mounted by the ROUTER (ViewContainerRef.createComponent) ARE
   * resolved. So the chrome lives in AppShellComponent, mounted as a LAYOUT
   * ROUTE (app.routes.ts), and AppComponent renders only <router-outlet>.
   *
   * The window starts HIDDEN (`tauri.conf.json` → `"visible": false`) and is
   * revealed here after the first render so the user never sees a blank/unstyled
   * frame. The window MUST NEVER stay hidden: the reveal is wrapped in
   * try/finally with the `finally` always calling `show()`.
   */
  private readonly _revealWindow = afterNextRender(() => {
    void this.revealMainWindow();
  });

  private async revealMainWindow(): Promise<void> {
    if (this.isBar()) return; // bar is shown on toggle from Rust
    let win: ReturnType<typeof getCurrentWindow> | null = null;
    try {
      win = getCurrentWindow();
      await win.show();
      await win.setFocus();
    } catch {
      // Window API unavailable (non-Tauri / dev browser) or a step threw — the
      // finally below guarantees the window is never left hidden.
    } finally {
      // The window must NEVER stay hidden, whatever happened above.
      try {
        await win?.show();
      } catch {
        // Nothing more we can do; in a real browser there is no window to show.
      }
    }
  }

  /**
   * First-run gate (MAIN window only). On startup, if the user hasn't completed
   * onboarding, send them to the wizard. The floating-bar window is never gated —
   * it just mirrors recording state and must stay chromeless.
   *
   * The main window also arms the screen-share privacy guard and primes the
   * folder tree (so the "N unlocked" pill + locked-meeting masking have state).
   * The bar window does neither — it stays chromeless and side-effect-free.
   */
  async ngOnInit(): Promise<void> {
    // Re-assert the chosen theme + glass level on startup (idempotent) for both windows.
    this.theme.ensureApplied();
    this.glass.ensureApplied();

    if (this.isBar()) return;

    // Arm the screen-share guard + prime the folder store (main window only).
    // Best-effort and non-blocking: a folders/listen failure must not trap the
    // user on a blank app, so each is fire-and-forget with its own catch.
    void this.screenShare.init();
    void this.folders.load();
    // Best-effort GitHub-release update check — fire-and-forget, non-blocking.
    // The service swallows any failure (a background check must never nag), so
    // this never traps the user; a found update surfaces as a sticky toast.
    void this.updates.checkOnStartup();

    try {
      const cfg = await this.ipc.getConfig();
      // Core onboarding STRICTLY precedes the optional sharing gate — a
      // brand-new user goes to /onboarding first (its finish() then hands off
      // to /welcome when the sharing gate is still open).
      if (!cfg.onboarded) {
        await this.router.navigateByUrl("/onboarding");
        return;
      }
      // First-run SHARING gate (single source of truth:
      // `!sharingChoiceMade && !accountStatus.loggedIn`). A returning logged-in
      // user, a user who picked local, or one who logged in then out (choice
      // made) never sees it again — "never nag".
      if (!cfg.sharingChoiceMade) {
        const st = await this.ipc.accountStatus().catch(() => null);
        if (!st?.loggedIn) {
          await this.router.navigateByUrl("/welcome");
          return;
        }
      }
    } catch {
      // Config unavailable — don't trap the user; the app loads normally.
    }
  }
}
