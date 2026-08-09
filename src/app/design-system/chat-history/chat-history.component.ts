import {
  ChangeDetectionStrategy,
  Component,
  computed,
  input,
  output,
} from "@angular/core";
import type { AskConversationSummary } from "../../core/models";

interface HistoryRow {
  thread: AskConversationSummary;
  updatedLabel: string;
}

/**
 * Shared in-flow browser for durable Ask Brain conversations. The owning Ask
 * surface owns IPC and view state; this primitive only renders bounded rows and
 * emits stable backend ids. It deliberately has no card frame so narrow drawers
 * remain one coherent pane instead of glass nested inside glass.
 */
@Component({
  selector: "mur-chat-history",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./chat-history.component.html",
  styleUrl: "./chat-history.component.scss",
})
export class ChatHistoryComponent {
  readonly threads = input<readonly AskConversationSummary[]>([]);
  readonly loading = input(false);
  readonly error = input<string | null>(null);
  /** Non-destructive row-action error; cached rows stay mounted and focused. */
  readonly notice = input<string | null>(null);
  readonly activeThreadId = input<string | null>(null);
  readonly loadingThreadId = input<string | null>(null);

  readonly selected = output<string>();
  readonly retryRequested = output<void>();

  protected readonly rows = computed<HistoryRow[]>(() =>
    this.threads().map((thread) => ({
      thread,
      updatedLabel: this.formatTimestamp(thread.updatedAt),
    })),
  );

  private formatTimestamp(value: string): string {
    const date = new Date(value);
    if (Number.isNaN(date.getTime())) {
      return value;
    }
    return new Intl.DateTimeFormat(undefined, {
      dateStyle: "medium",
      timeStyle: "short",
    }).format(date);
  }
}
