import {
  ChangeDetectionStrategy,
  Component,
  computed,
  inject,
  signal,
} from "@angular/core";
import { IpcService } from "../../../../core/ipc.service";
import type {
  OrgMember,
  OrgStatus,
  OrgSyncReport,
} from "../../../../core/models";

/**
 * Settings → Organization section (Shared Brain v1). Manages the org behind the
 * org-wide E2EE shared brain. Mirrors the sibling `settings-account-section`
 * shape (`:host { display: contents }` + `.section-stack` + frosted `.card`,
 * global `.btn`/`.btn-primary`/`.btn-ghost`, `var(--token)`).
 *
 * Two states, driven by `orgStatus`:
 *  - NO org → a create form (name → `orgCreate`).
 *  - IN an org → the org name/role, the member list (owner: invite by email +
 *    remove), Leave, the org-egress consent toggle, and the sync status
 *    (lastSeq / itemCount) with a "Sync now" (`orgSyncNow`).
 *
 * Everything talks to the Rust core through {@link IpcService}. `orgStatus` loads
 * once into a signal on construction; every mutation reloads it. The member list
 * loads only for the OWNER (the member management surface). The org-egress
 * consent is a dedicated command (`consentToOrgEgress` / `revokeOrgEgress`),
 * PRESERVE-ONLY config like the share-egress consent — never part of a config save.
 */
@Component({
  selector: "app-settings-organization-section",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./settings-organization-section.component.html",
  styleUrl: "./settings-organization-section.component.scss",
})
export class SettingsOrganizationSectionComponent {
  private readonly ipc = inject(IpcService);

  /** The org membership + sync state; `null` = in no org (show the create form). */
  private readonly _status = signal<OrgStatus | null>(null);
  readonly status = this._status.asReadonly();

  /** True until the first `orgStatus` load resolves (avoids a create-form flash). */
  private readonly _loaded = signal(false);
  readonly loaded = this._loaded.asReadonly();

  /** A general org error (load / mutate failure). */
  private readonly _error = signal<string | null>(null);
  readonly error = this._error.asReadonly();

  /** True during any in-flight mutation (debounces the buttons). */
  private readonly _busy = signal(false);
  readonly busy = this._busy.asReadonly();

  readonly isOwner = computed(() => this._status()?.role === "owner");

  // ── Create form ───────────────────────────────────────────────────────────
  readonly createName = signal("");
  readonly creating = signal(false);

  // ── Members (owner only) ──────────────────────────────────────────────────
  private readonly _members = signal<OrgMember[]>([]);
  readonly members = this._members.asReadonly();
  private readonly _membersError = signal<string | null>(null);
  readonly membersError = this._membersError.asReadonly();
  /** The invite-by-email draft + in-flight flag. */
  readonly inviteEmail = signal("");
  readonly inviting = signal(false);
  /** The user id currently being removed (locks that row). */
  private readonly _removingId = signal<string | null>(null);
  readonly removingId = this._removingId.asReadonly();

  /** Active members render first; removed (historical) ones fall to the bottom. */
  readonly sortedMembers = computed(() =>
    [...this._members()].sort((a, b) => Number(a.removed) - Number(b.removed)),
  );

  // ── Consent ───────────────────────────────────────────────────────────────
  readonly consented = computed(() => this._status()?.consented ?? false);
  readonly consentBusy = signal(false);

  // ── Sync ──────────────────────────────────────────────────────────────────
  readonly syncing = signal(false);
  private readonly _lastSync = signal<OrgSyncReport | null>(null);
  readonly lastSync = this._lastSync.asReadonly();

  constructor() {
    // Fire-and-forget one-shot load (no signal read → no effect needed).
    void this.reload();
  }

  /** Load the org status + (for an owner) the member list. */
  private async reload(): Promise<void> {
    try {
      const st = await this.ipc.orgStatus();
      this._status.set(st);
      if (st?.role === "owner") {
        await this.loadMembers();
      } else {
        this._members.set([]);
      }
      // On-open responsiveness: pull the org feed once (best-effort) so a freshly-joined member sees
      // teammates' items right away instead of waiting for the next background sync tick. The periodic
      // backend loop keeps it fresh thereafter; this only covers the "just opened Settings" moment.
      if (st) {
        void this.ipc
          .orgSyncNow()
          .then(async () => this._status.set(await this.ipc.orgStatus()))
          .catch(() => undefined);
      }
    } catch (e) {
      this._error.set(String(e));
    } finally {
      this._loaded.set(true);
    }
  }

