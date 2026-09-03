import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  inject,
  signal,
  viewChild,
} from "@angular/core";
import { IpcService } from "../../../../core/ipc.service";
import type {
  AccountStatus,
  FolderNode,
  ShareInboxItem,
} from "../../../../core/models";
import { SharingAuthFlowComponent } from "../../../sharing/sharing-auth-flow/sharing-auth-flow.component";
import { ErrorCopyService } from "../../../../core/copy/error-copy.service";
import { AccountSessionService } from "../../../../services/account-session.service";
import { DateFormatService } from "../../../../core/date-format.service";

/** A flattened, depth-indented folder option for the accept-into picker. */
interface FolderOption {
  node: FolderNode;
  depth: number;
}

/**
 * Settings → Account section (M3-CLIENT): the sharing account behind zero-knowledge
 * note LINK shares. Lean, signals-first surface mirroring the sibling
 * settings-privacy-section shape (`:host { display: contents }` + `.section-stack`
 * + frosted `.card`, global `.btn`/`.btn-primary`/`.btn-ghost`, `var(--token)`).
 *
 * REBUILT: the cramped all-at-once login+signup form (which never issued the
 * send-code call → the "broken signup" bug) is gone. When signed out, a single
 * primary button opens the SAME reusable `<app-sharing-auth-flow>` used by the
 * `/welcome` gateway — here inside an OPAQUE modal (`var(--surface-overlay)`,
 * `backdrop-filter: none`, `border-strong`, `shadow-lg` — trap T3, never the
 * frosted `.card`). The signed-in state, the sharing-server editor, and the M5
 * incoming-shares inbox are unchanged.
 *
 * Everything talks to the Rust core through {@link IpcService}: `accountStatus`
 * loads once into a signal on init; the flow's `completed` output triggers a
 * reload that flips the section to its signed-in state.
 */
