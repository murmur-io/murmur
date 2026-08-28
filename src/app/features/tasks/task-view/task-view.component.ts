import {
  ChangeDetectionStrategy,
  Component,
  computed,
  effect,
  inject,
  signal,
  untracked,
} from "@angular/core";
import { toSignal } from "@angular/core/rxjs-interop";
import { ActivatedRoute, Router } from "@angular/router";
import type {
  DashboardSummary,
  NoteAttachmentDto,
  NoteAttachmentOwnerKind,
  OrgTask,
  TaskDraft,
  TaskImageRef,
  TaskLocalRef,
  TaskStatus,
  TaskSubtask,
} from "../../../core/models";
import { IpcService } from "../../../core/ipc.service";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";
import { MurCopyIdComponent } from "../../../design-system/copy-id/copy-id.component";
import { MurEmptyStateComponent } from "../../../design-system/empty-state/empty-state.component";
import { MurSpinnerComponent } from "../../../design-system/spinner/spinner.component";
import { SharingAuthFlowComponent } from "../../sharing/sharing-auth-flow/sharing-auth-flow.component";
import { NoteAttachmentService } from "../../../services/note-attachment.service";
import { TaskStore } from "../task.store";

type TaskFilter = "open" | TaskStatus | "all";
/** Which door of the shared account flow the signed-out gate opened. */
type AuthDoor = "signin" | "create";

@Component({
  selector: "app-task-view",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    MurCopyIdComponent,
    MurEmptyStateComponent,
    MurSpinnerComponent,
    SharingAuthFlowComponent,
  ],
  templateUrl: "./task-view.component.html",
  styleUrl: "./task-view.component.scss",
})
export class TaskViewComponent {
  readonly store = inject(TaskStore);
  private readonly ipc = inject(IpcService);
  private readonly router = inject(Router);
  private readonly route = inject(ActivatedRoute);
  private readonly attachmentsApi = inject(NoteAttachmentService);
  private readonly errors = inject(ErrorCopyService);

  private readonly params = toSignal(this.route.paramMap);
  private readonly routeSegments = toSignal(this.route.url, { initialValue: this.route.snapshot.url });
  readonly routeId = computed(() => this.params()?.get("id") ?? null);
  readonly isNew = computed(() => this.routeSegments().some((segment) => segment.path === "new"));
  readonly showingDetail = computed(() => this.isNew() || this.routeId() !== null);
  readonly current = computed(() => {
    const id = this.routeId();
    return id ? (this.store.tasks().find((task) => task.id === id) ?? null) : null;
  });

  readonly filter = signal<TaskFilter>("open");
  readonly orgFilter = signal<string>("all");
  readonly query = signal("");
  readonly filteredTasks = computed(() => {
    const query = this.query().trim().toLocaleLowerCase();
    const filter = this.filter();
    const org = this.orgFilter();
    return this.store.tasks().filter((task) => {
      if (org !== "all" && task.orgId !== org) return false;
      if (filter === "open" && task.status === "done") return false;
      if (filter !== "all" && filter !== "open" && task.status !== filter) return false;
      return !query || `${task.title} ${task.description}`.toLocaleLowerCase().includes(query);
    });
  });

