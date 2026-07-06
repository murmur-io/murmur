import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  input,
  signal,
} from "@angular/core";
import { SettingsStore } from "../../../settings.store";
import { ModelEffortPickerComponent } from "../model-effort-picker/model-effort-picker.component";

/**
 * Engines → "Murmur Brain" card: the BUILT-IN on-device engine (managed GGUF
 * downloads, light/heavy classes). Rendered first in the "On this Mac" group
 * so the built-in brain and Ollama (an external local server) stop being
 * conflated. The Configure disclosure hosts the Claude-style on-device model
 * picker (ModelEffortPickerComponent — effort slider + language toggle). In-flow
 * disclosure, not an overlay (T3).
 */
@Component({
  selector: "app-brain-engine-card",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ModelEffortPickerComponent],
  templateUrl: "./brain-engine-card.component.html",
  styleUrl: "./brain-engine-card.component.scss",
})
export class BrainEngineCardComponent {
  private readonly store = inject(SettingsStore);

  /** True when the current posture actively routes work to the built-in engine. */
  readonly inUse = input<boolean>(false);

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
