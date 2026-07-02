import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { IpcService } from "../../../core/ipc.service";
import { UpdateService } from "../../../services/update.service";
import { SettingsStore } from "../settings.store";

/**
 * Settings → about section (Stage-1 split): the `@case ("about")` block of the
 * former settings.component.ts monolith, moved VERBATIM. State/actions live in
 * the shell-provided SettingsStore so section switches never drop them.
 */
@Component({
  selector: "app-settings-about-section",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="section-stack">
              <div class="card about-card">
                @if (appInfo(); as info) {
                  <div class="about-head">
                    <span class="about-name">{{ info.name }}</span>
                    <span class="about-version text-muted"
                      >Version {{ info.version }}</span
                    >
                  </div>
                  <p class="text-secondary about-desc">{{ info.description }}</p>
                } @else {
                  <p class="text-secondary about-desc">
                    Loading product info…
                  </p>
                }

                <div class="about-update">
                  <span class="about-section-label text-muted"
                    >Software update</span
                  >
                  <div class="about-update-row">
                    <button
                      type="button"
                      class="btn"
                      (click)="checkForUpdates()"
                      [disabled]="updateStatus() === 'checking'"
                    >
                      @if (updateStatus() === "checking") {
                        <span class="spin-ring" aria-hidden="true"></span>
                        Checking…
                      } @else {
                        Check for updates
                      }
                    </button>

                    @switch (updateStatus()) {
                      @case ("available") {
                        @if (latestUpdate(); as upd) {
                          <span class="about-update-result">
                            <span class="text-secondary"
                              >Version {{ upd.latestVersion }} is available.</span
                            >
                            <button
                              type="button"
                              class="btn btn-primary"
                              (click)="downloadUpdate(upd.releaseUrl)"
                            >
                              Download
                            </button>
                          </span>
                        }
                      }
                      @case ("upToDate") {
                        <span class="text-muted about-update-line"
                          >You're up to date.</span
                        >
                      }
                      @case ("error") {
                        <span class="text-danger about-update-line"
                          >Couldn't check for updates.</span
                        >
                      }
                    }
                  </div>
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

      /* About — product identity + manual update check. */
      .about-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .about-head {
        display: flex;
        align-items: baseline;
        gap: var(--space-3);
        flex-wrap: wrap;
      }
      .about-name {
        font-size: 1.15rem;
        font-weight: 650;
        letter-spacing: -0.01em;
      }
      .about-version {
        font-size: 0.9rem;
      }
      .about-desc {
        margin: 0;
        line-height: 1.55;
      }
      .about-update {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        padding-top: var(--space-3);
        border-top: 1px solid var(--border-subtle);
      }
      .about-section-label {
        font-size: 0.72rem;
        font-weight: 650;
        letter-spacing: 0.04em;
        text-transform: uppercase;
      }
      .about-update-row,
      .about-update-result {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex-wrap: wrap;
      }
      .about-update-line {
        font-size: 0.9rem;
      }

      /* Inline spinner on the Download button (matches the onboarding wizard). */
      .spin-ring {
        width: 15px;
        height: 15px;
        border-radius: 50%;
        border: 2px solid rgba(255, 255, 255, 0.35);
        border-top-color: var(--text-on-accent);
        animation: spin 0.8s linear infinite;
        margin-right: var(--space-2);
        vertical-align: -2px;
        display: inline-block;
      }
      @keyframes spin {
        to {
          transform: rotate(360deg);
        }
      }
      @media (prefers-reduced-motion: reduce) {
        .spin-ring {
          animation: none;
        }
      }
    `,
  ],
})
export class SettingsAboutSectionComponent {
  private readonly store = inject(SettingsStore);
  private readonly ipc = inject(IpcService);
  private readonly updates = inject(UpdateService);

  /** Static product identity (name/version/description), loaded by the store. */
  readonly appInfo = this.store.appInfo;

  /** Update-check lifecycle from the shared service (drives the button + result line). */
  readonly updateStatus = this.updates.status;

  /** The most-recent update-check result (the Download button reads its releaseUrl). */
  readonly latestUpdate = this.updates.latest;

  /** Run the shared manual update check (surfaces both outcomes as toasts). */
  checkForUpdates(): void {
    void this.updates.checkManually();
  }

  /** Open the GitHub release page for an available update. */
  downloadUpdate(url: string): void {
    void this.ipc.openReleasePage(url);
  }
}