  private async loadMembers(): Promise<void> {
    this._membersError.set(null);
    try {
      this._members.set(await this.ipc.orgListMembers());
    } catch (e) {
      this._membersError.set(String(e));
    }
  }

  // ── Create ─────────────────────────────────────────────────────────────────

  onCreateNameInput(event: Event): void {
    this.createName.set((event.target as HTMLInputElement).value);
  }

  /** Create the org (this user becomes owner), then reload into the managed state. */
  async createOrg(): Promise<void> {
    const name = this.createName().trim();
    if (!name || this.creating()) {
      return;
    }
    this._error.set(null);
    this.creating.set(true);
    try {
      const st = await this.ipc.orgCreate(name);
      this._status.set(st);
      this.createName.set("");
      if (st.role === "owner") {
        await this.loadMembers();
      }
    } catch (e) {
      this._error.set(String(e));
    } finally {
      this.creating.set(false);
    }
  }

  // ── Members ────────────────────────────────────────────────────────────────

  onInviteEmailInput(event: Event): void {
    this.inviteEmail.set((event.target as HTMLInputElement).value);
  }

  /** Invite a member by email (owner), then refresh the member list. */
  async invite(): Promise<void> {
    const email = this.inviteEmail().trim();
    if (!email || this.inviting()) {
      return;
    }
    this._membersError.set(null);
    this.inviting.set(true);
    try {
      await this.ipc.orgInviteMember(email);
      this.inviteEmail.set("");
      await this.loadMembers();
      await this.refreshStatusCounts();
    } catch (e) {
      this._membersError.set(String(e));
    } finally {
      this.inviting.set(false);
    }
  }

  /** Remove a member (owner) — drives OCK rotation backend-side. Then refresh. */
  async removeMember(member: OrgMember): Promise<void> {
    if (this._removingId() !== null) {
      return;
    }
    this._removingId.set(member.userId);
    this._membersError.set(null);
    try {
      await this.ipc.orgRemoveMember(member.userId);
      await this.loadMembers();
      await this.refreshStatusCounts();
    } catch (e) {
      this._membersError.set(String(e));
    } finally {
      this._removingId.set(null);
    }
  }

  // ── Leave ────────────────────────────────────────────────────────────────

  /** Leave the org (self-removal); the section flips back to the create form. */
  async leave(): Promise<void> {
    if (this._busy()) {
      return;
    }
    this._busy.set(true);
    this._error.set(null);
    try {
      await this.ipc.orgLeave();
      this._status.set(null);
      this._members.set([]);
      this._lastSync.set(null);
    } catch (e) {
      this._error.set(String(e));
    } finally {
      this._busy.set(false);
    }
  }

  // ── Consent ────────────────────────────────────────────────────────────────

  /** Toggle the one-time org-egress consent (dedicated command, preserve-only config). */
  async toggleConsent(): Promise<void> {
    if (this.consentBusy()) {
      return;
    }
    this.consentBusy.set(true);
    this._error.set(null);
    const grant = !this.consented();
    try {
      if (grant) {
        await this.ipc.consentToOrgEgress();
      } else {
        await this.ipc.revokeOrgEgress();
      }
      // Reflect locally so the UI flips without a full reload.
      const st = this._status();
      if (st) {
        this._status.set({ ...st, consented: grant });
      }
    } catch (e) {
      this._error.set(String(e));
    } finally {
      this.consentBusy.set(false);
    }
  }

  // ── Sync ─────────────────────────────────────────────────────────────────

  /** Pull + ingest the org feed now; show the report + refresh the status counts. */
  async syncNow(): Promise<void> {
    if (this.syncing()) {
      return;
    }
    this.syncing.set(true);
    this._error.set(null);
    try {
      const report = await this.ipc.orgSyncNow();
      this._lastSync.set(report);
      await this.refreshStatusCounts();
    } catch (e) {
      this._error.set(String(e));
    } finally {
      this.syncing.set(false);
    }
  }

  /** Re-read `orgStatus` to refresh the live counts (memberCount / lastSeq / itemCount). */
  private async refreshStatusCounts(): Promise<void> {
    try {
      const st = await this.ipc.orgStatus();
      this._status.set(st);
    } catch {
      // Non-fatal — the last-known status stays.
    }
  }

  /** Presentational: an ISO timestamp → a friendly local date. */
  formatDate(iso: string): string {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) {
      return iso;
    }
    return d.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    });
  }
}
