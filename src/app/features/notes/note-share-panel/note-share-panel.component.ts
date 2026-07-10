import {
  ChangeDetectionStrategy,
  Component,
  Injector,
  computed,
  effect,
  inject,
  input,
  output,
  signal,
} from "@angular/core";
import { IpcService } from "../../../core/ipc.service";
import type { AccountStatus, MyShareEntry } from "../../../core/models";

/** The link-share flow step (Manage always coexists as the list below). */
type ShareStep = "configure" | "created";

/** The Expires segmented choice — `null` = Never (omit `expiresDays`). */
type ExpiryChoice = null | 1 | 7 | 30;

/** One Active-links Manage-list row (view-model). */
interface LinkShareRow {
  shareId: string;
  createdAt: string;
  usageLabel: string;
  expiryLabel: string;
  state: "active" | "limit" | "expired" | "revoked";
  /** Non-null ONLY for a link created THIS session (the key is never re-derivable). */
  copyUrl: string | null;
  passwordProtected: boolean;
  locked: boolean;
}

/**
 * The NOTE link-share panel (WP6 / FP-share) — a floating modal over the editor
 * that creates end-to-end-encrypted link shares of an AUTHORED note and manages
 * this note's active links. It mirrors the meeting `SharePanelComponent`'s gate +
 * CONFIGURE → CREATED → MANAGE flow and visual language, but is anchored on the
 * note's `documentId` and drives `shareNoteToLinkDoc` (the `_doc` command) +
 * `listMyShares` (filtered to `documentId`) + `revokeShare`.
 *
 * SELF-CONTAINED: injects its own {@link IpcService}, owns the whole share
 * sub-state (the editor only opens/closes it + passes `noteId`). LINK sharing
 * only (mode A); person-share (mode B) stays meeting-scoped for v1.
 *
 * HONESTY invariants (identical to the meeting panel):
 *  - the created URL (with the `#…` key fragment) lives ONLY in a transient
 *    session signal — never persisted, never logged;
 *  - per-row Copy is enabled ONLY for a link created THIS session (the server
 *    can't rebuild the key) → otherwise disabled with an honest tooltip;
 *  - a sealed-not-unlocked note is refused by the backend (`Locked`) — the
 *    editor hides Share while locked, and the gate here fails closed too.
 *
 * OPAQUE overlay (T3): the modal floats OVER the document, so the scrim + card
 * use `--surface-overlay`, never the frosted `.card`.
 */
