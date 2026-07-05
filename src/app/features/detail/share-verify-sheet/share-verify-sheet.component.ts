import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  afterNextRender,
  computed,
  input,
  output,
  viewChild,
} from "@angular/core";

/**
 * The verification mode for {@link ShareVerifySheetComponent}:
 *  - `"first-contact"` — first time sharing with this recipient. Show the safety
 *    words for an out-of-band compare, then confirm to send.
 *  - `"key-changed"`   — their key CHANGED since the last share. BLOCKING tone:
 *    the ONLY way forward is an explicit "I re-verified" secondary action.
 */
export type ShareVerifyMode = "first-contact" | "key-changed";

/**
 * M5-CLIENT — the safety-word verification sheet for Murmur↔Murmur sharing.
 *
 * A FLOATING overlay (trap T3): it renders OVER the meeting detail, so the panel
 * is OPAQUE `var(--surface-overlay)` + `backdrop-filter: none` + a strong border
 * + `--shadow-lg` — never the frosted `.card` (which would bleed the note through).
 *
 * Presentational: the parent owns the async `shareNoteToUser` call, `busy`, and
 * any thrown `error`; this sheet only renders the fingerprint + emits
 * `confirm` / `cancelled`. The recipient's safety-word fingerprint is shown
 * prominently (monospace word chips, grouped for an out-of-band read-aloud).
 */
@Component({
  selector: "app-share-verify-sheet",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./share-verify-sheet.component.html",
  styleUrl: "./share-verify-sheet.component.scss",
})
export class ShareVerifySheetComponent {
  /** The recipient email being verified (shown in the copy). */
  readonly email = input.required<string>();
  /** The recipient's safety-word fingerprint (shown prominently for compare). */
  readonly fingerprint = input.required<string>();
  /** Which verification tone to render — first contact vs a changed key. */
  readonly mode = input.required<ShareVerifyMode>();
  /** True while the parent's `shareNoteToUser` call is in flight. */
  readonly busy = input(false);
  /** A thrown-share error to surface inline (e.g. a server-side key BLOCK). */
  readonly error = input<string | null>(null);

  /** The user confirmed (verified out of band) — proceed with the share. */
  readonly confirm = output<void>();
  /** The user dismissed the sheet — share nothing. */
  readonly cancelled = output<void>();

  private readonly panel = viewChild<ElementRef<HTMLDivElement>>("panel");

  /** The fingerprint split into safety words for chip rendering. */
  readonly words = computed(() =>
    this.fingerprint()
      .trim()
      .split(/\s+/)
      .filter((w) => w.length > 0),
  );

  constructor() {
    // Land focus in the dialog so Escape works + screen readers announce it.
    // Field-initialiser context → no injector needed.
    afterNextRender(() => this.panel()?.nativeElement.focus());
  }
}
