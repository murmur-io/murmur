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
import { FormControl, ReactiveFormsModule } from "@angular/forms";
import { IpcService } from "../../../core/ipc.service";
import type {
  AccountStatus,
  FolderNode,
  ShareInboxItem,
} from "../../../core/models";
import { SharingAuthFlowComponent } from "../../sharing/sharing-auth-flow.component";

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
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule, SharingAuthFlowComponent],
  template: `
    <div class="section-stack">
      <div class="card account-card">
        <div class="account-head">
          <span class="account-mark" aria-hidden="true">
            <svg
              viewBox="0 0 24 24"
              width="26"
              height="26"
              fill="none"
              role="img"
              aria-label="Account"
            >
              <path
                d="M12 2.5 4.5 5.5v5.2c0 4.6 3.1 8.1 7.5 9.3 4.4-1.2 7.5-4.7 7.5-9.3V5.5L12 2.5Z"
                fill="var(--accent-soft)"
                stroke="var(--accent-hover)"
                stroke-width="1.3"
                stroke-linejoin="round"
              />
              <circle
                cx="12"
                cy="10"
                r="2.2"
                fill="none"
                stroke="var(--accent-hover)"
                stroke-width="1.4"
              />
              <path
                d="M8.4 15.4c.7-1.6 2-2.4 3.6-2.4s2.9.8 3.6 2.4"
                fill="none"
                stroke="var(--accent-hover)"
                stroke-width="1.4"
                stroke-linecap="round"
              />
            </svg>
          </span>
          <div class="account-copy">
            <h3>Sharing account</h3>
            <p class="text-secondary account-sub">
              Sign in to share a note as a private, end-to-end-encrypted link.
              The link's <code>#…</code> part is the decryption key and never
              reaches the server.
            </p>
          </div>
        </div>

        <!-- (1) Sharing server base URL -->
        <div class="account-section">
          <span class="account-section-label text-muted">Sharing server</span>
          <p class="text-secondary account-note">
            The server that stores the encrypted note. It can't read your notes —
            only ciphertext and wrapped keys leave this Mac.
          </p>
          <div class="server-row">
            <input
              type="url"
              class="text-input server-input"
              [formControl]="serverControl"
              placeholder="https://share.example.com"
              autocomplete="off"
              spellcheck="false"
              aria-label="Sharing server base URL"
            />
            <button
              type="button"
              class="btn"
              (click)="saveServer()"
              [disabled]="savingServer()"
            >
              {{ savingServer() ? "Saving…" : "Save server" }}
            </button>
          </div>
          @if (serverError(); as serr) {
            <p class="text-danger account-note">{{ serr }}</p>
          }
        </div>

        @if (status(); as st) {
          @if (st.loggedIn) {
            <!-- (2a) Signed-in state -->
            <div class="account-section">
              <span class="account-section-label text-muted">Signed in</span>
              <div class="signed-in-row">
                <span class="pill is-success signed-pill">
                  <span class="pill-dot"></span>
                  {{ st.email ?? "Signed in" }}
                </span>
                <button
                  type="button"
                  class="btn btn-ghost"
                  (click)="logout()"
                  [disabled]="busy()"
                >
                  Sign out
                </button>
              </div>
              <p class="text-secondary account-note">
                Your notes live on this Mac. Signing out only turns off sharing —
                it never deletes, moves, or uploads a note.
              </p>
              @if (!st.unlockedForSharing) {
                <p class="text-secondary account-note">
                  Sign in again to share — your account key isn't loaded in this
                  session.
                </p>
                <div class="account-actions">
                  <button
                    type="button"
                    class="btn btn-primary"
                    (click)="openFlow()"
                  >
                    Sign in to share
                  </button>
                </div>
              }
            </div>
          } @else {
            <!-- (2b) Signed-out: ONE button opens the reusable flow -->
            <div class="account-section">
              <span class="account-section-label text-muted">Account</span>
              <p class="text-secondary account-note">
                Create a sharing account or sign in to this Mac to start sharing
                notes as end-to-end-encrypted links.
              </p>
              <div class="account-actions">
                <button
                  type="button"
                  class="btn btn-primary"
                  (click)="openFlow()"
                >
                  Create or sign in to a sharing account
                </button>
              </div>
              @if (accountError(); as aerr) {
                <p class="text-danger account-note">{{ aerr }}</p>
              }
            </div>
          }
        } @else {
          <p class="text-muted account-note">Loading account…</p>
        }
      </div>

      <!-- (3) Incoming shares (M5-CLIENT, mode B): notes other Murmur users sent
           you. Shown only when signed in AND a server is configured. -->
      @if (status(); as st) {
        @if (st.loggedIn && st.serverConfigured) {
          <div class="card inbox-card">
            <div class="account-copy">
              <h3>Incoming shares</h3>
              <p class="text-secondary account-sub">
                Notes other Murmur users shared with you. Compare the sender's
                safety words out of band, then accept to add the note to your
                vault.
              </p>
            </div>

            @if (inboxError(); as ierr) {
              <p class="text-danger account-note">{{ ierr }}</p>
            }

            @if (inboxLoading()) {
              <p class="text-muted account-note">Loading…</p>
            } @else {
              @for (item of inbox(); track item.shareId) {
                <div class="inbox-row">
                  <div class="inbox-meta">
                    <span class="account-section-label text-muted"
                      >From — safety words</span
                    >
                    <p class="fp-inline">{{ item.senderFingerprint }}</p>
                    <span class="text-secondary inbox-sub">
                      {{ formatDate(item.createdAt) }} ·
                      {{ formatSize(item.size) }} · rev {{ item.rev }}
                    </span>
                  </div>

                  @if (item.alreadyAccepted) {
                    <span class="pill is-success signed-pill">
                      <span class="pill-dot"></span>
                      Accepted
                    </span>
                  } @else {
                    <div class="inbox-actions-wrap">
                      <div class="account-actions">
                        <button
                          type="button"
                          class="btn btn-primary"
                          (click)="accept(item)"
                          [disabled]="busyShare() !== null"
                        >
                          {{
                            busyShare() === item.shareId
                              ? "Accepting…"
                              : "Accept"
                          }}
                        </button>
                        <button
                          type="button"
                          class="btn btn-ghost"
                          (click)="togglePicker(item.shareId)"
                          [disabled]="busyShare() !== null"
                        >
                          Choose folder…
                        </button>
                        <button
                          type="button"
                          class="btn btn-ghost"
                          (click)="decline(item)"
                          [disabled]="busyShare() !== null"
                        >
                          Decline
                        </button>
                      </div>

                      @if (pickerFor() === item.shareId) {
                        <div
                          class="folder-menu"
                          role="menu"
                          aria-label="Accept into folder"
                        >
                          <button
                            type="button"
                            class="folder-opt"
                            role="menuitem"
                            (click)="accept(item)"
                          >
                            Shared <span class="folder-opt-tag">default</span>
                          </button>
                          @for (opt of folderOptions(); track opt.node.id) {
                            <button
                              type="button"
                              class="folder-opt"
                              role="menuitem"
                              [style.--depth]="opt.depth"
                              (click)="accept(item, opt.node.id)"
                            >
                              {{ opt.node.name }}
                            </button>
                          } @empty {
                            <p class="opts-empty">No open folders yet.</p>
                          }
                        </div>
                      }
                    </div>
                  }

                  @if (acceptedTitle(); as at) {
                    @if (at.id === item.shareId) {
                      <p class="inbox-ok" role="status">
                        Added “{{ at.title }}” to your vault.
                      </p>
                    }
                  }
                  @if (rowError(); as re) {
                    @if (re.id === item.shareId) {
                      <p class="text-danger account-note" role="alert">
                        {{ re.msg }}
                      </p>
                    }
                  }
                </div>
              } @empty {
                <p class="text-muted account-note">No incoming shares.</p>
              }
            }
          </div>
        }
      }
    </div>

    <!-- The reusable flow, hosted in an OPAQUE modal (trap T3 — never .card).
         Dismissal = the flow's own Cancel (dismissed) or Escape on the panel —
         matching the repo's share-verify-sheet modal (no scrim-click handler, so
         the a11y lint rules stay clean). -->
    @if (showFlow()) {
      <div class="flow-scrim">
        <div
          #flowPanel
          class="flow-modal"
          role="dialog"
          aria-modal="true"
          aria-label="Sharing account"
          tabindex="-1"
          (keydown.escape)="closeFlow()"
        >
          <app-sharing-auth-flow
            (completed)="onFlowDone()"
            (dismissed)="closeFlow()"
          />
        </div>
      </div>
    }
  `,
  styles: [
    `
      /* Host stays layout-transparent so this section's card is a direct flex
         item of the shell's .section-body (identical spacing to siblings). */
      :host {
        display: contents;
      }
      .section-stack {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
      }

      .account-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-5);
      }
      .account-head {
        display: flex;
        align-items: flex-start;
        gap: var(--space-4);
      }
      .account-mark {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 48px;
        height: 48px;
        min-width: 48px;
        border-radius: var(--radius-md);
        background: var(--accent-soft);
        border: 1px solid var(--glass-border);
        box-shadow: var(--glass-highlight);
      }
      .account-mark svg {
        display: block;
      }
      .account-copy {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        min-width: 0;
      }
      .account-copy h3 {
        margin: 0;
      }
      .account-sub {
        margin: 0;
        font-size: 0.9rem;
        line-height: 1.55;
      }

      .account-section {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .account-section-label {
        font-size: 0.8125rem;
        font-weight: 550;
        letter-spacing: 0.01em;
        text-transform: uppercase;
      }
      .account-note {
        margin: 0;
        font-size: 0.9rem;
        line-height: 1.55;
      }

      /* Shared text input — mirrors the app's input treatment via tokens. */
      .text-input {
        height: 38px;
        padding: 0 var(--space-3);
        border: 1px solid var(--border);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        color: var(--text-primary);
        font: inherit;
        font-size: 0.9rem;
      }
      .text-input::placeholder {
        color: var(--text-muted);
      }
      .text-input:focus-visible {
        outline: none;
        border-color: var(--accent-hover);
        box-shadow: 0 0 0 3px var(--accent-soft);
      }

      .server-row {
        display: flex;
        gap: var(--space-2);
        flex-wrap: wrap;
      }
      .server-input {
        flex: 1 1 18rem;
        min-width: 0;
      }
      .server-row .btn {
        flex: none;
      }

      .account-actions {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        flex-wrap: wrap;
        margin-top: var(--space-1);
      }
      .account-actions .btn {
        flex: none;
      }

      .signed-in-row {
        display: flex;
        align-items: center;
        gap: var(--space-3);
        flex-wrap: wrap;
      }
      .signed-pill {
        align-self: flex-start;
      }

      /* --- Incoming shares (M5-CLIENT inbox) --- */
      .inbox-card {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .inbox-row {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        padding-top: var(--space-4);
        border-top: 1px solid var(--border-subtle);
      }
      .inbox-meta {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .fp-inline {
        margin: 0;
        color: var(--text-primary);
        font-family: var(--font-mono);
        font-size: 0.9rem;
        letter-spacing: 0.03em;
        line-height: 1.5;
        overflow-wrap: anywhere;
        user-select: text;
        -webkit-user-select: text;
      }
      .inbox-sub {
        font-size: 0.8rem;
      }
      .inbox-ok {
        margin: 0;
        color: var(--success);
        font-size: 0.85rem;
      }

      /* The accept-into picker floats over the rows below → OPAQUE (trap T3),
         never the frosted .card. */
      .inbox-actions-wrap {
        position: relative;
        align-self: flex-start;
      }
      .folder-menu {
        position: absolute;
        top: 100%;
        left: 0;
        z-index: 20;
        margin-top: var(--space-1);
        min-width: 220px;
        max-height: 260px;
        overflow-y: auto;
        display: flex;
        flex-direction: column;
        padding: var(--space-2);
        background: var(--surface-overlay);
        border: 1px solid var(--border-strong);
        border-radius: var(--radius-md);
        box-shadow: var(--shadow-lg);
        -webkit-backdrop-filter: none;
        backdrop-filter: none;
      }
      .folder-opt {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        width: 100%;
        padding: var(--space-2);
        padding-left: calc(var(--space-2) + var(--depth, 0) * var(--space-4));
        border: 1px solid transparent;
        border-radius: var(--radius-md);
        background: transparent;
        color: var(--text-secondary);
        font: inherit;
        font-size: 0.875rem;
        font-weight: 550;
        text-align: left;
        cursor: pointer;
        transition:
          color var(--transition),
          background var(--transition);
      }
      .folder-opt:hover {
        color: var(--text-primary);
        background: var(--surface-hover);
      }
      .folder-opt:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .folder-opt-tag {
        color: var(--text-muted);
        font-size: 0.72rem;
        font-weight: 600;
        text-transform: uppercase;
        letter-spacing: 0.03em;
      }
      .opts-empty {
        margin: var(--space-2);
        color: var(--text-muted);
        font-size: 0.8125rem;
      }

      /* --- The reusable-flow OPAQUE modal (trap T3) --- */
      .flow-scrim {
        position: fixed;
        inset: 0;
        z-index: 100;
        display: flex;
        align-items: center;
        justify-content: center;
        padding: var(--space-5);
        background: rgba(0, 0, 0, 0.5);
        -webkit-backdrop-filter: blur(2px);
        backdrop-filter: blur(2px);
        animation: scrim-in 180ms var(--transition) both;
      }
      @keyframes scrim-in {
        from {
          opacity: 0;
        }
        to {
          opacity: 1;
        }
      }
      .flow-modal {
        width: 100%;
        max-width: 460px;
        max-height: calc(100vh - 2 * var(--space-5));
        overflow-y: auto;
        padding: var(--space-6);
        /* OPAQUE overlay — NOT the frosted .card (trap T3). */
        background: var(--surface-overlay);
        border: 1px solid var(--border-strong);
        border-radius: var(--radius-md);
        box-shadow: var(--shadow-lg);
        -webkit-backdrop-filter: none;
        backdrop-filter: none;
        animation: modal-in 220ms var(--ease-spring) both;
      }
      .flow-modal:focus-visible {
        outline: none;
      }
      @keyframes modal-in {
        from {
          opacity: 0;
          transform: translateY(10px) scale(0.985);
        }
        to {
          opacity: 1;
          transform: translateY(0) scale(1);
        }
      }
      @media (prefers-reduced-motion: reduce) {
        .flow-scrim,
        .flow-modal {
          animation: none !important;
        }
      }
    `,
  ],
})
export class SettingsAccountSectionComponent {
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);

  /** The current sharing-account session; `null` until the first load resolves. */
  private readonly _status = signal<AccountStatus | null>(null);
  readonly status = this._status.asReadonly();

  /** True while a logout IPC call is in flight (debounces the Sign out button). */
  private readonly _busy = signal(false);
  readonly busy = this._busy.asReadonly();

  /** A general account error (status load / logout failure). */
  private readonly _accountError = signal<string | null>(null);
  readonly accountError = this._accountError.asReadonly();

  /** Whether the reusable-flow modal is open. */
  private readonly _showFlow = signal(false);
  readonly showFlow = this._showFlow.asReadonly();

  /** The modal panel — focused on open so Escape works immediately. */
  private readonly flowPanel =
    viewChild<ElementRef<HTMLElement>>("flowPanel");

  // ── Server base URL ──────────────────────────────────────────────────────
  readonly serverControl = new FormControl("", { nonNullable: true });
  private readonly _savingServer = signal(false);
  readonly savingServer = this._savingServer.asReadonly();
  private readonly _serverError = signal<string | null>(null);
  readonly serverError = this._serverError.asReadonly();

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
    let st: AccountStatus | null = null;
    try {
      st = await this.ipc.accountStatus();
      this._status.set(st);
    } catch (e) {
      this._accountError.set(String(e));
    }
    try {
      const cfg = await this.ipc.getConfig();
      // Only seed the input if the user hasn't typed something unsaved.
      if (!this.serverControl.dirty) {
        this.serverControl.setValue(cfg.shareBaseUrl ?? "");
      }
    } catch {
      // Leave the input empty on a config read failure.
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
      this._inboxError.set(String(e));
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

  /** Persist the sharing-server base URL through the normal config round-trip. */
  async saveServer(): Promise<void> {
    if (this._savingServer()) return;
    this._serverError.set(null);
    this._savingServer.set(true);
    try {
      const cfg = await this.ipc.getConfig();
      await this.ipc.saveConfig({
        ...cfg,
        shareBaseUrl: this.serverControl.value.trim(),
      });
      this.serverControl.markAsPristine();
      await this.reload();
    } catch (e) {
      this._serverError.set(String(e));
    } finally {
      this._savingServer.set(false);
    }
  }

  /** Sign out (server family-revoke + clear tokens + drop session MK), then reload. */
  async logout(): Promise<void> {
    if (this._busy()) return;
    this._busy.set(true);
    try {
      await this.ipc.accountLogout();
      await this.reload();
    } catch (e) {
      this._accountError.set(String(e));
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
        msg: this.friendlyShareError(String(e)),
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
      this._rowError.set({ id: item.shareId, msg: String(e) });
    } finally {
      this._busyShare.set(null);
    }
  }

  /** Turn a raw backend error into a friendly, non-crashy inline message. */
  private friendlyShareError(raw: string): string {
    if (/lock/i.test(raw)) {
      return "That folder is locked. Unlock it (or pick an open folder / the default) to accept.";
    }
    return raw;
  }

  /** Presentational: render an ISO timestamp as a friendly local date. */
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
