import {
  ChangeDetectionStrategy,
  Component,
  DestroyRef,
  computed,
  inject,
  signal,
} from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import { FormControl, ReactiveFormsModule } from "@angular/forms";
import { open } from "@tauri-apps/plugin-dialog";
import type { UnlistenFn } from "@tauri-apps/api/event";

import { IpcService } from "../../../../core/ipc.service";
import type {
  NoteFolder,
  NotionImportReport,
  NotionScanReport,
} from "../../../../core/models";
import { MurBannerComponent } from "../../../../design-system/banner/banner.component";
import { MurProgressComponent } from "../../../../design-system/progress/progress.component";
import { MurToggleComponent } from "../../../../design-system/toggle/toggle.component";
import { ToastService } from "../../../../services/toast.service";

/** The import sources this section offers. One today; the shape is what makes adding more cheap. */
type ImportSource = "notion";

/** Where the Notion flow is: pick an export, review the plan, watch it run, read the report. */
type Phase = "pick" | "planned" | "running" | "done";

/**
 * Settings → Imports. A source list on the left, the selected source's flow on the right.
 *
 * The Notion flow is deliberately a DRY RUN first: `scanNotionExport` writes nothing and returns
 * real counts, so the user confirms against what is actually in the archive rather than against a
 * promise. Every prior-art importer that skips this step has an issue tracker full of "it silently
 * did the wrong thing to my workspace".
 *
 * Zero egress: the export is a file already on this machine. Nothing here talks to Notion.
 */
@Component({
  selector: "app-settings-imports-section",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./settings-imports-section.component.html",
  styleUrl: "./settings-imports-section.component.scss",
  imports: [
    MurBannerComponent,
    MurProgressComponent,
    MurToggleComponent,
    ReactiveFormsModule,
  ],
})
export class SettingsImportsSectionComponent {
  private readonly ipc = inject(IpcService);
  private readonly toast = inject(ToastService);
  private readonly destroyRef = inject(DestroyRef);

  /** The selected source. A signal so adding a second one needs no restructuring. */
  readonly source = signal<ImportSource>("notion");

  readonly phase = signal<Phase>("pick");
  readonly exportPath = signal<string | null>(null);
  readonly scanning = signal(false);
  readonly plan = signal<NotionScanReport | null>(null);
  readonly report = signal<NotionImportReport | null>(null);

  /** Target folder id; `null` means the always-open Notes root. */
  readonly targetFolder = signal<string | null>(null);
  /**
   * `mur-toggle` is a ControlValueAccessor, so it binds through a control rather than an input.
   * `toSignal` is the sanctioned bridge from that stream back into signal state - never a
   * `.subscribe()` writing a field.
   */
  readonly mirrorControl = new FormControl<boolean>(true, { nonNullable: true });
  readonly mirrorHierarchy = toSignal(this.mirrorControl.valueChanges, {
    initialValue: true,
  });
  readonly folders = signal<NoteFolder[]>([]);

  /** Live progress, fed by the backend event stream (never by polling). */
  readonly stage = signal<string>("");
  readonly done = signal(0);
  readonly total = signal(0);

  /** The last path component, so the UI shows a name rather than a wall of path. */
  readonly exportName = computed(() => {
    const path = this.exportPath();
    if (!path) {
      return "";
    }
    return path.split("/").filter(Boolean).pop() ?? path;
  });

  readonly percent = computed(() => {
    const total = this.total();
    return total > 0 ? Math.round((this.done() / total) * 100) : 0;
  });

  /** New pages = everything in the plan that is not already here from an earlier run. */
  readonly newPages = computed(() => {
    const plan = this.plan();
    return plan ? Math.max(0, plan.pages - plan.alreadyImported) : 0;
  });

  readonly targetLabel = computed(() => {
    const id = this.targetFolder();
    if (!id) {
      return "Notes (unfiled)";
    }
    return this.folders().find((f) => f.id === id)?.name ?? "Notes (unfiled)";
  });

  /** A locked, not-session-unlocked target refuses every write; say so before the user tries. */
  readonly targetIsSealed = computed(() => {
    const id = this.targetFolder();
    if (!id) {
      return false;
    }
    const folder = this.folders().find((f) => f.id === id);
    return !!folder && folder.locked && !folder.unlocked;
  });

