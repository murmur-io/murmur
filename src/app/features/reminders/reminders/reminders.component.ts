import {
  ChangeDetectionStrategy,
  Component,
  OnInit,
  computed,
  inject,
  signal,
} from "@angular/core";
import { TabsService } from "../../../core/tabs.service";
import type {
  ReminderInboxItem,
  ReminderSourceView,
  ReminderView,
} from "../../../core/models";
import { ReminderComposerService } from "../reminder-composer/reminder-composer.service";
import { RemindersStore } from "../reminders.store";

type ReminderSegment = "inbox" | "upcoming" | "completed";

interface ReminderRowVm {
  key: string;
  occurrenceId: string | null;
  expectedDueAt: number;
  reminder: ReminderView;
  dueLabel: string;
  recurrenceLabel: string | null;
}

interface ReminderGroupVm {
  id: string;
  label: string;
  rows: ReminderRowVm[];
}

const DATE_TIME = new Intl.DateTimeFormat(undefined, {
  weekday: "short",
  month: "short",
  day: "numeric",
  hour: "numeric",
  minute: "2-digit",
});

const MONTH_YEAR = new Intl.DateTimeFormat(undefined, {
  month: "long",
  year: "numeric",
});

function recurrenceLabel(reminder: ReminderView): string | null {
  if (!reminder.repeatEvery || !reminder.repeatUnit) {
    return null;
  }
  const singular = reminder.repeatUnit.slice(0, -1);
  return reminder.repeatEvery === 1
    ? `Every ${singular}`
    : `Every ${reminder.repeatEvery} ${reminder.repeatUnit}`;
}

function reminderRow(
  reminder: ReminderView,
  occurrence?: ReminderInboxItem,
): ReminderRowVm {
  const expectedDueAt = occurrence?.dueAt ?? reminder.dueAt;
  return {
    key: occurrence?.occurrenceId ?? reminder.id,
    occurrenceId: occurrence?.occurrenceId ?? null,
    expectedDueAt,
    reminder,
    dueLabel: DATE_TIME.format(new Date(expectedDueAt)),
    recurrenceLabel: recurrenceLabel(reminder),
  };
}

@Component({
  selector: "app-reminders",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./reminders.component.html",
  styleUrl: "./reminders.component.scss",
})
export class RemindersComponent implements OnInit {
  readonly store = inject(RemindersStore);
  private readonly composer = inject(ReminderComposerService);
  private readonly tabs = inject(TabsService);

  readonly segment = signal<ReminderSegment>("inbox");
  readonly confirmingDelete = signal<string | null>(null);

  readonly counts = computed(() => ({
    inbox: this.store.inbox().length,
    upcoming: this.store.upcoming().length,
    completed: this.store.completed().length,
  }));

  readonly rows = computed<ReminderRowVm[]>(() => {
    switch (this.segment()) {
      case "inbox":
        return this.store
          .inbox()
          .map((item) => reminderRow(item.reminder, item));
      case "upcoming":
        return this.store.upcoming().map((reminder) => reminderRow(reminder));
      case "completed":
        return this.store.completed().map((reminder) => reminderRow(reminder));
    }
  });

  readonly groups = computed<ReminderGroupVm[]>(() => {
    const rows = this.rows();
    if (this.segment() === "inbox") {
      const now = Date.now();
      const overdue = rows.filter((row) => row.expectedDueAt < now);
      const due = rows.filter((row) => row.expectedDueAt >= now);
      return [
        { id: "overdue", label: "Overdue", rows: overdue },
        { id: "due", label: "Due now", rows: due },
      ].filter((group) => group.rows.length > 0);
    }
    const grouped = new Map<string, { label: string; rows: ReminderRowVm[] }>();
    for (const row of rows) {
      const date = new Date(row.expectedDueAt);
      const key = `${date.getFullYear()}-${String(date.getMonth() + 1).padStart(
        2,
        "0",
      )}`;
      const current = grouped.get(key);
      grouped.set(key, {
        label: current?.label ?? MONTH_YEAR.format(date),
        rows: [...(current?.rows ?? []), row],
      });
    }
    return [...grouped].map(([id, group]) => ({
      id: `month-${id}`,
      label: group.label,
      rows: group.rows,
    }));
  });

  readonly emptyCopy = computed(() => {
    switch (this.segment()) {
      case "inbox":
        return {
          title: "Inbox clear",
          body: "Due reminders will stay here until you complete or dismiss them.",
        };
      case "upcoming":
        return {
          title: "Nothing upcoming",
          body: "Create a reminder and Murmur will keep it close to your work.",
        };
      case "completed":
        return {
          title: "No completed reminders",
          body: "Finished reminders will collect here.",
        };
    }
  });

  ngOnInit(): void {
    void this.store.initSummary();
    void this.store.refresh();
  }

  selectSegment(segment: ReminderSegment): void {
    this.segment.set(segment);
    this.confirmingDelete.set(null);
  }

  newReminder(): void {
    this.composer.openCreate();
  }

  edit(reminder: ReminderView): void {
    this.composer.openEdit(reminder);
  }

  async complete(row: ReminderRowVm): Promise<void> {
    await this.store.complete(row.reminder.id, row.expectedDueAt).catch(() => {
      // Store owns the visible error state.
    });
  }

  async dismiss(row: ReminderRowVm): Promise<void> {
    if (!row.occurrenceId) {
      return;
    }
    await this.store.dismissOccurrence(row.occurrenceId).catch(() => {
      // Store owns the visible error state.
    });
  }

  askDelete(reminderId: string): void {
    this.confirmingDelete.set(reminderId);
  }

  cancelDelete(): void {
    this.confirmingDelete.set(null);
  }

  async delete(reminderId: string): Promise<void> {
    await this.store
      .delete(reminderId)
      .then(() => this.confirmingDelete.set(null))
      .catch(() => {
        // Store owns the visible error state.
      });
  }

  openSource(source: ReminderSourceView): void {
    if (source.kind === "meeting") {
      void this.tabs.openMeeting(source.id, source.title || "Meeting");
    } else {
      void this.tabs.openNote(source.id, source.title || "Note");
    }
  }
}
