import { Injectable, signal } from "@angular/core";

const STORAGE_KEY = "murmur-glass";
/** Full glass by default — the prototype's Liquid Glass look. */
const DEFAULT_LEVEL = 100;

/**
 * PROTOTYPE (Apple TV shell) — owns the Liquid Glass intensity: how translucent
 * the chrome panels (sidebar / pill bar / drill-down rails) are, 0 (opaque) to
 * 100 (full glass). Mirrors ThemeService: persisted in localStorage, applied as
 * the `--glass-user-alpha` custom property on <html>; the actual glass layers
 * (veil, lensing rim, illumination) live in styles.css and read that token.
 *
 * A stylesheet `prefers-reduced-transparency` override in styles.css wins over
 * this inline property (it is `!important`), matching the HIG's accessibility
 * behavior for Liquid Glass.
 */
@Injectable({ providedIn: "root" })
export class GlassService {
  private readonly _level = signal<number>(this.read());
  /** Glass transparency 0–100 (100 = full Liquid Glass, 0 = opaque panels). */
  readonly level = this._level.asReadonly();

  constructor() {
    this.apply(this._level());
  }

  /** Re-assert the property (idempotent). Called once at app start. */
  ensureApplied(): void {
    this.apply(this._level());
  }

  /** Set and persist the glass level; applies immediately (auto-saved). */
  setLevel(level: number): void {
    const v = Math.min(100, Math.max(0, Math.round(level)));
    this._level.set(v);
    try {
      localStorage.setItem(STORAGE_KEY, String(v));
    } catch {
      // localStorage unavailable — the in-memory value still applies this session.
    }
    this.apply(v);
  }

  private read(): number {
    try {
      const raw = localStorage.getItem(STORAGE_KEY);
      if (raw === null || raw.trim() === "") return DEFAULT_LEVEL; // Number(null)=0 — never mistake "unset" for opaque
      const v = Number(raw);
      if (Number.isFinite(v) && v >= 0 && v <= 100) return Math.round(v);
    } catch {
      // ignore — fall through to the default
    }
    return DEFAULT_LEVEL;
  }

  private apply(level: number): void {
    document.documentElement.style.setProperty(
      "--glass-user-alpha",
      String(level / 100),
    );
  }
}
