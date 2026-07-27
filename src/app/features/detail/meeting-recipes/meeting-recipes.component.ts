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
import { IpcService } from "../../../core/ipc.service";
import type { BuiltinRecipe, SavedRecipe } from "../../../core/models";
import { MarkdownComponent } from "../../../shared/markdown/markdown.component";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";

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
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MarkdownComponent],
  templateUrl: "./meeting-recipes.component.html",
  styleUrl: "./meeting-recipes.component.scss",
})
export class MeetingRecipesComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);
  private readonly destroyRef = inject(DestroyRef);
  private readonly errorCopy = inject(ErrorCopyService);

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
      this.error.set(this.errorCopy.because("Couldn’t generate", e));
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
      this.error.set(this.errorCopy.because("Couldn’t delete recipe", e));
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
      this.error.set(this.errorCopy.because("Couldn’t save recipe", e));
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
