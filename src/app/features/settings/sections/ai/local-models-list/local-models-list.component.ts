import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { SettingsStore } from "../../../settings.store";

/**
 * AI & Models → the shared "Local models" GGUF registry panel.
 *
 * Extracted from AiRoleRowsComponent (Task 3): rendered once inside `@if (anyLocal())`
 * in the role-rows block, shared by every feature role set to "Local model". Task 4
 * will re-mount this inside the forthcoming `<app-ai-advanced-block>`.
 *
 * Consumes brainModels / brainDownloadingId / brainDownloadFrac / brainPct /
 * brainModelsLoading / brainError / customGgufValue from SettingsStore, and
 * delegates downloads / selections / custom-GGUF writes back to it.
 */
@Component({
  selector: "app-local-models-list",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [],
  templateUrl: "./local-models-list.component.html",
  styleUrl: "./local-models-list.component.scss",
})
export class LocalModelsListComponent {
  private readonly store = inject(SettingsStore);

  readonly brainModels = this.store.brainModels;
  readonly brainModelsLoading = this.store.brainModelsLoading;
  readonly brainError = this.store.brainError;
  readonly brainDownloadingId = this.store.brainDownloadingId;
  readonly brainDownloadFrac = this.store.brainDownloadFrac;
  readonly brainPct = this.store.brainPct;
  readonly customGgufValue = this.store.customGgufValue;

  refreshBrainModels(): void {
    void this.store.refreshBrainModels();
  }

  useBrainModel(id: string): void {
    void this.store.useBrainModel(id);
  }

  downloadBrainModel(id: string): void {
    void this.store.downloadBrainModel(id);
  }

  setCustomGguf(v: string): void {
    this.store.setCustomGguf(v);
  }

  /** Human GB/MB size label from a byte count (binary), mirroring the Storage section. */
  sizeLabel(bytes: number): string {
    const gb = 1024 * 1024 * 1024;
    if (bytes >= gb) return (bytes / gb).toFixed(1) + " GB";
    return Math.round(bytes / (1024 * 1024)) + " MB";
  }
}