@Component({
  selector: "app-settings-account-section",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [SharingAuthFlowComponent],
  templateUrl: "./settings-account-section.component.html",
  styleUrl: "./settings-account-section.component.scss",
})
export class SettingsAccountSectionComponent {
  private readonly dates = inject(DateFormatService);

  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);
  private readonly errorCopy = inject(ErrorCopyService);
  private readonly accountSession = inject(AccountSessionService);

  /** The current sharing-account session; `null` until the first load resolves. */
  private readonly _status = signal<AccountStatus | null>(null);
  readonly status = this._status.asReadonly();

  /**
   * True once the first `reload()` has settled (success OR failure). Distinct
   * from `status()` itself: a resolved-but-falsy status (e.g. an unmocked
   * `account_status` call in the demo/screenshot world falling through to a
   * benign `null`) must NOT read as "still loading" forever — only render the
   * "Loading account…" copy before the first settle, mirroring the
   * `loaded`-signal shape in `settings-organization-section.component.ts`.
   */
  private readonly _loaded = signal(false);
  readonly loaded = this._loaded.asReadonly();

  /** Derived signed-in check the template gates on (post-`loaded()`). */
  readonly isLoggedIn = computed(() => !!this._status()?.loggedIn);

  /** True while a logout IPC call is in flight (debounces the Sign out button). */
  private readonly _busy = signal(false);
  readonly busy = this._busy.asReadonly();

  /** A general account error (status load / logout failure). */
  private readonly _accountError = signal<string | null>(null);
  readonly accountError = this._accountError.asReadonly();

  /** True while the one-tap Touch ID unlock IPC is in flight ("Unlocking…"). */
  private readonly _unlocking = signal(false);
  readonly unlocking = this._unlocking.asReadonly();

  /** A friendly, non-crashy message shown when the Touch ID unlock fails. */
  private readonly _unlockError = signal<string | null>(null);
  readonly unlockError = this._unlockError.asReadonly();

  /**
   * Latches true when a Touch ID unlock attempt fails, so the row falls back to
   * the password "Sign in to share" path instead of looping the biometric sheet.
   * Cleared on the next status reload (a fresh state re-enables Touch ID).
   */
  private readonly _biometricFailed = signal(false);

  /**
   * Whether to offer the one-tap Touch ID unlock: logged in, NOT yet unlocked
   * this session, a cached account key exists, and no prior attempt just failed.
   * When false the password sign-in flow is the fallback.
   */
  readonly canBiometricUnlock = computed(() => {
    const st = this._status();
    return (
      !!st &&
      st.loggedIn &&
      !st.unlockedForSharing &&
      st.biometricUnlockAvailable &&
      !this._biometricFailed()
    );
  });

  /** Whether the reusable-flow modal is open. */
  private readonly _showFlow = signal(false);
  readonly showFlow = this._showFlow.asReadonly();

  /** The modal panel — focused on open so Escape works immediately. */
  private readonly flowPanel =
    viewChild<ElementRef<HTMLElement>>("flowPanel");

  // ── Server base URL ──────────────────────────────────────────────────────

  // ── Incoming shares (M5-CLIENT inbox) ────────────────────────────────────
  private readonly _inbox = signal<ShareInboxItem[]>([]);
  readonly inbox = this._inbox.asReadonly();
  private readonly _inboxLoading = signal(false);
  readonly inboxLoading = this._inboxLoading.asReadonly();
  private readonly _inboxError = signal<string | null>(null);
  readonly inboxError = this._inboxError.asReadonly();
  /** The shareId currently being accepted/declined (locks the whole list). */
  private readonly _busyShare = signal<string | null>(null);
  readonly busyShare = this._busyShare.asReadonly();
  /** The shareId whose folder picker is open (null = none). */
  private readonly _pickerFor = signal<string | null>(null);
  readonly pickerFor = this._pickerFor.asReadonly();
  /** The last accepted row → its returned title (rendered inline on that row). */
  private readonly _acceptedTitle = signal<{ id: string; title: string } | null>(
    null,
  );
  readonly acceptedTitle = this._acceptedTitle.asReadonly();
  /** The last row error (rendered inline on that row). */
  private readonly _rowError = signal<{ id: string; msg: string } | null>(null);
  readonly rowError = this._rowError.asReadonly();
  /** Open folders (locked ones excluded — accepting into a sealed folder fails). */
  private readonly _folders = signal<FolderNode[]>([]);

  /** Flattened, depth-indented OPEN folders for the accept-into picker. */
  readonly folderOptions = computed<FolderOption[]>(() => {
    const out: FolderOption[] = [];
    const walk = (nodes: FolderNode[], depth: number): void => {
      for (const node of nodes) {
        if (!node.locked) {
          out.push({ node, depth });
        }
        if (node.children?.length) {
          walk(node.children, depth + 1);
        }
      }
    };
    walk(this._folders(), 0);
    return out;
  });

  constructor() {
    // Fire-and-forget one-shot load — the section fetches its state exactly once
    // on construction (no signal is read, so no NG0600 / no effect needed).
    void this.reload();
  }

  /** Load the current account status + seed the server URL input from config. */
  private async reload(): Promise<void> {
    // A fresh status re-enables the Touch ID path (drops any prior fail latch).
    this._biometricFailed.set(false);
    this._unlockError.set(null);
    let st: AccountStatus | null = null;
    try {
      st = await this.ipc.accountStatus();
      this._status.set(st);
      if (st) {
        this.accountSession.accept(st);
      }
    } catch (e) {
      this._accountError.set(this.errorCopy.humanize(e));
    } finally {
      this._loaded.set(true);
    }
    // Load the incoming-share inbox only when it's usable (signed in + a server).
    if (st?.loggedIn && st.serverConfigured) {
      await this.loadInbox();
    } else {
      this._inbox.set([]);
    }
  }

  /**
   * Load the incoming-share inbox + the open-folder options for the picker.
   * Fires the on-launch `shareRewrapPending` best-effort (advances any pending
   * outgoing invites whose recipient has since registered) — errors ignored.
   */
  private async loadInbox(): Promise<void> {
    this._inboxLoading.set(true);
    this._inboxError.set(null);
    // Fire-and-forget: advance pending invites; never blocks / breaks the inbox.
    void this.ipc.shareRewrapPending().catch(() => undefined);
    try {
      this._inbox.set(await this.ipc.listShareInbox());
    } catch (e) {
      this._inboxError.set(this.errorCopy.humanize(e));
    } finally {
      this._inboxLoading.set(false);
    }
    try {
      this._folders.set(await this.ipc.listFolders());
    } catch {
      // Leave the picker with just the default "Shared" folder option.
    }
  }

  // ── The reusable flow (modal host) ───────────────────────────────────────

  /** Open the OPAQUE modal + focus its panel so Escape/keyboard work at once. */
  openFlow(): void {
    this._accountError.set(null);
    this._showFlow.set(true);
    afterNextRender(() => this.flowPanel()?.nativeElement.focus(), {
      injector: this.injector,
    });
  }

  /** Dismissed (user backed out) — just close. */
  closeFlow(): void {
    this._showFlow.set(false);
  }

  /** Completed (signed in) — close + reload so the section flips to signed-in. */
  async onFlowDone(): Promise<void> {
    this._showFlow.set(false);
    await this.reload();
  }


  /**
   * One-tap Touch ID unlock for sharing: presents a single biometric sheet,
   * restores the session MK, and flips the row to "Ready to share". On ANY
   * failure it fails closed to the password sign-in flow (latch the fallback +
   * surface a friendly message) — never a dead end.
   */
  async unlockWithBiometric(): Promise<void> {
    if (this._unlocking()) return;
    this._unlocking.set(true);
    this._unlockError.set(null);
    try {
      const st = await this.ipc.unlockSharingWithBiometric();
      this._status.set(st);
      this.accountSession.accept(st);
      if (!st.unlockedForSharing) {
        // Resolved but still locked — fall back to the password path.
        this._biometricFailed.set(true);
        this._unlockError.set(
          "Couldn't unlock this session. Sign in with your password to share.",
        );
      }
    } catch (e) {
      this._biometricFailed.set(true);
      // Cancel-vs-failure now comes from the `[touch-id-*]` code, not from the word "cancel"
      // appearing in the keychain's own sentence. The password fall-back is unchanged.
      this._unlockError.set(this.errorCopy.humanize(e, "account"));
    } finally {
      this._unlocking.set(false);
    }
  }

  /** Sign out (server family-revoke + clear tokens + drop session MK), then reload. */
  async logout(): Promise<void> {
    if (this._busy()) return;
    this._busy.set(true);
    try {
      await this.ipc.accountLogout();
      this.accountSession.acceptLoggedOut();
      await this.reload();
    } catch (e) {
      this._accountError.set(this.errorCopy.humanize(e));
    } finally {
      this._busy.set(false);
    }
  }

  // ── Incoming shares: accept / decline ────────────────────────────────────

  /** Toggle the OPAQUE accept-into folder picker for a row. */
  togglePicker(shareId: string): void {
    this._pickerFor.update((cur) => (cur === shareId ? null : shareId));
  }

  /**
   * Accept an incoming share into `folderId` (omitted → the auto "Shared"
   * folder). On success shows the returned title; a thrown error (sealed target
   * → "locked …", or a verification failure) surfaces inline on the row.
   */
  async accept(item: ShareInboxItem, folderId?: string): Promise<void> {
    if (this._busyShare() !== null) {
      return;
    }
    this._busyShare.set(item.shareId);
    this._pickerFor.set(null);
    this._rowError.set(null);
    this._acceptedTitle.set(null);
    try {
      const res = await this.ipc.acceptShare(item.shareId, folderId);
      this._acceptedTitle.set({ id: item.shareId, title: res.title });
      await this.loadInbox();
    } catch (e) {
      this._rowError.set({
        id: item.shareId,
        msg: this.errorCopy.humanize(e, "account"),
      });
    } finally {
      this._busyShare.set(null);
    }
  }

  /** Decline an incoming share, then refresh the list. */
  async decline(item: ShareInboxItem): Promise<void> {
    if (this._busyShare() !== null) {
      return;
    }
    this._busyShare.set(item.shareId);
    this._pickerFor.set(null);
    this._rowError.set(null);
    try {
      await this.ipc.declineShare(item.shareId);
      await this.loadInbox();
    } catch (e) {
      this._rowError.set({
        id: item.shareId,
        msg: this.errorCopy.humanize(e, "account"),
      });
    } finally {
      this._busyShare.set(null);
    }
  }

  /** Presentational: render an ISO timestamp as a friendly local date. */
  /** Formatted through {@link DateFormatService} — the one place a date becomes user-visible text. */
  formatDate(iso: string): string {
    return this.dates.day(iso);
  }

  /** Presentational: render a byte count as a compact size. */
  formatSize(bytes: number): string {
    if (bytes < 1024) {
      return `${bytes} B`;
    }
    if (bytes < 1024 * 1024) {
      return `${(bytes / 1024).toFixed(1)} KB`;
    }
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  }
}