@Component({
  selector: "app-note-share-panel",
  changeDetection: ChangeDetectionStrategy.OnPush,
  templateUrl: "./note-share-panel.component.html",
  styleUrl: "./note-share-panel.component.scss",
})
export class NoteSharePanelComponent {
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);

  /** THIS note's document id — the shares filter key. */
  readonly noteId = input.required<string>();

  /** Close the modal (backdrop / Close / Esc). */
  readonly closed = output<void>();
  /** Emitted after any create/revoke so the editor can refresh the `shared` flag. */
  readonly changed = output<void>();
  /** Press the gate CTA → the editor routes to Settings › Sharing. */
  readonly setupSharing = output<void>();

  // --- Account + gate -------------------------------------------------------
  private readonly accountStatus = signal<AccountStatus | null>(null);
  readonly loading = signal(false);
  readonly gateError = signal<string | null>(null);
  readonly unlocking = signal(false);
  private readonly _biometricFailed = signal(false);

  /** Sharing can happen: server set + signed in + unlocked for sharing. */
  readonly gateReady = computed(() => {
    const s = this.accountStatus();
    return !!s && s.serverConfigured && s.loggedIn && s.unlockedForSharing;
  });

  /** Offer one-tap Touch ID when the only blocker is the session share-key. */
  readonly canBiometricUnlock = computed(() => {
    const s = this.accountStatus();
    return (
      !!s &&
      s.serverConfigured &&
      s.loggedIn &&
      !s.unlockedForSharing &&
      s.biometricUnlockAvailable &&
      !this._biometricFailed()
    );
  });

  readonly shareConsented = computed(
    () => this.accountStatus()?.shareConsented ?? false,
  );

  /** Only the FAILING preconditions, top-down (server → sign in → unlock). */
  readonly gateReasons = computed<{ key: string; text: string }[]>(() => {
    const s = this.accountStatus();
    const out: { key: string; text: string }[] = [];
    if (!s || !s.serverConfigured) {
      out.push({ key: "server", text: "No sharing server is configured." });
    }
    if (s && s.serverConfigured && !s.loggedIn) {
      out.push({ key: "signin", text: "You're not signed in to a sharing account." });
    }
    if (s && s.serverConfigured && s.loggedIn && !s.unlockedForSharing) {
      out.push({ key: "unlock", text: "Unlock sharing (Touch ID) to continue." });
    }
    if (!out.length) {
      out.push({ key: "server", text: "No sharing server is configured." });
    }
    return out;
  });

  readonly gateCta = computed(() => {
    const s = this.accountStatus();
    if (!s || !s.serverConfigured) {
      return "Set up sharing";
    }
    if (!s.loggedIn) {
      return "Sign in";
    }
    return "Unlock for sharing";
  });

  // --- CONFIGURE state ------------------------------------------------------
  readonly step = signal<ShareStep>("configure");
  readonly password = signal("");
  readonly showPassword = signal(false);
  readonly noPassword = signal(false);
  readonly expiry = signal<ExpiryChoice>(7);
  readonly limitOpens = signal(false);
  readonly maxOpens = signal(5);
  readonly creating = signal(false);
  readonly consenting = signal(false);
  readonly createError = signal<string | null>(null);

  readonly expiryOptions: { value: ExpiryChoice; label: string }[] = [
    { value: null, label: "Never" },
    { value: 1, label: "1 day" },
    { value: 7, label: "7 days" },
    { value: 30, label: "30 days" },
  ];

  /** Cosmetic password-strength hint — NEVER blocks Create. */
  readonly strength = computed<{ level: number; label: string }>(() => {
    const pw = this.password();
    if (!pw) {
      return { level: 0, label: "" };
    }
    let score = 0;
    if (pw.length >= 8) score++;
    if (pw.length >= 14) score++;
    if (/[a-z]/.test(pw) && /[A-Z]/.test(pw)) score++;
    if (/\d/.test(pw) || /[^\w\s]/.test(pw)) score++;
    const level = Math.min(4, Math.max(1, score));
    const label = ["", "Weak", "Fair", "Good", "Strong"][level];
    return { level, label };
  });

  // --- CREATED state (transient — the URL lives ONLY here) ------------------
  readonly createdUrl = signal<string | null>(null);
  readonly createdWithPassword = signal(false);
  readonly createdExpiryLabel = signal("Never expires");
  readonly createdMaxLabel = signal<string | null>(null);
  readonly createdCopied = signal(false);

  // --- MANAGE state ---------------------------------------------------------
  private readonly myShares = signal<MyShareEntry[]>([]);
  readonly listError = signal<string | null>(null);
  /** Per-session share_id → { url, pw }. `L` lives only in the fragment (never persisted). */
  private readonly sessionShares = signal<Map<string, { url: string; pw: boolean }>>(
    new Map(),
  );
  readonly confirmingRevokeId = signal<string | null>(null);
  readonly revokingId = signal<string | null>(null);
  readonly copiedRowId = signal<string | null>(null);

  /** Active-links view-model: `listMyShares()` filtered to THIS note + mode 'link'. */
  readonly linkRows = computed<LinkShareRow[]>(() => {
    const id = this.noteId();
    const sess = this.sessionShares();
    const now = Date.now();
    return this.myShares()
      .filter((s) => s.documentId === id && s.mode === "link")
      .map((s) => {
        const exp = s.expiresAt ? Date.parse(s.expiresAt) : null;
        const expired = exp != null && !Number.isNaN(exp) && exp < now;
        const limit = s.maxDownloads != null && s.downloadCount >= s.maxDownloads;
        const state: LinkShareRow["state"] = s.revoked
          ? "revoked"
          : limit
            ? "limit"
            : expired
              ? "expired"
              : "active";
        const session = sess.get(s.shareId);
        return {
          shareId: s.shareId,
          createdAt: s.createdAt,
          usageLabel:
            s.maxDownloads != null
              ? `${s.downloadCount} / ${s.maxDownloads} opens`
              : `${s.downloadCount} opens`,
          expiryLabel:
            exp == null || Number.isNaN(exp)
              ? "never"
              : expired
                ? "expired"
                : `${Math.max(1, Math.ceil((exp - now) / 86400000))}d left`,
          state,
          copyUrl: session?.url ?? null,
          passwordProtected: session?.pw ?? false,
          locked: s.locked,
        };
      });
  });

  constructor() {
    // Lazy-load account status + shares whenever the note id resolves (T1 — async
    // IPC-on-signal-change effect writes loading/error, stale-guarded on the id).
    effect(
      () => {
        const id = this.noteId();
        if (!id) {
          return;
        }
        void this.refresh();
      },
      { injector: this.injector },
    );
  }

  // --- Loading --------------------------------------------------------------

  /** Reload the account status + shares list (stale-guarded on the captured id). */
  async refresh(): Promise<void> {
    const id = this.noteId();
    if (!id) {
      return;
    }
    this.listError.set(null);
    this.gateError.set(null);
    this._biometricFailed.set(false);
    this.loading.set(true);
    try {
      const st = await this.ipc.accountStatus();
      if (this.noteId() !== id) {
        return; // stale — the note changed under us
      }
      this.accountStatus.set(st);
      if (st.serverConfigured && st.loggedIn && st.unlockedForSharing) {
        const shares = await this.ipc.listMyShares();
        if (this.noteId() !== id) {
          return;
        }
        this.myShares.set(shares);
      } else {
        this.myShares.set([]);
      }
    } catch (e) {
      this.listError.set(String(e));
      this.gateError.set(String(e));
    } finally {
      this.loading.set(false);
    }
  }

  /** One-tap Touch ID unlock from the gate → re-read status + load this note's shares. */
  async unlockWithBiometric(): Promise<void> {
    if (this.unlocking()) {
      return;
    }
    this.unlocking.set(true);
    this.gateError.set(null);
    try {
      await this.ipc.unlockSharingWithBiometric();
      await this.refresh();
      if (!this.accountStatus()?.unlockedForSharing) {
        this._biometricFailed.set(true);
        this.gateError.set(
          "Couldn't unlock this session. Use Unlock for sharing to unlock with your password.",
        );
      }
    } catch (e) {
      this._biometricFailed.set(true);
      this.gateError.set(this.friendlyUnlockError(String(e)));
    } finally {
      this.unlocking.set(false);
    }
  }

  private friendlyUnlockError(raw: string): string {
    if (/cancel/i.test(raw)) {
      return "Touch ID was cancelled. Use Unlock for sharing to unlock with your password.";
    }
    return `Couldn't unlock with Touch ID — ${raw}. Use Unlock for sharing to unlock with your password.`;
  }

  // --- CONFIGURE handlers ---------------------------------------------------

  onPasswordInput(event: Event): void {
    this.password.set((event.target as HTMLInputElement).value);
  }

  onNoPasswordChange(event: Event): void {
    const on = (event.target as HTMLInputElement).checked;
    this.noPassword.set(on);
    if (on) {
      this.password.set("");
      this.showPassword.set(false);
    }
  }

  onLimitOpensChange(event: Event): void {
    this.limitOpens.set((event.target as HTMLInputElement).checked);
  }

  stepOpens(delta: number): void {
    this.maxOpens.set(Math.max(1, this.maxOpens() + delta));
  }

  /** Grant the one-time share-egress consent inline (enables Create). */
  async grantConsent(): Promise<void> {
    if (this.consenting()) {
      return;
    }
    this.createError.set(null);
    this.consenting.set(true);
    try {
      await this.ipc.consentToShareEgress();
      const s = this.accountStatus();
      if (s) {
        this.accountStatus.set({ ...s, shareConsented: true });
      }
    } catch (e) {
      this.createError.set(String(e));
    } finally {
      this.consenting.set(false);
    }
  }

  /**
   * Create the zero-knowledge link for THIS note. Maps the form → the backend
   * contract: Never ⇒ null expiresDays, No-password ⇒ null password, unchecked
   * limit ⇒ null maxDownloads. The URL lands ONLY in the transient `createdUrl`
   * signal + the clipboard; the password is cleared right after.
   */
  async createLink(): Promise<void> {
    const id = this.noteId();
    if (!id || this.creating() || !this.shareConsented()) {
      return;
    }
    const pw = this.noPassword() ? "" : this.password().trim();
    const hasPw = pw.length > 0;
    const expiryDays = this.expiry();
    const maxDl = this.limitOpens() ? Math.max(1, this.maxOpens()) : null;

    this.createError.set(null);
    this.creating.set(true);
    try {
      const url = await this.ipc.shareNoteToLinkDoc(
        id,
        expiryDays,
        hasPw ? pw : null,
        maxDl,
      );
      this.stashSessionUrl(url, hasPw);
      this.createdUrl.set(url);
      this.createdWithPassword.set(hasPw);
      this.createdExpiryLabel.set(
        expiryDays === null
          ? "Never expires"
          : `Expires in ${expiryDays} day${expiryDays === 1 ? "" : "s"}`,
      );
      this.createdMaxLabel.set(maxDl != null ? `${maxDl} opens` : null);
      this.password.set("");
      this.step.set("created");
      try {
        await navigator.clipboard.writeText(url);
        this.createdCopied.set(true);
      } catch {
        // Clipboard unavailable — the URL stays visible + selectable.
      }
      await this.refresh();
      this.changed.emit();
    } catch (e) {
      this.createError.set(this.friendlyCreateError(String(e)));
    } finally {
      this.creating.set(false);
    }
  }

  /** A `Locked` create failure means the folder sealed under us — say so plainly. */
  private friendlyCreateError(raw: string): string {
    if (/Locked/i.test(raw)) {
      return "This note is locked — unlock its folder to share it.";
    }
    return raw;
  }

  // --- CREATED handlers -----------------------------------------------------

  async copyCreated(): Promise<void> {
    const url = this.createdUrl();
    if (!url) {
      return;
    }
    try {
      await navigator.clipboard.writeText(url);
      this.createdCopied.set(true);
    } catch {
      // Clipboard unavailable — the URL stays visible + selectable.
    }
  }

  /** "Create another": drop the transient URL + return to a fresh Configure form. */
  createAnother(): void {
    this.createdUrl.set(null);
    this.createdCopied.set(false);
    this.step.set("configure");
  }

  /** "Done": drop the transient URL entirely; the Manage row has no re-copy. */
  done(): void {
    this.createdUrl.set(null);
    this.createdCopied.set(false);
    this.step.set("configure");
  }

  // --- MANAGE handlers ------------------------------------------------------

  /** Copy a row's URL from the session map (present ONLY for this-session links). */
  async copyRow(shareId: string): Promise<void> {
    const url = this.sessionShares().get(shareId)?.url;
    if (!url) {
      return;
    }
    try {
      await navigator.clipboard.writeText(url);
      this.copiedRowId.set(shareId);
    } catch {
      // Clipboard unavailable — nothing to surface.
    }
  }

  /** Revoke (after the inline confirm), optimistically flip, then re-fetch. */
  async revokeRow(shareId: string): Promise<void> {
    if (this.revokingId()) {
      return;
    }
    this.revokingId.set(shareId);
    this.listError.set(null);
    this.myShares.set(
      this.myShares().map((s) =>
        s.shareId === shareId ? { ...s, revoked: true } : s,
      ),
    );
    try {
      await this.ipc.revokeShare(shareId);
      this.confirmingRevokeId.set(null);
      await this.refresh();
      this.changed.emit();
    } catch (e) {
      this.listError.set(String(e));
      await this.refresh();
    } finally {
      this.revokingId.set(null);
    }
  }

  /** A just-created link → session map, keyed by its share_id (fragment segment). Never logged. */
  private stashSessionUrl(url: string, pw: boolean): void {
    const frag = url.split("#")[1];
    const shareId = frag ? frag.split(".")[0] : "";
    if (!shareId) {
      return;
    }
    const next = new Map(this.sessionShares());
    next.set(shareId, { url, pw });
    this.sessionShares.set(next);
  }

  /** Presentational: a share's ISO createdAt → a compact local date. */
  formatShareDate(createdAt: string): string {
    const d = new Date(createdAt);
    if (Number.isNaN(d.getTime())) {
      return createdAt;
    }
    return d.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    });
  }

  /** Close the modal (backdrop / Close / Esc). */
  close(): void {
    this.closed.emit();
  }
}
