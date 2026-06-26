import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  effect,
  inject,
  input,
  output,
  signal,
  viewChild,
} from "@angular/core";
import { IpcService } from "../../core/ipc.service";
import type { BriefResult, VaultSource } from "../../core/models";
import { MarkdownComponent } from "../../shared/markdown.component";
import { SourcesComponent } from "../../shared/sources.component";

/**
 * "Prepare for a meeting" — a tasteful, dismissible prep affordance shown near
 * the top of the record stage when NOT recording. The user names the meeting
 * (the subject is pre-filled by the parent from the next calendar event or a
 * detected meeting app, otherwise typed) and asks for a grounded brief assembled
 * from past meetings via {@link IpcService.preMeetingBrief}.
 *
 * Lives in its own file so its inline styles get their own per-component
 * `anyComponentStyle` budget (the record component's styles are near the cap),
 * and so it never competes structurally with the record hero.
 *
 * The brief markdown is rendered as PLAIN TEXT with `white-space: pre-wrap`
 * (no markdown lib, no innerHTML/DomSanitizer) — the model's line breaks and
 * spacing are preserved verbatim and safely. Source meetings render as chips
 * that deep-link to /meeting/:id.
 */
@Component({
  selector: "app-pre-meeting-brief",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MarkdownComponent, SourcesComponent],
  template: `
    <section class="brief card" role="group" aria-label="Prepare for a meeting">
      <div class="brief-head">
        <div class="brief-head-text">
          <span class="brief-eyebrow" aria-hidden="true">
            <span class="brief-spark"></span>
            Prep
          </span>
          <h3 class="brief-title">Prepare for a meeting</h3>
          <p class="brief-sub">
            Get a quick brief from your past meetings on this topic.
          </p>
        </div>
        <button
          type="button"
          class="brief-dismiss"
          (click)="dismissed.emit()"
          aria-label="Dismiss"
          title="Dismiss"
        >
          <span class="brief-dismiss-ico" aria-hidden="true"></span>
        </button>
      </div>

      <!-- Subject composer: input (Enter prepares) + Prepare button. -->
      <form class="brief-composer" (submit)="onSubmit($event)">
        <input
          #input
          type="text"
          class="brief-input"
          autocomplete="off"
          spellcheck="false"
          aria-label="Meeting subject"
          placeholder="e.g. Acme onboarding, Q3 planning…"
          [value]="subject()"
          [disabled]="pending()"
          (input)="onSubjectInput($event)"
        />
        <button
          type="submit"
          class="btn btn-primary brief-go"
          [disabled]="!canPrepare()"
        >
          @if (pending()) {
            <span class="brief-go-spin" aria-hidden="true"></span>
            Preparing…
          } @else {
            Prepare brief
          }
        </button>
      </form>

      <!-- Result / error region: pending shows in the button above. -->
      @if (error(); as err) {
        <div class="brief-error" role="alert">
          <span class="brief-error-text">{{ err }}</span>
          <button
            type="button"
            class="btn btn-ghost brief-retry"
            (click)="prepare()"
            [disabled]="pending()"
          >
            Retry
          </button>
        </div>
      }

      @if (visibleBrief(); as brief) {
        <div class="brief-result" aria-live="polite">
          <app-markdown
            class="brief-body"
            [markdown]="brief.markdown"
            compact
          />

          @if (brief.sources.length) {
            <app-sources class="brief-sources" [sources]="brief.sources" />
          }
        </div>
      }
    </section>
  `,
  styles: [
    `
      :host {
        display: block;
        width: 100%;
        max-width: 560px;
      }

      .brief {
        position: relative;
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
        padding: var(--space-4) var(--space-5) var(--space-5);
        overflow: hidden;
        animation: rise 360ms var(--transition) both;
      }
      /* A faint accent wash so the prep card reads as secondary, not a hero. */
      .brief::before {
        content: "";
        position: absolute;
        inset: 0;
        pointer-events: none;
        background: radial-gradient(
          120% 90% at 12% -10%,
          rgba(110, 118, 255, 0.1),
          transparent 60%
        );
      }
      .brief > * {
        position: relative;
        z-index: 1;
      }

      /* --- Head --- */
      .brief-head {
        display: flex;
        align-items: flex-start;
        justify-content: space-between;
        gap: var(--space-3);
      }
      .brief-head-text {
        display: flex;
        flex-direction: column;
        gap: 3px;
        min-width: 0;
      }
      .brief-eyebrow {
        display: inline-flex;
        align-items: center;
        gap: var(--space-1);
        color: var(--accent-hover);
        font-size: 0.6875rem;
        font-weight: 700;
        letter-spacing: 0.08em;
        text-transform: uppercase;
      }
      .brief-spark {
        width: 7px;
        height: 7px;
        border-radius: 50%;
        background: var(--accent);
        box-shadow: 0 0 10px rgba(110, 118, 255, 0.8);
      }
      .brief-title {
        margin: 0;
        font-size: 1.0625rem;
      }
      .brief-sub {
        margin: 0;
        color: var(--text-muted);
        font-size: 0.85rem;
        line-height: 1.45;
      }
      .brief-dismiss {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        flex: none;
        width: 30px;
        height: 30px;
        margin: -2px -4px 0 0;
        border: 1px solid transparent;
        border-radius: 50%;
        background: transparent;
        cursor: pointer;
        transition:
          background var(--transition),
          border-color var(--transition),
          transform var(--transition-fast);
      }
      .brief-dismiss:hover {
        background: var(--surface-hover);
        border-color: var(--border);
      }
      .brief-dismiss:active {
        transform: scale(0.94);
      }
      .brief-dismiss:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      /* Pure-CSS × glyph (no icon dependency). */
      .brief-dismiss-ico {
        width: 12px;
        height: 12px;
        background: var(--text-secondary);
        -webkit-mask: var(--x-mask) center / contain no-repeat;
        mask: var(--x-mask) center / contain no-repeat;
        --x-mask: url("data:image/svg+xml;charset=utf-8,%3Csvg xmlns='http://www.w3.org/2000/svg' width='24' height='24' viewBox='0 0 24 24' fill='none'%3E%3Cpath d='M5 5l14 14M19 5L5 19' stroke='black' stroke-width='2.4' stroke-linecap='round'/%3E%3C/svg%3E");
      }

      /* --- Composer --- */
      .brief-composer {
        display: flex;
        align-items: stretch;
        gap: var(--space-2);
      }
      .brief-input {
        flex: 1 1 auto;
        min-width: 0;
      }
      .brief-go {
        flex: none;
        white-space: nowrap;
      }
      .brief-go-spin {
        width: 15px;
        height: 15px;
        border-radius: 50%;
        border: 2px solid rgba(255, 255, 255, 0.35);
        border-top-color: var(--text-on-accent);
        animation: brief-spin 0.7s linear infinite;
      }

      /* --- Error + retry --- */
      .brief-error {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        padding: var(--space-2) var(--space-3);
        border: 1px solid rgba(255, 107, 107, 0.3);
        border-radius: var(--radius-md);
        background: var(--danger-soft);
        animation: rise 240ms var(--transition) both;
      }
      .brief-error-text {
        flex: 1 1 auto;
        min-width: 0;
        color: var(--text-primary);
        font-size: 0.875rem;
      }
      .brief-retry {
        flex: none;
        height: 30px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
      }

      /* --- Result --- */
      .brief-result {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
        animation: rise 320ms var(--transition) both;
      }
      .brief-body {
        max-height: 320px;
        overflow-y: auto;
        padding: var(--space-3) var(--space-4);
        border: 1px solid var(--glass-border);
        border-radius: var(--radius-md);
        background: var(--surface-raised);
        -webkit-backdrop-filter: blur(var(--glass-blur))
          saturate(var(--glass-saturate));
        backdrop-filter: blur(var(--glass-blur)) saturate(var(--glass-saturate));
        box-shadow: var(--glass-highlight);
        color: var(--text-primary);
        font-size: 0.9375rem;
        line-height: 1.6;
        /* Preserve the model's line breaks + spacing as plain text. */
        white-space: pre-wrap;
        overflow-wrap: anywhere;
        overscroll-behavior: contain;
      }

      /* --- Source chips --- */
      .brief-sources {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: var(--space-2);
      }
      .brief-sources-label {
        color: var(--text-muted);
        font-size: 0.6875rem;
        font-weight: 600;
        letter-spacing: 0.06em;
        text-transform: uppercase;
      }
      .brief-chip {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        max-width: 100%;
        padding: var(--space-1) var(--space-3);
        border: 1px solid var(--glass-border);
        border-radius: var(--radius-pill);
        background: rgba(255, 255, 255, 0.05);
        color: var(--text-secondary);
        font-size: 0.8125rem;
        font-weight: 550;
        line-height: 1.3;
        box-shadow: var(--glass-highlight);
        animation: rise 320ms var(--transition) both;
        animation-delay: calc(var(--i, 0) * 50ms + 60ms);
        transition:
          background var(--transition),
          border-color var(--transition),
          color var(--transition),
          transform var(--transition-fast);
      }
      .brief-chip:hover {
        background: var(--surface-hover);
        border-color: var(--border-strong);
        color: var(--text-primary);
      }
      .brief-chip:active {
        transform: translateY(1px);
      }
      .brief-chip:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .brief-chip-dot {
        width: 6px;
        height: 6px;
        min-width: 6px;
        border-radius: 50%;
        background: var(--accent);
      }
      .brief-chip-text {
        min-width: 0;
        overflow: hidden;
        text-overflow: ellipsis;
        white-space: nowrap;
      }

      @keyframes brief-spin {
        to {
          transform: rotate(360deg);
        }
      }

      @media (prefers-reduced-motion: reduce) {
        .brief,
        .brief-result,
        .brief-error,
        .brief-chip {
          animation: none;
        }
        .brief-go-spin {
          animation-duration: 0.01ms;
        }
        .brief-body {
          scroll-behavior: auto;
        }
      }
    `,
  ],
})
export class PreMeetingBriefComponent {
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);

  /** Initial subject the parent prefills (calendar event / detected app / ""). */
  readonly initialSubject = input<string>("");

  /** Bubbles up when the user dismisses the prep card. */
  readonly dismissed = output<void>();

  /** Working copy of the subject input. Seeded once from {@link initialSubject}. */
  readonly subject = signal("");

  /** The latest grounded brief, or null before a successful request. */
  readonly result = signal<BriefResult | null>(null);
  /** True while a {@link IpcService.preMeetingBrief} call is in flight. */
  readonly pending = signal(false);
  /** Inline error message (with a Retry affordance); null when clear. */
  readonly error = signal<string | null>(null);

  /** A request is allowed only with non-empty subject and nothing in flight. */
  readonly canPrepare = computed(
    () => !this.pending() && this.subject().trim().length > 0,
  );

  /** The brief to show — suppressed while an error is on screen. */
  readonly visibleBrief = computed(() => (this.error() ? null : this.result()));

  /** The subject input — focused on mount so the prep card is type-ready. */
  private readonly inputEl = viewChild<ElementRef<HTMLInputElement>>("input");

  /** Flips true the moment the user edits the field; locks out auto-prefill. */
  private userEdited = false;

  constructor() {
    // The prefill (calendar event / detected app) may resolve in the parent
    // AFTER this card first renders, so seed reactively rather than once: mirror
    // the latest prefill into the subject until the user takes over the field.
    effect(() => {
      const prefill = this.initialSubject().trim();
      if (!this.userEdited && prefill && prefill !== this.subject()) {
        this.subject.set(prefill);
      }
    });

    // Focus the input on mount (caret at end) so it's immediately type-ready —
    // afterNextRender is a zoneless-safe one-shot, auto-torn-down on destroy.
    afterNextRender(
      () => {
        const el = this.inputEl()?.nativeElement;
        if (el) {
          const len = el.value.length;
          el.setSelectionRange(len, len);
          el.focus();
        }
      },
      { injector: this.injector },
    );
  }

  /** Mirror the input value into the `subject` signal; lock out auto-prefill. */
  onSubjectInput(event: Event): void {
    this.userEdited = true;
    this.subject.set((event.target as HTMLInputElement).value);
  }

  /** Composer form submit (Prepare button / Enter). */
  onSubmit(event: Event): void {
    event.preventDefault();
    void this.prepare();
  }

  /**
   * Ask the backend for a grounded brief on the current subject. Awaits the IPC
   * result (no data subscriptions); on failure the subject is kept so the inline
   * Retry can re-run it.
   */
  async prepare(): Promise<void> {
    const subject = this.subject().trim();
    if (!subject || this.pending()) {
      return;
    }
    this.error.set(null);
    this.pending.set(true);
    try {
      this.result.set(await this.ipc.preMeetingBrief(subject));
    } catch (e) {
      this.error.set("Couldn’t prepare a brief: " + String(e));
    } finally {
      this.pending.set(false);
    }
  }

  /** Presentational only: a chip tooltip with the source meeting's date. */
  chipTitle(source: VaultSource): string {
    const date = new Date(source.startedAt);
    if (Number.isNaN(date.getTime())) {
      return source.title || "Untitled meeting";
    }
    const when = date.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    });
    return `${source.title || "Untitled meeting"} · ${when}`;
  }
}