  readonly title = signal("");
  readonly description = signal("");
  readonly status = signal<TaskStatus>("todo");
  readonly dueAt = signal("");
  readonly assigneeUserId = signal("");
  readonly orgId = signal("");
  readonly access = signal<"view" | "edit">("edit");
  readonly subtasks = signal<TaskSubtask[]>([]);
  readonly orgRefs = signal<{ orgId: string; docId: string }[]>([]);
  readonly images = signal<TaskImageRef[]>([]);
  readonly localRefs = signal<TaskLocalRef[]>([]);
  readonly attachments = signal<NoteAttachmentDto[]>([]);
  readonly dashboards = signal<DashboardSummary[]>([]);
  readonly saving = signal(false);
  readonly deleting = signal(false);
  readonly uploading = signal(false);
  readonly localError = signal<string | null>(null);
  readonly dirty = signal(false);
  readonly error = computed(() => this.localError() ?? this.store.error());
  readonly assignees = computed(() => this.store.assignees()[this.orgId()] ?? []);
  readonly relatedCandidates = computed(() =>
    this.store
      .tasks()
      .filter(
        (task) =>
          task.orgId === this.orgId() &&
          task.id !== this.current()?.id &&
          !this.orgRefs().some((ref) => ref.docId === task.docId),
      ),
  );
  readonly canSave = computed(
    () =>
      this.title().trim().length > 0 &&
      this.orgId().length > 0 &&
      !this.saving() &&
      (this.isNew() || this.current()?.canEdit === true),
  );

  /**
   * The account door the signed-out gate has opened, or `null` while it only shows the two CTAs.
   *
   * `SharingAuthFlowComponent` is `:host { display: contents }` and paints no surface of its own,
   * so it is embedded INLINE in the empty state rather than in a floating panel — which keeps this
   * clear of the opaque-overlay trap entirely instead of re-deriving a second modal.
   */
  readonly authDoor = signal<AuthDoor | null>(null);

  readonly newSubtask = signal("");
  readonly relatedTaskId = signal("");
  readonly localRefKind = signal<TaskLocalRef["kind"]>("dashboard");
  readonly localRefId = signal("");
  readonly dashboardRefId = signal("");
  private loadedStamp: string | null = null;
  private attachmentToken = 0;
  private seenScrubEpoch = 0;

  constructor() {
    void this.store.init();
    void this.loadDashboards();

    effect(() => {
      const scrubEpoch = this.store.scrubEpoch();
      if (scrubEpoch === this.seenScrubEpoch) return;
      this.seenScrubEpoch = scrubEpoch;
      untracked(() => this.scrubDraft());
    });

    effect(() => {
      const isNew = this.isNew();
      const task = this.current();
      const orgs = this.store.orgs();
      untracked(() => {
        if (isNew) {
          if (this.loadedStamp !== "new") {
            this.loadedStamp = "new";
            this.resetDraft(orgs[0]?.orgId ?? "");
          } else if (!this.orgId() && orgs[0]) {
            this.orgId.set(orgs[0].orgId);
          }
          return;
        }
        if (!task || (this.dirty() && this.loadedStamp?.startsWith(task.id))) return;
        const stamp = `${task.id}:${task.updatedAt}:${task.itemId}`;
        if (stamp === this.loadedStamp) return;
        this.loadedStamp = stamp;
        this.populate(task);
      });
    });

    effect(() => {
      const orgId = this.orgId();
      if (orgId) untracked(() => void this.store.loadAssignees(orgId));
    });
  }

  openAuth(door: AuthDoor): void {
    this.authDoor.set(door);
  }

  closeAuth(): void {
    this.authDoor.set(null);
  }

  /**
   * A finished sign-in/sign-up. Re-run the authoritative read: it is the read itself that decides
   * whether the gate stays up, so nothing here guesses at the new session state.
   */
  onAuthCompleted(): void {
    this.authDoor.set(null);
    void this.store.reload();
  }

  selectFilter(value: TaskFilter): void {
    this.filter.set(value);
  }

  onQuery(event: Event): void {
    this.query.set((event.target as HTMLInputElement).value);
  }

  onOrgFilter(event: Event): void {
    this.orgFilter.set((event.target as HTMLSelectElement).value);
  }

  open(task: OrgTask): void {
    void this.router.navigate(["/tasks", task.id]);
  }

  newTask(): void {
    void this.router.navigateByUrl("/tasks/new");
  }

  backToList(): void {
    void this.router.navigateByUrl("/tasks");
  }

  setText(field: "title" | "description", event: Event): void {
    const value = (event.target as HTMLInputElement | HTMLTextAreaElement).value;
    if (field === "title") this.title.set(value);
    else this.description.set(value);
    this.dirty.set(true);
  }

