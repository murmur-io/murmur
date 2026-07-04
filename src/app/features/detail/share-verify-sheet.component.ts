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
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  template: `
    <div class="scrim">
      <div
        #panel
        class="sheet"
        [class.sheet--danger]="mode() === 'key-changed'"
        role="dialog"
        aria-modal="true"
        [attr.aria-label]="
          mode() === 'key-changed'
            ? 'Security key changed — re-verify before sharing'
            : 'Verify recipient before sharing'
        "
        tabindex="-1"
        (keydown.escape)="cancelled.emit()"
      >
        @if (mode() === "key-changed") {
          <h3 class="sheet-title sheet-title--warn">
            <span aria-hidden="true">⚠️</span> Security key changed
          </h3>
          <p class="sheet-copy">
            <strong>{{ email() }}</strong
            >'s security key <strong>changed</strong> since you last shared. This
            can be normal (they reset their account) or a sign of tampering.
            Re-verify the safety words out of band before sharing.
          </p>
        } @else {
          <h3 class="sheet-title">Verify it's really them</h3>
          <p class="sheet-copy">
            First time sharing with <strong>{{ email() }}</strong
            >. To be sure it's really them, compare these safety words out of
            band (a call, or in person).
          </p>
        }

        @if (words().length > 1) {
          <div class="fingerprint" role="group" aria-label="Safety words">
            @for (w of words(); track $index) {
              <span class="fp-word">{{ w }}</span>
            }
          </div>
        } @else {
          <p
            class="fingerprint fingerprint--mono"
            role="group"
            aria-label="Safety words"
          >
            {{ fingerprint() }}
          </p>
        }

        @if (error(); as err) {
          <p class="sheet-error" role="alert">{{ err }}</p>
        }

        <div class="sheet-actions">
          <button
            type="button"
            class="btn btn-ghost"
            (click)="cancelled.emit()"
            [disabled]="busy()"
          >
            Cancel
          </button>
          @if (mode() === "key-changed") {
            <button
              type="button"
              class="btn btn-danger"
              (click)="confirm.emit()"
              [disabled]="busy()"
            >
              {{ busy() ? "Sending…" : "I re-verified — send anyway" }}
            </button>
          } @else {
            <button
              type="button"
              class="btn btn-primary"
              (click)="confirm.emit()"
              [disabled]="busy()"
            >
              {{ busy() ? "Sending…" : "Verify & send" }}
            </button>
          }
        </div>
      </div>
    </div>
  `,
  styles: [
    `
      .scrim {
        position: fixed;
        inset: 0;
        z-index: 100;
        display: flex;
        align-items: center;
        justify-content: center;
        padding: var(--space-5);
        background: rgba(0, 0, 0, 0.5);
      }
      /* Floating overlay → OPAQUE (trap T3): never the frosted .card, or the
         meeting note behind it bleeds through. */
      .sheet {
        width: 100%;
        max-width: 30rem;
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
        padding: var(--space-5);
        background: var(--surface-overlay);
        border: 1px solid var(--border-strong);
        border-radius: var(--radius-lg);
        box-shadow: var(--shadow-lg);
        -webkit-backdrop-filter: none;
        backdrop-filter: none;
        animation: sheet-in var(--transition-fast);
      }
      .sheet:focus {
        outline: none;
      }
      .sheet--danger {
        border-color: var(--warning);
        box-shadow: var(--shadow-lg), 0 0 0 4px var(--warning-soft);
      }
      @keyframes sheet-in {
        from {
          opacity: 0;
          transform: translateY(8px) scale(0.98);
        }
        to {
          opacity: 1;
          transform: none;
        }
      }
      @media (prefers-reduced-motion: reduce) {
        .sheet {
          animation: none;
        }
      }

      .sheet-title {
        margin: 0;
        font-size: 1.05rem;
        font-weight: 650;
        color: var(--text-primary);
      }
      .sheet-title--warn {
        color: var(--warning);
      }
      .sheet-copy {
        margin: 0;
        color: var(--text-secondary);
        font-size: 0.9rem;
        line-height: 1.6;
      }
      .sheet-copy strong {
        color: var(--text-primary);
      }

      /* The safety words — prominent, monospace, grouped for a read-aloud. */
      .fingerprint {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-2);
        padding: var(--space-4);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
      }
      .fp-word {
        padding: var(--space-1) var(--space-3);
        border-radius: var(--radius-sm);
        background: var(--surface-raised);
        border: 1px solid var(--border-subtle);
        color: var(--text-primary);
        font-family: var(--font-mono);
        font-size: 0.95rem;
        letter-spacing: 0.02em;
        user-select: text;
        -webkit-user-select: text;
      }
      .fingerprint--mono {
        display: block;
        color: var(--text-primary);
        font-family: var(--font-mono);
        font-size: 1rem;
        letter-spacing: 0.06em;
        line-height: 1.6;
        overflow-wrap: anywhere;
        user-select: text;
        -webkit-user-select: text;
      }

      .sheet-error {
        margin: 0;
        color: var(--danger);
        font-size: 0.85rem;
        line-height: 1.5;
      }

      .sheet-actions {
        display: flex;
        justify-content: flex-end;
        gap: var(--space-2);
        flex-wrap: wrap;
      }
      .sheet-actions .btn {
        flex: none;
      }
    `,
  ],
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
