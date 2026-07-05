import { ChangeDetectionStrategy, Component, inject } from "@angular/core";
import { ReactiveFormsModule } from "@angular/forms";
import { SettingsStore } from "../../../settings.store";

/**
 * AI & Models → "On-device intelligence" block (Task 5).
 *
 * Extracted verbatim from AiDefaultsBlockComponent as a standalone card.
 * Owns the always-on-device honesty badges (Embeddings / Name redaction /
 * Transcription), the semantic-search toggle, the embedding-model download
 * flow, and the re-index controls.
 *
 * All work is on-device — no cloud calls, no consent requirement.
 */
@Component({
  selector: "app-on-device-intelligence-block",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
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