  setStatus(event: Event): void {
    this.status.set((event.target as HTMLSelectElement).value as TaskStatus);
    this.dirty.set(true);
  }

  setDueAt(event: Event): void {
    this.dueAt.set((event.target as HTMLInputElement).value);
    this.dirty.set(true);
  }

  setAssignee(event: Event): void {
    this.assigneeUserId.set((event.target as HTMLSelectElement).value);
    this.dirty.set(true);
  }

  setOrg(event: Event): void {
    this.orgId.set((event.target as HTMLSelectElement).value);
    this.assigneeUserId.set("");
    this.orgRefs.set([]);
    this.dirty.set(true);
  }

  async setAccess(event: Event): Promise<void> {
    const next = (event.target as HTMLSelectElement).value as "view" | "edit";
    const task = this.current();
    if (!task) {
      this.access.set(next);
      this.dirty.set(true);
      return;
    }
    if (!task.canManage || next === task.access) return;
    if (await this.store.setAccess(task, next)) this.access.set(next);
  }

  onNewSubtask(event: Event): void {
    this.newSubtask.set((event.target as HTMLInputElement).value);
  }

  addSubtask(): void {
    const title = this.newSubtask().trim();
    if (!title) return;
    this.subtasks.update((rows) => [
      ...rows,
      { id: crypto.randomUUID(), title, done: false },
    ]);
    this.newSubtask.set("");
    this.dirty.set(true);
  }

  toggleSubtask(id: string): void {
    this.subtasks.update((rows) =>
      rows.map((row) => (row.id === id ? { ...row, done: !row.done } : row)),
    );
    this.dirty.set(true);
  }

  removeSubtask(id: string): void {
    this.subtasks.update((rows) => rows.filter((row) => row.id !== id));
    this.dirty.set(true);
  }

  setRelatedTask(event: Event): void {
    this.relatedTaskId.set((event.target as HTMLSelectElement).value);
  }

  addRelatedTask(): void {
    const task = this.store.tasks().find((row) => row.id === this.relatedTaskId());
    if (!task || task.orgId !== this.orgId()) return;
    this.orgRefs.update((rows) => [...rows, { orgId: task.orgId, docId: task.docId }]);
    this.relatedTaskId.set("");
    this.dirty.set(true);
  }

  removeRelated(docId: string): void {
    this.orgRefs.update((rows) => rows.filter((row) => row.docId !== docId));
    this.dirty.set(true);
  }

  relatedTitle(docId: string): string {
    return (
      this.store.tasks().find((task) => task.orgId === this.orgId() && task.docId === docId)
        ?.title ?? "Shared task"
    );
  }

  openRelated(ref: { orgId: string; docId: string }): void {
    void this.router.navigate(["/tasks", `${ref.orgId}:${ref.docId}`]);
  }

  setLocalRefKind(event: Event): void {
    this.localRefKind.set((event.target as HTMLSelectElement).value as TaskLocalRef["kind"]);
  }

  setLocalRefId(event: Event): void {
    this.localRefId.set((event.target as HTMLInputElement).value);
  }

  setDashboardRef(event: Event): void {
    this.dashboardRefId.set((event.target as HTMLSelectElement).value);
  }

  async addDashboardRef(): Promise<void> {
    const refId = this.dashboardRefId();
    if (!refId) return;
    await this.persistLocalRefs([...this.localRefs(), { kind: "dashboard", refId }]);
    this.dashboardRefId.set("");
  }

  async addLocalRef(): Promise<void> {
    const refId = this.localRefId().trim();
    if (!refId) return;
    await this.persistLocalRefs([...this.localRefs(), { kind: this.localRefKind(), refId }]);
    this.localRefId.set("");
  }

  async removeLocalRef(ref: TaskLocalRef): Promise<void> {
    await this.persistLocalRefs(
      this.localRefs().filter((row) => row.kind !== ref.kind || row.refId !== ref.refId),
    );
  }

