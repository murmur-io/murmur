import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ThemeService, type ThemeMode } from "../../../services/theme.service";

/**
 * Settings → appearance section (Stage-1 split): the `@case ("appearance")` block of the
 * former settings.component.ts monolith, moved VERBATIM. State/actions live in
 * the shell-provided SettingsStore so section switches never drop them.
 */
@Component({
  selector: "app-settings-appearance-section",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="section-stack">
              <div class="card appearance-card">
                <div class="appearance-copy">
                  <h3>Appearance</h3>
                  <p class="text-secondary">
                    Choose how Murmur looks. <b>System</b> follows your macOS
                    Light/Dark setting automatically.
                  </p>
                </div>
                <div class="theme-seg" role="group" aria-label="Theme">
                  <button
                    type="button"
                    [class.active]="themeMode() === 'light'"
                    [attr.aria-pressed]="themeMode() === 'light'"
                    (click)="setTheme('light')"
                  >
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="4" /><path d="M12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4" /></svg>
                    Light
                  </button>
                  <button
                    type="button"
                    [class.active]="themeMode() === 'dark'"
                    [attr.aria-pressed]="themeMode() === 'dark'"
                    (click)="setTheme('dark')"
                  >
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z" /></svg>
                    Dark
                  </button>
                  <button
                    type="button"
                    [class.active]="themeMode() === 'system'"
                    [attr.aria-pressed]="themeMode() === 'system'"
                    (click)="setTheme('system')"
                  >
                    <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><rect x="3" y="4" width="18" height="12" rx="2" /><path d="M8 20h8M12 16v4" /></svg>
                    System
                  </button>
                </div>
              </div>
    </div>
  `,
  styles: [
    `
      /* Stage-1 split: the host stays layout-transparent so this section's
         cards remain direct flex items of the shell's .section-body (identical
         spacing to the pre-split monolith); .section-stack reproduces the
         .section-body column gap between this section's own cards. */
      :host {
        display: contents;
      }
      .section-stack {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
      }

      /* --- Appearance / theme --- */
      .appearance-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .appearance-copy {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .appearance-copy h3 {
        margin: 0;
      }
      .appearance-copy p {
        margin: 0;
        font-size: 0.875rem;
      }
      .theme-seg {
        display: inline-flex;
        gap: var(--space-1);
        padding: var(--space-1);
        width: fit-content;
        background: var(--surface-input);
        border: 1px solid var(--border);
        border-radius: var(--radius-pill);
      }
      .theme-seg button {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        padding: var(--space-2) var(--space-4);
        border: 0;
        background: transparent;
        color: var(--text-secondary);
        border-radius: var(--radius-pill);
        font: inherit;
        font-weight: 600;
        font-size: 0.875rem;
        cursor: pointer;
        transition:
          background var(--transition-fast),
          color var(--transition-fast);
      }
      .theme-seg button svg {
        width: 16px;
        height: 16px;
      }
      .theme-seg button:hover {
        color: var(--text-primary);
      }
      .theme-seg button.active {
        background: var(--accent-soft);
        color: var(--accent);
      }
    `,
  ],
})
export class SettingsAppearanceSectionComponent {
  private readonly theme = inject(ThemeService);

  /** Current theme choice (Light / Dark / System) — drives the Appearance control. */
  readonly themeMode = this.theme.mode;

  /** Apply a theme immediately (persisted in the service; no save() needed). */
  setTheme(mode: ThemeMode): void {
    this.theme.setMode(mode);
  }
}