  private unlisten: UnlistenFn | null = null;

  constructor() {
    void this.loadFolders();
    void this.subscribe();
    this.destroyRef.onDestroy(() => {
      this.unlisten?.();
      this.unlisten = null;
    });
  }

  private async loadFolders(): Promise<void> {
    try {
      this.folders.set(await this.ipc.listNoteFolders());
    } catch {
      // A folder-list failure must not block the flow: the import still works against the root.
      this.folders.set([]);
    }
  }

  private async subscribe(): Promise<void> {
    this.unlisten = await this.ipc.onBulkImport((p) => {
      this.stage.set(p.stage);
      this.done.set(p.done);
      this.total.set(p.total);
    });
  }

  /** Choose the `.zip` Notion emailed you. */
  async chooseArchive(): Promise<void> {
    const chosen = await open({
      directory: false,
      multiple: false,
      filters: [{ name: "Notion export", extensions: ["zip"] }],
    });
    if (typeof chosen === "string") {
      this.selectExport(chosen);
    }
  }

  /** Choose an already-unpacked export folder. */
  async chooseFolder(): Promise<void> {
    const chosen = await open({ directory: true, multiple: false });
    if (typeof chosen === "string") {
      this.selectExport(chosen);
    }
  }

  private selectExport(path: string): void {
    this.exportPath.set(path);
    this.plan.set(null);
    this.report.set(null);
    this.phase.set("pick");
    void this.scan();
  }

  /** DRY RUN — writes nothing. */
  async scan(): Promise<void> {
    const path = this.exportPath();
    if (!path) {
      return;
    }
    this.scanning.set(true);
    try {
      const plan = await this.ipc.scanNotionExport(path);
      this.plan.set(plan);
      this.phase.set("planned");
      if (plan.pages === 0) {
        this.toast.danger(
          "No Notion pages found in there. Pick the export archive or the folder it unpacked to.",
        );
      }
    } catch (e) {
      this.toast.danger(`Could not read that export: ${errorText(e)}`);
      this.phase.set("pick");
    } finally {
      this.scanning.set(false);
    }
  }

  async runImport(): Promise<void> {
    const path = this.exportPath();
    if (!path || this.phase() === "running") {
      return;
    }
    this.phase.set("running");
    this.stage.set("scanning");
    this.done.set(0);
    this.total.set(this.plan()?.pages ?? 0);
    try {
      const report = await this.ipc.importNotionExport(
        path,
        this.targetFolder(),
        this.mirrorHierarchy(),
      );
      this.report.set(report);
      this.phase.set("done");
      if (report.failed > 0) {
        this.toast.danger(
          `Imported ${report.imported + report.updated}, but ${report.failed} failed.`,
        );
      } else if (report.cancelled) {
        this.toast.success(
          `Stopped. ${report.imported + report.updated} notes were kept.`,
        );
      } else {
        this.toast.success(
          `${report.imported} imported, ${report.updated} updated.`,
        );
      }
      // A fresh import may have created folders; keep the picker honest.
      void this.loadFolders();
    } catch (e) {
      this.toast.danger(`Import failed: ${errorText(e)}`);
      this.phase.set("planned");
    }
  }

  async cancel(): Promise<void> {
    await this.ipc.cancelNotionImport();
  }

  /** Back to the start without losing the chosen export. */
  reset(): void {
    this.report.set(null);
    this.phase.set(this.plan() ? "planned" : "pick");
  }

  selectTarget(event: Event): void {
    const value = (event.target as HTMLSelectElement).value;
    this.targetFolder.set(value === "" ? null : value);
  }

  mb(bytes: number): string {
    if (bytes < 1024) {
      return `${bytes} B`;
    }
    if (bytes < 1024 * 1024) {
      return `${Math.round(bytes / 1024)} KB`;
    }
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  }
}

/** IPC rejects with an `AppError` shape or a string; render either without leaking `[object Object]`. */
function errorText(e: unknown): string {
  if (typeof e === "string") {
    return e;
  }
  if (e && typeof e === "object") {
    const values = Object.values(e as Record<string, unknown>);
    const first = values.find((v) => typeof v === "string");
    if (typeof first === "string") {
      return first;
    }
  }
  return "unknown error";
}