  openLocalRef(ref: TaskLocalRef): void {
    const path =
      ref.kind === "note"
        ? ["/notes", ref.refId]
        : ref.kind === "meeting"
          ? ["/meeting", ref.refId]
          : ["/dashboards", ref.refId];
    void this.router.navigate(path);
  }

  async addImages(event: Event): Promise<void> {
    const task = this.current();
    const input = event.target as HTMLInputElement;
    if (!task || !input.files?.length || !task.canEdit) return;
    const available = Math.max(0, 16 - this.images().length);
    const plan = this.attachmentsApi.planFromFiles(input.files, available);
    input.value = "";
    if (plan.skippedUnsupportedImages || plan.skippedTooManyImages) {
      this.localError.set("Choose up to 16 PNG, JPEG, or WebP images.");
    }
    const owner = this.taskAttachmentOwner(task);
    this.uploading.set(true);
    try {
      for (const segment of plan.segments) {
        if (segment.kind !== "image") continue;
        const attachment = await this.attachmentsApi.importImage(
          owner.kind,
          owner.id,
          segment.image,
        );
        this.attachments.update((rows) => [...rows, attachment]);
        this.images.update((rows) => [
          ...rows,
          {
            reference: this.attachmentsApi.attachmentMarkdown(
              attachment,
              segment.image.alt,
            ),
            alt: segment.image.alt,
          },
        ]);
        this.dirty.set(true);
      }
    } catch (error) {
      this.localError.set(this.errors.humanize(error));
    } finally {
      this.uploading.set(false);
    }
  }

  removeImage(image: TaskImageRef): void {
    this.images.update((rows) => rows.filter((row) => row.reference !== image.reference));
    this.dirty.set(true);
  }

  imageData(image: TaskImageRef): string | null {
    const id = this.attachmentId(image.reference);
    return id ? (this.attachments().find((row) => row.id === id)?.dataUrl ?? null) : null;
  }

  async save(): Promise<void> {
    if (!this.canSave()) return;
    this.saving.set(true);
    this.localError.set(null);
    try {
      const current = this.current();
      const beforeImages = current?.images ?? [];
      const result = current
        ? await this.store.update(current.id, this.draft())
        : await this.store.create(this.draft());
      if (!result) return;
      this.dirty.set(false);
      if (current) await this.deleteRemovedImages(current, beforeImages, this.images());
      await this.router.navigate(["/tasks", result.id]);
    } finally {
      this.saving.set(false);
    }
  }

  async remove(): Promise<void> {
    const task = this.current();
    if (!task?.canManage || this.deleting()) return;
    if (!window.confirm(`Delete “${task.title}” from ${this.orgName(task.orgId)}?`)) return;
    this.deleting.set(true);
    try {
      if (await this.store.remove(task.id)) await this.router.navigateByUrl("/tasks");
    } finally {
      this.deleting.set(false);
    }
  }

  orgName(orgId: string): string {
    return this.store.orgs().find((org) => org.orgId === orgId)?.name ?? "Organization";
  }

  formatDue(task: OrgTask): string {
    if (!task.dueAt) return "No due date";
    const date = new Date(task.dueAt);
    return Number.isNaN(date.getTime())
      ? task.dueAt
      : new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" }).format(date);
  }

  private draft(): TaskDraft {
    return {
      orgId: this.orgId(),
      title: this.title().trim(),
      description: this.description(),
      status: this.status(),
      dueAt: this.fromLocalDateTime(this.dueAt()),
      assigneeUserId: this.assigneeUserId() || null,
      subtasks: this.subtasks(),
      orgRefs: this.orgRefs(),
      images: this.images(),
      access: this.access(),
    };
  }

