import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  computed,
  inject,
} from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { MurToggleComponent } from "../../../../design-system/toggle/toggle.component";
import { SettingsStore } from "../../settings.store";

/** Settings → Storage: recordings location + usage, the GB cap, opt-in auto-prune, and a
 *  manual "Free up space". Notes/transcripts are never deleted — only audio. */
@Component({
  selector: "app-settings-storage-section",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    MurToggleComponent,ReactiveFormsModule],
  templateUrl: "./settings-storage-section.component.html",
  styleUrl: "./settings-storage-section.component.scss",
})
export class SettingsStorageSectionComponent implements OnInit {
  private readonly store = inject(SettingsStore);
  readonly form = this.store.form;
  readonly report = this.store.storageReport;
  readonly storageBusy = this.store.storageBusy;
  readonly lastFreed = this.store.lastFreed;

  /** % fill of the cap bar (0..100, clamped). */
  readonly pct = computed(() => {
    const r = this.report();
    if (!r || r.limitBytes == null || r.limitBytes === 0) return 0;
    return Math.min(100, Math.round((r.usedBytes / r.limitBytes) * 100));
  });
  /** Bar color state by fill. */
  readonly barState = computed(() => {
    const p = this.pct();
    return p >= 95 ? "red" : p >= 75 ? "amber" : "ok";
  });
  /** True when a storage cap is configured (drives the "Free up space" enablement). */
  readonly hasCap = computed(() => {
    const r = this.report();
    return !!r && r.limitBytes !== null;
  });

  ngOnInit(): void {
    // Ensure the report is fresh when the section mounts (load() already fetched it once).
    void this.store.loadStorageReport();
  }

  /** Human MB/GB label (binary). */
  mb(bytes: number): string {
    if (bytes >= 1024 * 1024 * 1024) return (bytes / (1024 * 1024 * 1024)).toFixed(2) + " GB";
    return Math.round(bytes / (1024 * 1024)) + " MB";
  }

  reveal(): void {
    this.store.revealAudioDir();
  }

  onFreeUp(): void {
    if (!confirm("Delete oldest recordings' audio to free up space? Notes are kept. This can't be undone.")) return;
    void this.store.freeUpSpace();
  }
}
