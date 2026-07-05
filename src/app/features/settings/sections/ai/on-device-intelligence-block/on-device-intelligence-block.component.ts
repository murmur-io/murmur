import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../../../settings.store";
import { MurToggleComponent } from "../../../../../design-system/toggle/toggle.component";

/**
 * AI & Models → "Search index" block.
 *
 * Owns the semantic-search toggle, the embedding-model download flow, and the
 * re-index controls. The always-on-device honesty rows (Embeddings / Name
 * redaction / Transcription) now live in the "What runs where" map card, so
 * the badges that used to sit here were removed.
 *
 * All work is on-device — no cloud calls, no consent requirement.
 */
@Component({
  selector: "app-on-device-intelligence-block",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    MurToggleComponent,ReactiveFormsModule],
  templateUrl: "./on-device-intelligence-block.component.html",
  styleUrl: "./on-device-intelligence-block.component.scss",
})
export class OnDeviceIntelligenceBlockComponent {
  private readonly store = inject(SettingsStore);

  readonly form = this.store.form;
  readonly embedModelPresent = this.store.embedModelPresent;
  readonly downloadingEmbedModel = this.store.downloadingEmbedModel;
  readonly embedDownloadFrac = this.store.embedDownloadFrac;
  readonly embedPct = this.store.embedPct;
  readonly embedDownloadError = this.store.embedDownloadError;
  readonly reindexing = this.store.reindexing;
  readonly reindexFrac = this.store.reindexFrac;
  readonly reindexPct = this.store.reindexPct;
  readonly reindexResult = this.store.reindexResult;
  readonly reindexError = this.store.reindexError;

  downloadEmbedModel(): void {
    void this.store.downloadEmbedModel();
  }

  reindexEmbeddings(): void {
    void this.store.reindexEmbeddings();
  }
}
