import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  signal,
} from "@angular/core";
import { IpcService } from "../../../core/ipc.service";
import type { DigestResult } from "../../../core/models";
import { MarkdownComponent } from "../../../shared/markdown/markdown.component";

/** The selectable digest ranges, in days. */
const RANGES = [7, 30] as const;
type Range = (typeof RANGES)[number];

/**
 * "Weekly digest" — a one-shot synthesis of the user's recent meetings over a
 * chosen window (7 or 30 days), produced by {@link IpcService.generateDigest},
 * which also writes the digest into the vault's Digests/ folder.
 *
 * Presentational sibling of the analytics dashboard cards: the parent owns the
 * page; this component owns only the range picker + the generated result.
 *
 * Lives in its own file so its inline styles get their own per-component
 * `anyComponentStyle` budget (the analytics component's styles are near the cap).
 *
 * The returned markdown is rendered as PLAIN TEXT with `white-space: pre-wrap`
 * (no markdown lib, no innerHTML/DomSanitizer) — the model's line breaks +
 * spacing are preserved verbatim and safely.
 */
@Component({
  selector: "app-weekly-digest",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MarkdownComponent],
  templateUrl: "./weekly-digest.component.html",
  styleUrl: "./weekly-digest.component.scss",
})
export class WeeklyDigestComponent {
  private readonly ipc = inject(IpcService);

  /** Selectable ranges (days). */
  protected readonly ranges = RANGES;

  /** Currently-selected range, in days. */
  readonly range = signal<Range>(7);
  /** True while {@link IpcService.generateDigest} is in flight. */
  readonly pending = signal(false);
  /** The latest generated digest; null until one is produced. */
  readonly result = signal<DigestResult | null>(null);
  /** Inline error message; null when clear. */
  readonly error = signal<string | null>(null);

  /** The vault path the digest was written to, if any. */
  readonly savedPath = computed(() => this.result()?.exportedPath ?? null);

  /** Switch the active range (ignored while a generation is in flight). */
  setRange(r: Range): void {
    if (this.pending()) {
      return;
    }
    this.range.set(r);
  }

  /**
   * Generate a digest over the selected window. Awaits the one-shot IPC call
   * (no subscribe), surfaces an inline error on failure, and replaces any prior
   * result on success.
   */
  async generate(): Promise<void> {
    if (this.pending()) {
      return;
    }
    this.pending.set(true);
    this.error.set(null);
    try {
      this.result.set(await this.ipc.generateDigest(this.range()));
    } catch (e) {
      this.error.set("Couldn’t generate the digest: " + String(e));
    } finally {
      this.pending.set(false);
    }
  }
}