  private populate(task: OrgTask): void {
    this.title.set(task.title);
    this.description.set(task.description);
    this.status.set(task.status);
    this.dueAt.set(this.toLocalDateTime(task.dueAt));
    this.assigneeUserId.set(task.assigneeUserId ?? "");
    this.orgId.set(task.orgId);
    this.access.set(task.access);
    this.subtasks.set(task.subtasks.map((row) => ({ ...row })));
    this.orgRefs.set(task.orgRefs.map((row) => ({ ...row })));
    this.images.set(task.images.map((row) => ({ ...row })));
    this.localRefs.set(task.localRefs.map((row) => ({ ...row })));
    this.dirty.set(false);
    void this.loadAttachments(task);
  }

  private resetDraft(orgId: string): void {
    this.title.set("");
    this.description.set("");
    this.status.set("todo");
    this.dueAt.set("");
    this.assigneeUserId.set("");
    this.orgId.set(orgId);
    this.access.set("edit");
    this.subtasks.set([]);
    this.orgRefs.set([]);
    this.images.set([]);
    this.localRefs.set([]);
    this.attachments.set([]);
    this.dirty.set(false);
  }

  private scrubDraft(): void {
    this.loadedStamp = null;
    ++this.attachmentToken;
    this.resetDraft("");
    this.newSubtask.set("");
    this.relatedTaskId.set("");
    this.localRefId.set("");
    this.dashboardRefId.set("");
    this.localError.set(null);
  }

  private async loadDashboards(): Promise<void> {
    try {
      this.dashboards.set(await this.ipc.listDashboards());
    } catch {
      // Device-private dashboard refs are optional; task editing remains available.
    }
  }

  private async loadAttachments(task: OrgTask): Promise<void> {
    const token = ++this.attachmentToken;
    const owner = this.taskAttachmentOwner(task);
    try {
      const rows = await this.ipc.listNoteAttachments(owner.kind, owner.id);
      if (token === this.attachmentToken && this.current()?.id === task.id) {
        this.attachments.set(rows);
      }
    } catch {
      if (token === this.attachmentToken) this.attachments.set([]);
    }
  }

  private taskAttachmentOwner(task: OrgTask): { kind: NoteAttachmentOwnerKind; id: string } {
    return task.sourceDocumentId
      ? { kind: "task", id: task.sourceDocumentId }
      : { kind: "org", id: task.itemId };
  }

  private async deleteRemovedImages(
    task: OrgTask,
    before: readonly TaskImageRef[],
    after: readonly TaskImageRef[],
  ): Promise<void> {
    const retained = new Set(after.map((row) => row.reference));
    const owner = this.taskAttachmentOwner(task);
    for (const row of before) {
      if (retained.has(row.reference)) continue;
      const id = this.attachmentId(row.reference);
      if (!id) continue;
      try {
        await this.attachmentsApi.deleteAttachment(owner.kind, owner.id, id);
      } catch {
        // The shared revision is already authoritative; orphan cleanup is best-effort only.
      }
    }
  }

  private async persistLocalRefs(rows: TaskLocalRef[]): Promise<void> {
    const task = this.current();
    if (!task) return;
    const dedup = rows.filter(
      (row, index) =>
        rows.findIndex((candidate) => candidate.kind === row.kind && candidate.refId === row.refId) ===
        index,
    );
    if (await this.store.setLocalRefs(task.id, dedup)) this.localRefs.set(dedup);
  }

  private attachmentId(reference: string): string | null {
    return /murmur-attachment:\/\/([0-9a-f-]{36})\)/i.exec(reference)?.[1] ?? null;
  }

  private toLocalDateTime(value: string | null): string {
    if (!value) return "";
    const date = new Date(value);
    if (Number.isNaN(date.getTime())) return "";
    const local = new Date(date.getTime() - date.getTimezoneOffset() * 60_000);
    return local.toISOString().slice(0, 16);
  }

  private fromLocalDateTime(value: string): string | null {
    if (!value) return null;
    const date = new Date(value);
    return Number.isNaN(date.getTime()) ? null : date.toISOString();
  }
}
