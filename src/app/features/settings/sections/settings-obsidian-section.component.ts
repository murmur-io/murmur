import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { SettingsStore } from "../settings.store";

/**
 * Settings → obsidian section (Stage-1 split): the `@case ("obsidian")` block of the
 * former settings.component.ts monolith, moved VERBATIM. State/actions live in
 * the shell-provided SettingsStore so section switches never drop them.
 */
@Component({
  selector: "app-settings-obsidian-section",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="section-stack">
              <div class="card obsidian-card">
                <div class="obsidian-head">
                  <span class="obsidian-mark" aria-hidden="true">
                    <svg
                      viewBox="0 0 24 24"
                      width="30"
                      height="30"
                      fill="none"
                      role="img"
                      aria-label="Obsidian"
                    >
                      <defs>
                        <linearGradient
                          id="obs-face-a"
                          x1="3"
                          y1="2"
                          x2="20"
                          y2="22"
                          gradientUnits="userSpaceOnUse"
                        >
                          <stop stop-color="var(--accent-hover)" />
                          <stop offset="1" stop-color="var(--accent-active)" />
                        </linearGradient>
                        <linearGradient
                          id="obs-face-b"
                          x1="11"
                          y1="2"
                          x2="22"
                          y2="20"
                          gradientUnits="userSpaceOnUse"
                        >
                          <stop stop-color="#b79bff" />
                          <stop offset="1" stop-color="var(--accent)" />
                        </linearGradient>
                      </defs>
                      <path
                        d="M9.4 2.3 4 8.1l3.1 10.2L11 22l-1.3-9.5L9.4 2.3Z"
                        fill="url(#obs-face-a)"
                      />
                      <path
                        d="M9.4 2.3 9.7 12.5 11 22l5.6-4.2L20 8.4 14.6 2.6 9.4 2.3Z"
                        fill="url(#obs-face-b)"
                      />
                      <path
                        d="M9.7 12.5 9.4 2.3l5.2.3-.7 8 .9 1.9-5.1.0Z"
                        fill="#ffffff"
                        fill-opacity="0.16"
                      />
                    </svg>
                  </span>
                  <div class="obsidian-copy">
                    <h3>Works with Obsidian</h3>
                    <p class="text-secondary obsidian-sub">
                      Murmur saves each meeting as a Markdown note in your Obsidian
                      vault. Obsidian is <strong>optional</strong> — you can read every
                      recording, AI summary and transcript right here in Murmur (the
                      Meetings tab, with audio playback). Want the full vault
                      experience?
                    </p>
                  </div>
                </div>

                <div class="obsidian-get">
                  <span class="obsidian-get-label text-muted"
                    >Get Obsidian — it's free</span
                  >
                  <span class="obsidian-url-row">
                    <span class="obsidian-url" role="text">{{ obsidianUrl }}</span>
                    <button
                      type="button"
                      class="btn obsidian-copy-btn"
                      [class.is-copied]="urlCopied()"
                      (click)="copyObsidianUrl()"
                      [attr.aria-label]="
                        urlCopied() ? 'Copied' : 'Copy ' + obsidianUrl + ' to clipboard'
                      "
                    >
                      @if (urlCopied()) {
                        <svg
                          class="obsidian-copy-icon"
                          viewBox="0 0 16 16"
                          width="14"
                          height="14"
                          aria-hidden="true"
                        >
                          <path
                            d="M3 8.5 6.2 12 13 4.5"
                            fill="none"
                            stroke="currentColor"
                            stroke-width="1.8"
                            stroke-linecap="round"
                            stroke-linejoin="round"
                          />
                        </svg>
                        Copied
                      } @else {
                        <svg
                          class="obsidian-copy-icon"
                          viewBox="0 0 16 16"
                          width="14"
                          height="14"
                          aria-hidden="true"
                        >
                          <rect
                            x="5.5"
                            y="5.5"
                            width="8"
                            height="8"
                            rx="2"
                            fill="none"
                            stroke="currentColor"
                            stroke-width="1.5"
                          />
                          <path
                            d="M10.5 5.5V4a2 2 0 0 0-2-2H4a2 2 0 0 0-2 2v4.5a2 2 0 0 0 2 2h1.5"
                            fill="none"
                            stroke="currentColor"
                            stroke-width="1.5"
                            stroke-linecap="round"
                            stroke-linejoin="round"
                          />
                        </svg>
                        Copy
                      }
                    </button>
                  </span>
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

      /* --- Works-with-Obsidian card --- */
      .obsidian-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
      }
      .obsidian-head {
        display: flex;
        align-items: flex-start;
        gap: var(--space-4);
      }
      .obsidian-mark {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 52px;
        height: 52px;
        min-width: 52px;
        border-radius: var(--radius-md);
        background: var(--accent-soft);
        border: 1px solid var(--glass-border);
        box-shadow: var(--glass-highlight);
        /* Gentle staggered settle, in keeping with the global entrance language. */
        animation: rise 460ms var(--ease-spring) both;
        animation-delay: 80ms;
        transition:
          transform var(--transition),
          box-shadow var(--transition),
          border-color var(--transition);
      }
      .obsidian-card:hover .obsidian-mark {
        transform: translateY(-1px) rotate(-3deg);
        border-color: var(--border-strong);
        box-shadow: var(--shadow-accent), var(--glass-highlight);
      }
      .obsidian-mark svg {
        display: block;
        filter: drop-shadow(0 2px 6px rgba(110, 118, 255, 0.45));
      }
      .obsidian-copy {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        min-width: 0;
      }
      .obsidian-copy h3 {
        margin: 0;
      }
      .obsidian-sub {
        margin: 0;
        font-size: 0.9rem;
        line-height: 1.55;
      }
      .obsidian-sub strong {
        color: var(--text-primary);
        font-weight: 600;
      }

      /* The free-Obsidian call-out: a quiet inset well with copyable URL + button. */
      .obsidian-get {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
        padding: var(--space-4);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
      }
      .obsidian-get-label {
        font-size: 0.8125rem;
        font-weight: 550;
        letter-spacing: 0.01em;
        text-transform: uppercase;
      }
      .obsidian-url-row {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        flex-wrap: wrap;
      }
      .obsidian-url {
        flex: 1 1 auto;
        min-width: 0;
        padding: 0 var(--space-3);
        height: 40px;
        display: inline-flex;
        align-items: center;
        border: 1px solid var(--glass-border);
        border-radius: var(--radius-md);
        background: var(--surface-raised);
        color: var(--text-primary);
        font-family: var(--font-mono);
        font-size: 0.9375rem;
        letter-spacing: -0.01em;
        /* Selectable text — the user can also just copy it by hand. */
        user-select: text;
        -webkit-user-select: text;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }
      .obsidian-copy-btn {
        flex: none;
      }
      .obsidian-copy-btn.is-copied {
        color: var(--success);
        border-color: rgba(74, 222, 128, 0.4);
        background: var(--success-soft);
      }
      .obsidian-copy-icon {
        flex: none;
      }
    `,
  ],
})
export class SettingsObsidianSectionComponent {
  private readonly store = inject(SettingsStore);

  readonly obsidianUrl = this.store.obsidianUrl;
  readonly urlCopied = this.store.urlCopied;

  copyObsidianUrl(): void {
    void this.store.copyObsidianUrl();
  }
}
