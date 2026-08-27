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
import { Router } from "@angular/router";
import { open } from "@tauri-apps/plugin-dialog";
import type { UnlistenFn } from "@tauri-apps/api/event";

import { IpcService } from "../../../../core/ipc.service";
import type {
  ImportReport,
  ImportScanReport,
  ImportSourceId,
  NoteFolder,
} from "../../../../core/models";
import { MurBannerComponent } from "../../../../design-system/banner/banner.component";
import { MurProgressComponent } from "../../../../design-system/progress/progress.component";
import { MurToggleComponent } from "../../../../design-system/toggle/toggle.component";
import { NotesService } from "../../../../services/notes.service";
import { ToastService } from "../../../../services/toast.service";

/** How a source is chosen: an archive file, a folder, or nothing at all. */
type PickKind = "archive" | "folder" | "none";

interface ImportSourceMeta {
  readonly id: ImportSourceId;
  readonly label: string;
  readonly pick: PickKind;
}

/**
 * The sources, in rail order. Adding a fourth is an entry here plus a Rust normalizer — the flow
 * below is deliberately source-agnostic.
 */
const SOURCES: readonly ImportSourceMeta[] = [
  { id: "notion", label: "Notion", pick: "archive" },
  { id: "obsidian", label: "Obsidian", pick: "folder" },
  { id: "apple-notes", label: "Apple Notes", pick: "none" },
];

/** Where the flow is: choose a source, review the plan, watch it run, read the report. */
type Phase = "pick" | "planned" | "running" | "done";

/**
 * Settings → Imports. A source rail on the left, the selected source's flow on the right.
 *
 * Every source is scanned as a DRY RUN first: the scan writes nothing and returns real counts, so
 * the user confirms against what is actually there rather than against a promise. Every comparable
 * importer that skips this step has an issue tracker full of "it silently did the wrong thing to my
 * workspace".
 *
 * Zero egress: a downloaded export, a local vault folder, or the Notes app over Apple events.
 * Nothing here talks to a server.
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
  private readonly notes = inject(NotesService);
  private readonly router = inject(Router);

  readonly sources = SOURCES;
  readonly sourceId = signal<ImportSourceId>("notion");
  readonly source = computed(
    () => SOURCES.find((s) => s.id === this.sourceId()) ?? SOURCES[0],
  );

  readonly phase = signal<Phase>("pick");
  readonly sourcePath = signal<string | null>(null);
  readonly scanning = signal(false);
  readonly plan = signal<ImportScanReport | null>(null);
  readonly report = signal<ImportReport | null>(null);

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
  readonly sourceName = computed(() => {
    const path = this.sourcePath();
    if (!path) {
      return "";
    }
    return path.split("/").filter(Boolean).pop() ?? path;
  });

  /** Apple Notes has nothing to choose — the library is the source. */
  readonly needsPick = computed(() => this.source().pick !== "none");

  readonly percent = computed(() => {
    const total = this.total();
    return total > 0 ? Math.round((this.done() / total) * 100) : 0;
  });

  /** New pages = everything in the plan that is not already here from an earlier run. */
  readonly newPages = computed(() => {
    const plan = this.plan();
    return plan ? Math.max(0, plan.pages - plan.alreadyImported) : 0;
  });

  /**
   * The destination, named. Picking nothing does NOT mean the notes root — the backend files an
   * unfiled import into a per-source container ("Imported from Notion", ...), and this label used
   * to say "Notes (unfiled)" regardless. That was the whole bug the user reported as "I can't see
   * them": the import was filed correctly and the UI pointed at the wrong place.
   *
   * The name comes from the scan report rather than a copy of the backend's map here, so the two
   * cannot drift. Before a scan there is no source chosen and nothing to promise.
   */
  readonly targetLabel = computed(() => {
    const id = this.targetFolder();
    const fallback = this.plan()?.defaultDestination ?? "Notes";
    if (!id) {
      return fallback;
    }
    return this.folders().find((f) => f.id === id)?.name ?? fallback;
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

  /** Switch source, discarding a plan that belonged to the previous one. */
  selectSource(id: ImportSourceId): void {
    if (id === this.sourceId()) {
      return;
    }
    this.sourceId.set(id);
    this.sourcePath.set(null);
    this.plan.set(null);
    this.report.set(null);
    this.phase.set("pick");
  }

  /** Choose an export archive (Notion). */
  async chooseArchive(): Promise<void> {
    await this.pick({
      directory: false,
      multiple: false,
      filters: [{ name: "Export archive", extensions: ["zip"] }],
    });
  }

  /** Choose a folder — an unpacked Notion export, or an Obsidian vault. */
  async chooseFolder(): Promise<void> {
    await this.pick({ directory: true, multiple: false });
  }

  /**
   * Open the native picker and take the result.
   *
   * The try/catch is not defensive noise: a rejected `open()` in a click handler produces an
   * unhandled rejection and a button that visibly does NOTHING, which is indistinguishable from a
   * dead control. Surfacing the reason is the difference between a bug report that can be acted on
   * and "it doesn't work".
   */
  private async pick(options: Parameters<typeof open>[0]): Promise<void> {
    try {
      const chosen = await open(options);
      // A cancelled dialog returns null — a normal outcome, not an error.
      if (typeof chosen === "string") {
        this.selectPath(chosen);
      }
    } catch (e) {
      this.toast.danger(`Could not open the file picker: ${errorText(e)}`);
    }
  }

  private selectPath(path: string): void {
    this.sourcePath.set(path);
    this.plan.set(null);
    this.report.set(null);
    this.phase.set("pick");
    void this.scan();
  }

  /** DRY RUN — writes nothing. */
  async scan(): Promise<void> {
    if (this.needsPick() && !this.sourcePath()) {
      return;
    }
    this.scanning.set(true);
    try {
      const plan = await this.ipc.scanImport(this.sourceId(), this.sourcePath());
      this.plan.set(plan);
      this.phase.set("planned");
      if (plan.pages === 0) {
        this.toast.danger("Nothing to import was found there.");
      }
    } catch (e) {
      this.toast.danger(`Could not read that source: ${errorText(e)}`);
      this.phase.set("pick");
    } finally {
      this.scanning.set(false);
    }
  }

  async runImport(): Promise<void> {
    if (this.phase() === "running") {
      return;
    }
    this.phase.set("running");
    this.stage.set("scanning");
    this.done.set(0);
    this.total.set(this.plan()?.pages ?? 0);
    try {
      const report = await this.ipc.runImport(
        this.sourceId(),
        this.sourcePath(),
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
          `${report.imported} imported, ${report.updated} updated — in ${report.destinationName}.`,
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
    await this.ipc.cancelImport();
  }

  /**
   * Open the folder the import landed in. `selectFolder` is the SHARED note-folder scope the
   * sidebar tree and the notes list both read, so selecting it before navigating means /notes opens
   * already showing the imported notes rather than "All notes".
   *
   * Selecting first, navigating second: the reverse order lands on the list while it is still
   * scoped to whatever was selected before, which flashes the wrong content.
   */
  async openDestination(): Promise<void> {
    const report = this.report();
    if (!report?.destinationId) {
      return;
    }
    await this.notes.selectFolder(report.destinationId);
    await this.router.navigate(["/notes"]);
  }

  /** Back to the start without losing the chosen source. */
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
