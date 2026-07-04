import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  signal,
} from "@angular/core";
import { SettingsStore } from "../../settings.store";
import { LocalModelsListComponent } from "./local-models-list.component";

/**
 * Engines → "Murmur Brain" card: the BUILT-IN on-device engine (managed GGUF
 * downloads, light/heavy classes). Rendered first in the "On this Mac" group
 * so the built-in brain and Ollama (an external local server) stop being
 * conflated. The Configure disclosure hosts the shared GGUF registry
 * (LocalModelsListComponent — moved here from the role rows). In-flow
 * disclosure, not an overlay (T3).
 */
@Component({
  selector: "app-brain-engine-card",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [LocalModelsListComponent],
  template: `
    <div class="brain-card">
      <div class="brain-row">
        <div class="brain-main">
          <span class="brain-name">On this Mac — built-in models</span>
          @if (ready()) {
            <span class="pill is-success">
              <span class="pill-dot"></span>
              Ready
            </span>
          } @else {
            <span class="pill">
              <span class="pill-dot"></span>
              No model downloaded
            </span>
          }
        </div>
        <button
          type="button"
          class="btn btn-sm"
          (click)="toggle()"
          [attr.aria-expanded]="expanded()"
        >
          Configure
          <svg
            class="brain-chevron"
            [class.is-open]="expanded()"
            viewBox="0 0 16 16"
            width="12"
            height="12"
            fill="none"
            stroke="currentColor"
            stroke-width="1.8"
            stroke-linecap="round"
            stroke-linejoin="round"
            aria-hidden="true"
          >
            <path d="M4 6.5 8 10.5 12 6.5" />
          </svg>
        </button>
      </div>
      <span class="brain-privacy text-muted">
        Built into Murmur — managed models, nothing leaves this Mac. Powers
        Realtime reactions and the Fully local posture.
      </span>
      @if (expanded()) {
        <div class="brain-config">
          <app-local-models-list />
        </div>
      }
    </div>
  `,
  styles: [
    `
      :host {
        display: contents;
      }
      .brain-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        padding: var(--space-3) var(--space-4);
        border: 1px solid var(--border-subtle);
        border-radius: var(--radius-md);
        background: var(--surface-input);
      }
      .brain-row {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-3);
      }
      .brain-main {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        min-width: 0;
      }
      .brain-name {
        font-size: 0.9375rem;
        font-weight: 600;
        color: var(--text-primary);
      }
      .brain-privacy {
        font-size: 0.8125rem;
      }
      .brain-chevron {
        margin-left: var(--space-1);
        transition: transform var(--transition);
      }
      .brain-chevron.is-open {
        transform: rotate(180deg);
      }
      .brain-config {
        padding-top: var(--space-2);
        border-top: 1px solid var(--border-subtle);
      }
    `,
  ],
})
export class BrainEngineCardComponent {
  private readonly store = inject(SettingsStore);

  /** Whether the Configure disclosure is open. */
  readonly expanded = signal(false);

  /** Ready = at least one registry GGUF is on disk. */
  readonly ready = computed(() =>
    this.store.brainModels().some((m) => m.downloaded),
  );

  toggle(): void {
    this.expanded.update((v) => !v);
  }
}
