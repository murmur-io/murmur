import { Injectable, signal } from "@angular/core";

const STORAGE_KEY = "murmur.developerMode";

/**
 * Developer mode — OFF by default, and off is the only state a normal user ever
 * sees. On, it reveals the diagnostics surface: the "Developer mode" group in
 * the shell sidebar and the tools under it (today: Logs).
 *
 * Mirrors ThemeService / GlassService: a root singleton whose state is a signal
 * persisted in localStorage, so the shell (always mounted) and the Settings
 * section (mounted only on /settings) read the SAME value and the sidebar
 * reacts the moment the toggle flips — no IPC round-trip, no reload.
 *
 * Deliberately NOT part of `AppConfig`: this decides what the UI OFFERS, it
 * never unlocks anything the backend would otherwise refuse. Every command it
 * surfaces is safe to call with the toggle off (see `commands/devtools.rs`),
 * which is what keeps it a preference rather than a security boundary — a
 * toggle that gated real capability would have to live in the backend.
 */
@Injectable({ providedIn: "root" })
export class DeveloperModeService {
  private readonly _enabled = signal<boolean>(this.read());

  /** Whether the developer tools are revealed. Default `false`. */
  readonly enabled = this._enabled.asReadonly();

  /** Turn developer mode on/off; persists immediately (no save button). */
  setEnabled(enabled: boolean): void {
    this._enabled.set(enabled);
    try {
      localStorage.setItem(STORAGE_KEY, String(enabled));
    } catch {
      // Storage unavailable — the choice still holds for this session.
    }
  }

  /** Flip the toggle. */
  toggle(): void {
    this.setEnabled(!this._enabled());
  }

  private read(): boolean {
    try {
      // Anything other than the exact string "true" is off, so a corrupted or
      // half-written value can never turn developer tools on by accident.
      return localStorage.getItem(STORAGE_KEY) === "true";
    } catch {
      return false;
    }
  }
}
