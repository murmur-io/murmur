import { Injectable, signal } from "@angular/core";

/** The user-selectable accent palettes (Settings → Appearance). `purple` is
 * the base :root ramp in colors.css; the others live in accents.css. */
export type AccentId =
  | "purple"
  | "blue"
  | "teal"
  | "green"
  | "orange"
  | "pink";

const ACCENT_KEY = "murmur-accent";
const VALID_ACCENTS: readonly AccentId[] = [
  "purple",
  "blue",
  "teal",
  "green",
  "orange",
  "pink",
];

/**
 * Owns visual chrome preferences (Settings → Appearance). Persisted in
 * localStorage like ThemeService — pure webview chrome state, no IPC needed.
 */
@Injectable({ providedIn: "root" })
export class ChromeService {
  private readonly _accent = signal<AccentId>(this.readAccent());
  /** The user's chosen accent palette (default `purple`). */
  readonly accent = this._accent.asReadonly();

  constructor() {
    // Applied at first injection (AppComponent, before the window is revealed)
    // so a non-default accent never flashes purple — same timing as ThemeService.
    this.applyAccent(this._accent());
  }

  /** Set and persist the accent palette; applies immediately (auto-saved). */
  setAccent(accent: AccentId): void {
    this._accent.set(accent);
    try {
      localStorage.setItem(ACCENT_KEY, accent);
    } catch {
      // localStorage unavailable — the in-memory signal still works.
    }
    this.applyAccent(accent);
  }

  private readAccent(): AccentId {
    try {
      const v = localStorage.getItem(ACCENT_KEY);
      if (v && VALID_ACCENTS.includes(v as AccentId)) return v as AccentId;
    } catch {
      // ignore — fall through to the default
    }
    return "purple";
  }

  /** Default purple = NO attribute (the base :root ramp); others stamp it. */
  private applyAccent(accent: AccentId): void {
    if (accent === "purple") {
      document.documentElement.removeAttribute("data-accent");
    } else {
      document.documentElement.setAttribute("data-accent", accent);
    }
  }
}
