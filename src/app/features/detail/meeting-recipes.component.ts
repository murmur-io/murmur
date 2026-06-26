import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  ElementRef,
  Injector,
  OnInit,
  afterNextRender,
  computed,
  inject,
  input,
  signal,
  viewChild,
} from "@angular/core";
import { IpcService } from "../../core/ipc.service";
import type { BuiltinRecipe, SavedRecipe } from "../../core/models";
import { MarkdownComponent } from "../../shared/markdown.component";

/**
 * "Recipes / Generate" — one-tap grounded generations over a single meeting's
 * transcript. A presentational sibling of the analysis + chat cards: the parent
 * owns the meeting; this component owns only the recipe catalog (builtin +
 * user-saved) and the single generation it produces via
 * {@link IpcService.runRecipe}, which answers strictly from that transcript.
 *
 * Lives in its own file so its inline styles get their own per-component
 * `anyComponentStyle` budget (the detail component's styles are near the cap),
 * mirroring {@link MeetingChatComponent}.
 *
 * The returned Markdown is rendered as PLAIN TEXT with `white-space: pre-wrap`
 * (no markdown lib, no innerHTML/DomSanitizer) — line breaks + spacing from the
 * model are preserved verbatim and safely.
 */
@Component({
  selector: "app-meeting-recipes",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MarkdownComponent],
  template: `
    <div class="rec card">
      <div class="rec-head">
        <div class="rec-head-text">
          <h3 class="rec-title">Generate</h3>
          <span class="rec-sub"
            >Run a recipe over this transcript — grounded only here</span
          >
        </div>
      </div>

      <!-- Recipe chips: builtin (label) + saved (title + tiny × delete). -->
      <div class="rec-chips" role="group" aria-label="Recipes">
        @for (b of builtin(); track b.id) {
          <button
            type="button"
            class="rec-chip"
            [style.--i]="$index"
            [disabled]="pending()"
            (click)="run(b.prompt)"
          >
            {{ b.label }}
          </button>
        }
        @for (s of saved(); track s.id) {
          <span
            class="rec-chip rec-chip-saved"
            [class.is-disabled]="pending()"
            [style.--i]="builtin().length + $index"
          >
            <button
              type="button"
              class="rec-chip-run"
              [disabled]="pending()"
              (click)="run(s.prompt)"
            >
              {{ s.title }}
            </button>
            <button
              type="button"
              class="rec-chip-x"
              [attr.aria-label]="'Delete recipe ' + s.title"
              [disabled]="pending() || deletingId() === s.id"
              (click)="removeRecipe(s.id)"
            >
              ×
            </button>
          </span>
        }

        <!-- "Custom…" affordance toggles a free-form prompt composer. -->
        <button
          type="button"
          class="rec-chip rec-chip-custom"
          [class.is-open]="customOpen()"
          [style.--i]="builtin().length + saved().length"
          [disabled]="pending()"
          [attr.aria-expanded]="customOpen()"
          (click)="toggleCustom()"
        >
          Custom…
        </button>
      </div>

      <!-- Custom prompt composer (textarea + Run). -->
      @if (customOpen()) {
        <form class="rec-custom" (submit)="onCustomSubmit($event)">
          <textarea
            #custom
            class="rec-custom-input"
            rows="2"
            autocapitalize="sentences"
            autocomplete="off"
            spellcheck="true"
            aria-label="Custom prompt"
            placeholder="e.g. Draft a follow-up email to the team…"
            [value]="customDraft()"
            [disabled]="pending()"
            (input)="onCustomInput($event)"
            (keydown)="onCustomKeydown($event)"
          ></textarea>
          <button
            type="submit"
            class="btn btn-primary rec-custom-run"
            [disabled]="!canRunCustom()"
          >
            Run
          </button>
        </form>
      }

      <!-- "Generating…" indicator while a run is in flight. -->
      @if (pending()) {
        <div class="rec-pending" role="status" aria-live="polite">
          <span class="rec-pending-spin" aria-hidden="true"></span>
          <span>Generating…</span>
        </div>
      }

      <!-- Inline error (keeps the last prompt so Retry can re-run it). -->
      @if (error(); as err) {
        <div class="rec-error" role="alert">
          <span class="rec-error-text">{{ err }}</span>
          <button
            type="button"
            class="btn btn-ghost rec-retry"
            (click)="retry()"
            [disabled]="pending() || !lastPrompt()"
          >
            Retry
          </button>
        </div>
      }

      <!-- OUTPUT card: returned Markdown as PLAIN text + Copy / Save affordances. -->
      @if (output(); as out) {
        <article
          #output
          class="rec-output"
          role="region"
          aria-label="Generated output"
        >
          <div class="rec-output-head">
            <span class="rec-output-label">Result</span>
            <div class="rec-output-actions">
              <button
                type="button"
                class="btn btn-ghost rec-output-btn"
                (click)="copy(out)"
                [disabled]="pending()"
              >
                {{ copied() ? "Copied" : "Copy" }}
              </button>
              <button
                type="button"
                class="btn btn-ghost rec-output-btn"
                (click)="saveAsRecipe()"
                [disabled]="pending() || !lastPrompt()"
              >
                Save as recipe
              </button>
            </div>
          </div>
          <app-markdown class="rec-output-body" [markdown]="out" compact />
        </article>
      } @else if (!pending() && !error()) {
        <!-- Empty / first-use hint. -->
        <div class="rec-empty">
          <span class="rec-empty-mark" aria-hidden="true"></span>
          <p class="rec-empty-title">Turn this meeting into something useful</p>
          <p class="rec-empty-copy">
            Pick a recipe above — a summary, action items, a follow-up email —
            or write your own. The result is grounded only in this transcript.
          </p>
        </div>
      }
    </div>
  `,
  styles: [
    `
      :host {
        display: block;
      }

      .rec {
        position: relative;
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
        padding: var(--space-5);
        overflow: hidden;
        animation: rise 420ms var(--transition) both;
      }
      /* A faint aurora wash to lift the glass above the page surface. */
      .rec::before {
        content: "";
        position: absolute;
        inset: 0;
        pointer-events: none;
        background: radial-gradient(
          120% 90% at 12% -10%,
          rgba(74, 196, 240, 0.1),
          transparent 60%
        );
      }
      .rec > * {
        position: relative;
        z-index: 1;
      }

      /* --- Head --- */
      .rec-head {
        display: flex;
        align-items: flex-start;
        justify-content: space-between;
        gap: var(--space-3);
      }
      .rec-head-text {
        display: flex;
        flex-direction: column;
        gap: 2px;
        min-width: 0;
      }
      .rec-title {
        margin: 0;
      }
      .rec-sub {
        color: var(--text-muted);
        font-size: 0.8125rem;
      }

      /* --- Recipe chips (wrapping row) --- */
      .rec-chips {
        display: flex;
        flex-wrap: wrap;
        gap: var(--space-2);
      }
      .rec-chip {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        padding: var(--space-2) var(--space-3);
        border: 1px solid var(--glass-border);
        border-radius: var(--radius-pill);
        background: rgba(255, 255, 255, 0.05);
        color: var(--text-secondary);
        font-family: inherit;
        font-size: 0.8125rem;
        font-weight: 550;
        line-height: 1.2;
        cursor: pointer;
        box-shadow: var(--glass-highlight);
        animation: rise 360ms var(--transition) both;
        animation-delay: calc(var(--i, 0) * 50ms + 60ms);
        transition:
          background var(--transition),
          border-color var(--transition),
          color var(--transition),
          transform var(--transition-fast);
      }
      .rec-chip:hover:not(:disabled):not(.is-disabled) {
        background: var(--surface-hover);
        border-color: var(--border-strong);
        color: var(--text-primary);
      }
      .rec-chip:active:not(:disabled) {
        transform: translateY(1px);
      }
      .rec-chip:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .rec-chip:disabled,
      .rec-chip.is-disabled {
        opacity: 0.45;
        cursor: not-allowed;
      }

      /* Saved chip: a run button + a tiny delete, as one pill. */
      .rec-chip-saved {
        padding: 0 var(--space-1) 0 0;
        background: var(--accent-soft);
        border-color: transparent;
        color: var(--accent-hover);
      }
      .rec-chip-run {
        padding: var(--space-2) var(--space-1) var(--space-2) var(--space-3);
        border: none;
        background: transparent;
        color: inherit;
        font-family: inherit;
        font-size: 0.8125rem;
        font-weight: 600;
        line-height: 1.2;
        cursor: pointer;
      }
      .rec-chip-run:disabled {
        cursor: not-allowed;
      }
      .rec-chip-run:focus-visible {
        outline: none;
        border-radius: var(--radius-pill);
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .rec-chip-x {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 18px;
        height: 18px;
        padding: 0;
        border: none;
        border-radius: var(--radius-pill);
        background: transparent;
        color: inherit;
        font-size: 0.95rem;
        line-height: 1;
        cursor: pointer;
        opacity: 0.7;
        transition:
          background var(--transition),
          opacity var(--transition);
      }
      .rec-chip-x:hover:not(:disabled) {
        background: var(--surface-hover);
        opacity: 1;
      }
      .rec-chip-x:focus-visible {
        outline: none;
        box-shadow: 0 0 0 2px var(--accent-ring);
      }
      .rec-chip-x:disabled {
        cursor: not-allowed;
        opacity: 0.4;
      }

      /* Custom toggle — dashed accent to read as "add your own". */
      .rec-chip-custom {
        border-style: dashed;
        color: var(--text-muted);
      }
      .rec-chip-custom.is-open {
        background: var(--accent-soft);
        border-color: transparent;
        color: var(--accent-hover);
      }

      /* --- Custom prompt composer --- */
      .rec-custom {
        display: flex;
        align-items: flex-end;
        gap: var(--space-2);
        animation: rise 240ms var(--transition) both;
      }
      .rec-custom-input {
        flex: 1 1 auto;
        min-width: 0;
        min-height: 56px;
        max-height: 168px;
        padding: var(--space-3) var(--space-4);
        line-height: 1.5;
        resize: none;
      }
      .rec-custom-run {
        flex: none;
        height: 44px;
        padding: 0 var(--space-4);
      }

      /* --- "Generating…" indicator --- */
      .rec-pending {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        align-self: flex-start;
        padding: var(--space-2) var(--space-3);
        border: 1px solid var(--glass-border);
        border-radius: var(--radius-pill);
        background: rgba(255, 255, 255, 0.05);
        color: var(--text-secondary);
        font-size: 0.8125rem;
        font-weight: 550;
        box-shadow: var(--glass-highlight);
        animation: rise 240ms var(--transition) both;
      }
      .rec-pending-spin {
        width: 14px;
        height: 14px;
        border-radius: 50%;
        border: 2px solid var(--surface-hover);
        border-top-color: var(--accent-hover);
        animation: rec-spin 0.7s linear infinite;
      }

      /* --- Inline error + retry --- */
      .rec-error {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        padding: var(--space-2) var(--space-3);
        border: 1px solid rgba(255, 107, 107, 0.3);
        border-radius: var(--radius-md);
        background: var(--danger-soft);
        animation: rise 240ms var(--transition) both;
      }
      .rec-error-text {
        flex: 1 1 auto;
        min-width: 0;
        color: var(--text-primary);
        font-size: 0.875rem;
      }
      .rec-retry {
        flex: none;
        height: 30px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
      }

      /* --- Output card --- */
      .rec-output {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
        padding: var(--space-4);
        border: 1px solid var(--glass-border);
        border-radius: var(--radius-lg);
        background: var(--surface-raised);
        -webkit-backdrop-filter: blur(var(--glass-blur))
          saturate(var(--glass-saturate));
        backdrop-filter: blur(var(--glass-blur)) saturate(var(--glass-saturate));
        box-shadow: var(--glass-highlight);
        animation: bubble-in 320ms var(--ease-spring) both;
      }
      .rec-output-head {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-3);
      }
      .rec-output-label {
        color: var(--text-muted);
        font-size: 0.75rem;
        font-weight: 600;
        letter-spacing: 0.04em;
        text-transform: uppercase;
      }
      .rec-output-actions {
        display: flex;
        align-items: center;
        gap: var(--space-1);
      }
      .rec-output-btn {
        height: 30px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
      }
      .rec-output-body {
        display: block;
        color: var(--text-primary);
        font-size: 0.9375rem;
        overflow-wrap: anywhere;
        max-height: 520px;
        overflow-y: auto;
      }

      /* --- Empty / first-use state --- */
      .rec-empty {
        display: flex;
        flex-direction: column;
        align-items: center;
        gap: var(--space-2);
        padding: var(--space-5) var(--space-4);
        text-align: center;
      }
      .rec-empty-mark {
        width: 40px;
        height: 40px;
        margin-bottom: var(--space-1);
        border-radius: var(--radius-pill);
        background: var(--accent-soft);
        border: 1px solid var(--glass-border);
        box-shadow: var(--glass-highlight);
      }
      .rec-empty-title {
        margin: 0;
        color: var(--text-primary);
        font-weight: 600;
      }
      .rec-empty-copy {
        margin: 0;
        max-width: 48ch;
        color: var(--text-muted);
        font-size: 0.875rem;
      }

      @keyframes bubble-in {
        from {
          opacity: 0;
          transform: translateY(8px) scale(0.98);
        }
        to {
          opacity: 1;
          transform: translateY(0) scale(1);
        }
      }
      @keyframes rec-spin {
        to {
          transform: rotate(360deg);
        }
      }

      @media (prefers-reduced-motion: reduce) {
        .rec,
        .rec-chip,
        .rec-custom,
        .rec-pending,
        .rec-error,
        .rec-output {
          animation: none;
        }
        .rec-pending-spin {
          animation-duration: 0.01ms;
        }
      }
    `,
  ],
})
export class MeetingRecipesComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);
  private readonly destroyRef = inject(DestroyRef);

  /** The meeting whose transcript grounds every generation. */
  readonly meetingId = input.required<string>();

  /** Built-in recipe templates (quick chips). */
  readonly builtin = signal<BuiltinRecipe[]>([]);
  /** User-saved recipe templates (quick chips with delete). */
  readonly saved = signal<SavedRecipe[]>([]);

  /** True while a {@link IpcService.runRecipe} call is in flight. */
  readonly pending = signal(false);
  /** The generated Markdown (rendered as plain text); null before any run. */
  readonly output = signal<string | null>(null);
  /** Inline error message (with a Retry affordance); null when clear. */
  readonly error = signal<string | null>(null);
  /** The prompt of the most recent run — re-used by Retry + Save-as-recipe. */
  readonly lastPrompt = signal<string | null>(null);

  /** Whether the free-form custom-prompt composer is shown. */
  readonly customOpen = signal(false);
  /** Working copy of the custom-prompt textarea (input → signal). */
  readonly customDraft = signal("");

  /** The saved recipe currently being deleted (disables only its × button). */
  readonly deletingId = signal<string | null>(null);
  /** Drives the brief "Copied" flash on the output Copy button. */
  readonly copied = signal(false);

  /** Tracked so we can cancel the pending "Copied" reset on destroy (no leaks). */
  private copiedResetTimer: ReturnType<typeof setTimeout> | null = null;

  /** A custom run is allowed only with non-empty text and no in-flight request. */
  readonly canRunCustom = computed(
    () => !this.pending() && this.customDraft().trim().length > 0,
  );

  async ngOnInit(): Promise<void> {
    await this.refreshRecipes();
  }

  /** (Re)load builtin + saved recipes into their signals (best-effort). */
  private async refreshRecipes(): Promise<void> {
    try {
      const [builtin, saved] = await Promise.all([
        this.ipc.listBuiltinRecipes(),
        this.ipc.listSavedRecipes(),
      ]);
      this.builtin.set(builtin);
      this.saved.set(saved);
    } catch {
      // Leave whatever we have; the empty state still reads sensibly.
    }
  }

  // --- Custom composer -----------------------------------------------------

  /** Show/hide the custom-prompt composer, focusing it when it opens. */
  toggleCustom(): void {
    const next = !this.customOpen();
    this.customOpen.set(next);
    if (next) {
      afterNextRender(() => this.customInput()?.nativeElement.focus(), {
        injector: this.injector,
      });
    }
  }

  /** Mirror the custom textarea value into the `customDraft` signal. */
  onCustomInput(event: Event): void {
    this.customDraft.set((event.target as HTMLTextAreaElement).value);
  }

  /** Enter runs; Shift+Enter inserts a newline (textarea default). */
  onCustomKeydown(event: KeyboardEvent): void {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      void this.runCustom();
    }
  }

  /** Custom form submit (Run button / Enter). */
  onCustomSubmit(event: Event): void {
    event.preventDefault();
    void this.runCustom();
  }

  /** Run the custom prompt, then clear it from the composer. */
  private async runCustom(): Promise<void> {
    const prompt = this.customDraft().trim();
    if (!prompt || this.pending()) {
      return;
    }
    this.customDraft.set("");
    await this.run(prompt);
  }

  // --- Run -----------------------------------------------------------------

  /**
   * Run a grounded prompt over the transcript. Ignores empty prompts / a run
   * already in flight; sets pending, awaits the Markdown result, then shows it
   * in the output card. On failure the prompt is kept (Retry re-runs it).
   */
  async run(prompt: string): Promise<void> {
    const trimmed = prompt.trim();
    if (!trimmed || this.pending()) {
      return;
    }

    this.error.set(null);
    this.lastPrompt.set(trimmed);
    this.pending.set(true);

    try {
      const result = await this.ipc.runRecipe(this.meetingId(), trimmed);
      this.output.set(result);
      this.scrollToOutput();
    } catch (e) {
      // Keep lastPrompt so Retry can re-run the same recipe.
      this.error.set("Couldn’t generate: " + String(e));
    } finally {
      this.pending.set(false);
    }
  }

  /** Re-run the most recent prompt after an error. */
  retry(): void {
    const prompt = this.lastPrompt();
    if (!prompt || this.pending()) {
      return;
    }
    void this.run(prompt);
  }

  // --- Saved-recipe management ---------------------------------------------

  /** Delete a saved recipe, then refresh the chips. */
  async removeRecipe(id: string): Promise<void> {
    if (this.pending() || this.deletingId()) {
      return;
    }
    this.deletingId.set(id);
    try {
      await this.ipc.deleteRecipe(id);
      await this.refreshRecipes();
    } catch (e) {
      this.error.set("Couldn’t delete recipe: " + String(e));
    } finally {
      this.deletingId.set(null);
    }
  }

  /**
   * Save the prompt behind the current output as a reusable recipe. Prompts for
   * a title (the one place a native prompt is acceptable — no modal scope here),
   * saves it, then refreshes the chips so it appears immediately.
   */
  async saveAsRecipe(): Promise<void> {
    const prompt = this.lastPrompt();
    if (!prompt || this.pending()) {
      return;
    }
    const title = window.prompt("Name this recipe")?.trim();
    if (!title) {
      return;
    }
    try {
      await this.ipc.saveRecipe(title, prompt);
      await this.refreshRecipes();
    } catch (e) {
      this.error.set("Couldn’t save recipe: " + String(e));
    }
  }

  // --- Copy ----------------------------------------------------------------

  /** Copy the output to the clipboard, flashing "Copied" briefly. */
  async copy(text: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(text);
      this.flashCopied();
    } catch {
      this.copied.set(false);
    }
  }

  /** Show "Copied" for a moment (tracked timeout — cancelled on destroy). */
  private flashCopied(): void {
    this.copied.set(true);
    if (this.copiedResetTimer) {
      clearTimeout(this.copiedResetTimer);
    }
    this.copiedResetTimer = setTimeout(() => this.copied.set(false), 2000);
    this.destroyRef.onDestroy(() => {
      if (this.copiedResetTimer) {
        clearTimeout(this.copiedResetTimer);
      }
    });
  }

  // --- Auto-scroll ---------------------------------------------------------

  /** The custom-prompt textarea (focused on open). */
  private readonly customInput =
    viewChild<ElementRef<HTMLTextAreaElement>>("custom");
  /** The generated-output card (scrolled into view after a run). */
  private readonly outputEl = viewChild<ElementRef<HTMLElement>>("output");

  /**
   * Bring the output into view after the next render so the card is laid out
   * before we scroll — zoneless safe, no setTimeout. afterNextRender registered
   * with this component's injector is a one-shot and is auto-torn-down when the
   * component is destroyed, so there is nothing to clean up manually.
   */
  private scrollToOutput(): void {
    afterNextRender(
      () => {
        this.outputEl()?.nativeElement.scrollIntoView({
          behavior: "smooth",
          block: "nearest",
        });
      },
      { injector: this.injector },
    );
  }
}
