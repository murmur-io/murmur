import { Injectable, signal } from "@angular/core";

/** The three theme choices the user can pick in Settings. `system` (default) follows macOS. */
export type ThemeMode = "light" | "dark" | "system";

const STORAGE_KEY = "murmur-theme";
const VALID: readonly ThemeMode[] = ["light", "dark", "system"];

/**
 * Owns the app's color theme. The mode is persisted in localStorage and applied
 * as a `data-theme` attribute on <html>; the actual light/dark tokens live in
 * styles.css (and, for pre-paint correctness, the critical block in index.html).
 *
 * Applied in the constructor so the theme is live the moment the service is first
 * injected (AppComponent injects it during bootstrap, BEFORE the main window is
 * revealed in `afterNextRender` — see app.component.ts), which means no flash of
 * the wrong theme on launch. `system` mode is resolved purely in CSS via a
 * `prefers-color-scheme` media query, so OS appearance changes are picked up live
 * with no JS listener.
 */
@Injectable({ providedIn: "root" })
export class ThemeService {
  private readonly _mode = signal<ThemeMode>(this.read());
  /** The user's chosen theme: `light`, `dark`, or `system` (default). */
  readonly mode = this._mode.asReadonly();

  constructor() {
    this.apply(this._mode());
  }

  /** Set and persist the theme; applies immediately (no save button needed). */
  setMode(mode: ThemeMode): void {
    this._mode.set(mode);
    try {
      localStorage.setItem(STORAGE_KEY, mode);
    } catch {
      // localStorage unavailable (private mode / disabled) — the in-memory
      // signal + attribute still work for this session.
    }
    this.apply(mode);
  }

  /** Re-assert the attribute (idempotent). Called once at app start. */
  ensureApplied(): void {
    this.apply(this._mode());
  }

  private read(): ThemeMode {
    try {
      const v = localStorage.getItem(STORAGE_KEY);
      if (v && VALID.includes(v as ThemeMode)) return v as ThemeMode;
    } catch {
      // ignore — fall through to the default
    }
    return "system";
  }

  private apply(mode: ThemeMode): void {
    document.documentElement.setAttribute("data-theme", mode);
  }
}
