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
import type {
  AccountStatus,
  MyShareEntry,
  NoteAttachmentDto,
  OrgShareEntry,
  OrgSourceShareStatus,
  OrgStatus,
  RecipientPreview,
} from "../../../core/models";
import {
  ShareVerifySheetComponent,
  type ShareVerifyMode,
} from "../share-verify-sheet/share-verify-sheet.component";
import {
  OrgShareSheetComponent,
  type OrgShareTarget,
} from "../org-share-sheet/org-share-sheet.component";
import { MurOrgBrainCtaComponent } from "../../../design-system/org-brain-cta/org-brain-cta.component";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";

/** The step the in-flow "Share with a person" panel is showing. */
export type PersonShareStep = "email" | "suggest-link" | "consent" | "result";

/** The 3-state link-share flow (Manage always coexists as the list below). */
export type ShareStep = "configure" | "created";

/** The Expires segmented choice — `null` = Never (omit `expiresDays`). */
type ExpiryChoice = null | 1 | 7 | 30;

/** The per-row view-model for the Active-links Manage list. */
export interface LinkShareRow {
  shareId: string;
  createdAt: string;
  usageLabel: string;
  expiryLabel: string;
  state: "active" | "limit" | "expired" | "revoked";
  /** Non-null ONLY for a link created THIS session (the key is never re-derivable). */
  copyUrl: string | null;
  /** True when the created-this-session link carried a password (best-effort, never wrong). */
  passwordProtected: boolean;
  /** True when the local meeting is sealed/unknown → render a masked 🔒 row. */
  locked: boolean;
}

/**
 * The SHARE tab — the password-FIRST link-share flow rebuilt into a
 * CONFIGURE → CREATED → MANAGE state machine, gated on the sharing account, plus
 * the mode-B "Share with a person" flow behind the same gate.
 *
 * SELF-CONTAINED (spec §6): it injects its OWN {@link IpcService} and owns the
 * whole share sub-state, exactly like `meeting-chat` owns its IPC. The shell only
 * passes `meetingId` / `active` / `editing` / `locked` and receives a `changed`
 * ping (so the shell can, e.g., refresh a share count) — no share state lives in
 * the shell any more.
 *
 * HONESTY invariants baked in:
 *  - The created URL (with the `#…` decryption-key fragment) lives ONLY in a
 *    transient session signal, cleared on Done / a meeting change — NEVER persisted
 *    or logged.
 *  - Per-row Copy is enabled ONLY for a link created THIS session; older links
 *    can't be re-shown (the key isn't stored server-side) → disabled + honest
 *    tooltip, never a wrong claim.
 *  - The verify SHEET floats OVER the note → OPAQUE `--surface-overlay` (trap T3),
 *    handled inside {@link ShareVerifySheetComponent}.
 */
@Component({
  selector: "app-share-panel",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [
    ShareVerifySheetComponent,
    OrgShareSheetComponent,
    MurOrgBrainCtaComponent,
  ],
  templateUrl: "./share-panel.component.html",
  styleUrl: "./share-panel.component.scss",
})
export class SharePanelComponent {
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);
  private readonly errorCopy = inject(ErrorCopyService);

  // --- Inputs from the shell ------------------------------------------------
  /** THIS meeting's id (null while the detail is loading), the shares filter key. */
  readonly meetingId = input<string | null>(null);
  /** True while the Share tab is the active tab (drives the lazy load). */
  readonly active = input(false);
  /** True while the note is being edited (disables Create / person share). */
  readonly editing = input(false);
  /** True when the meeting is sealed-and-not-session-unlocked (never reached — the shell
   *  renders the lock gate instead — but kept fail-closed for safety). */
  readonly locked = input(false);
  /** Only image attachments referenced by this meeting note's markdown. */
  readonly attachments = input<readonly NoteAttachmentDto[]>([]);
  readonly attachmentBytes = computed(() =>
    this.attachments().reduce((total, attachment) => total + attachment.byteLen, 0),
  );

  /** Emitted after any create/revoke so the shell can refresh a dependent count. */
  readonly changed = output<void>();
  /** Emitted when the gate's CTA is pressed → the shell routes to Settings › Sharing. */
  readonly setupSharing = output<void>();

  formatAttachmentBytes(bytes: number): string {
    if (bytes < 1024) {
      return `${bytes} B`;
    }
    if (bytes < 1024 * 1024) {
      return `${(bytes / 1024).toFixed(1)} KB`;
    }
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  }

  // --- Account + gate -------------------------------------------------------
  private readonly accountStatus = signal<AccountStatus | null>(null);
  readonly loading = signal(false);
  readonly gateError = signal<string | null>(null);
  /** True while the one-tap Touch ID unlock IPC is in flight ("Unlocking…"). */
  readonly unlocking = signal(false);
  /**
   * Latches true when a Touch ID unlock attempt fails, so the gate falls back to
   * the password CTA instead of re-offering the biometric sheet. Cleared on the
   * next `refresh()` (a fresh gate state re-enables Touch ID).
   */
  private readonly _biometricFailed = signal(false);

  /** Sharing can actually happen: server set + signed in + unlocked for sharing. */
  readonly gateReady = computed(() => {
    const s = this.accountStatus();
    return !this.locked() && !!s && s.serverConfigured && s.loggedIn && s.unlockedForSharing;
  });

  /**
   * Offer the one-tap Touch ID unlock in the gate: the ONLY blocker is the
   * session share-key (server set + signed in, just not unlocked this run) AND a
   * cached account key exists AND no prior attempt just failed. Otherwise the
   * password CTA (`setupSharing`) is the fallback.
   */
  readonly canBiometricUnlock = computed(() => {
    const s = this.accountStatus();
    return (
      !this.locked() &&
      !!s &&
      s.serverConfigured &&
      s.loggedIn &&
      !s.unlockedForSharing &&
      s.biometricUnlockAvailable &&
      !this._biometricFailed()
    );
  });
  /** Whether the one-time share-egress consent is granted (drives the inline consent). */
  readonly shareConsented = computed(() => this.accountStatus()?.shareConsented ?? false);

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
  /** The gate CTA label, keyed to the first failing precondition. */
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
  /** The just-created share URL (fragment = decryption key). NEVER persisted/logged. */
  readonly createdUrl = signal<string | null>(null);
  readonly createdWithPassword = signal(false);
  readonly createdExpiryLabel = signal("Never expires");
  readonly createdMaxLabel = signal<string | null>(null);
  readonly createdCopied = signal(false);

  // --- MANAGE state ---------------------------------------------------------
  private readonly myShares = signal<MyShareEntry[]>([]);
  readonly listError = signal<string | null>(null);
  /**
   * Per-session share_id → { url, pw }. `L` lives only in the URL fragment (never
   * persisted/sent), so `list_my_shares` can't rebuild a URL — per-row Copy works
   * ONLY for links created this session.
   */
  private readonly sessionShares = signal<Map<string, { url: string; pw: boolean }>>(
    new Map(),
  );
  readonly confirmingRevokeId = signal<string | null>(null);
  readonly revokingId = signal<string | null>(null);
  readonly copiedRowId = signal<string | null>(null);

  /** Active-links view-model: `listMyShares()` → this meeting + mode 'link', enriched. */
  readonly linkRows = computed<LinkShareRow[]>(() => {
    const id = this.meetingId();
    const sess = this.sessionShares();
    const now = Date.now();
    return this.myShares()
      .filter((s) => s.meetingId === id && s.mode === "link")
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

  /** People this note has been shared with (mode 'user'). Informational. */
  readonly peopleShareCount = computed(
    () =>
      this.myShares().filter(
        (s) => s.meetingId === this.meetingId() && s.mode === "user",
      ).length,
  );

  // --- Person share (mode B) -----------------------------------------------
  readonly personOpen = signal(false);
  readonly personStep = signal<PersonShareStep>("email");
  readonly personEmail = signal("");
  readonly personPreview = signal<RecipientPreview | null>(null);
  readonly personBusy = signal(false);
  readonly personError = signal<string | null>(null);
  readonly personResult = signal<string | null>(null);
  readonly verifyMode = signal<ShareVerifyMode | null>(null);

  // --- Org Brain (Shared Brain v1) ------------------------------------------
  /**
   * EVERY org this user actively belongs to (created OR invited-into) — drives
   * whether the "Add to Org Brain" section renders at all. Uses `orgListStatuses`
   * (not the legacy single-org `orgStatus`, which only ever returns the FIRST
   * locally-joined org): an invite-only member whose org isn't first in the local
   * list must still see the CTA. The actual org PICK happens inside
   * `OrgShareSheetComponent`, which already loads this same full list.
   */
  private readonly _orgs = signal<OrgStatus[]>([]);
  /** True while the user belongs to at least one org (gates the CTA section). */
  readonly org = computed(() => this._orgs().length > 0);
  /** This user's outgoing org shares (drives the "In Org Brain" state for THIS meeting). */
  private readonly orgShares = signal<OrgShareEntry[]>([]);
  /**
   * Live (uploaded) org shares of THIS meeting specifically — powers the CTA "In
   * Org Brain ✓" state (a re-share is blocked; edits sync automatically). Joined to
   * the meeting id backend-side, unlike the content-free `orgShares` list.
   */
  private readonly _orgSourceShares = signal<OrgSourceShareStatus[]>([]);
  /** True when THIS meeting is already live in ≥1 org (flips the CTA to shared). */
  readonly sourceInOrg = computed(() => this._orgSourceShares().length > 0);
  /** True while the org-share PREVIEW SHEET is open (the confirm flow). */
  readonly orgSheetOpen = signal(false);

  /** The org-share sheet target (this meeting), passed into the preview sheet. */
  readonly orgTarget = computed<OrgShareTarget>(() => ({
    kind: "meeting",
    id: this.meetingId() ?? "",
  }));

  /**
   * Whether THIS meeting is already in the org brain — matched by an active
   * (non-revoked) org share. The org-share list is content-free (item ids), so a
   * true match needs a live badge but we can't join it to a meeting id from the
   * FE alone; the backend `list_org_shares` returns THIS user's shares and the
   * detail header's badge wires to that. Here we only show the CTA-vs-shared
   * state for the whole section (see the template's active-count line).
   */
  readonly orgActiveShareCount = computed(
    () => this.orgShares().filter((s) => s.state !== "revoked").length,
  );

  constructor() {
    // Lazy-load account status + shares whenever the Share tab is active for a
    // loaded meeting (T1 — async IPC-on-signal-change effect writes loading/error).
    effect(
      () => {
        const id = this.meetingId();
        if (!this.active() || !id) {
          return;
        }
        void this.refresh();
      },
      { injector: this.injector },
    );

    // A meeting change clears the transient created-link + per-session URLs — a
    // stashed URL must NEVER survive into another note's list.
    effect(
      () => {
        this.meetingId();
        this.resetTransient();
      },
      { injector: this.injector },
    );
  }

  // --- Loading --------------------------------------------------------------

  /** Reload the account status + shares list (stale-guarded on the captured id). */
  async refresh(): Promise<void> {
    const id = this.meetingId();
    if (!id) {
      return;
    }
    this.listError.set(null);
    this.gateError.set(null);
    // A fresh gate state re-enables the Touch ID path (drop any prior fail latch).
    this._biometricFailed.set(false);
    this.loading.set(true);
    try {
      const st = await this.ipc.accountStatus();
      if (this.meetingId() !== id) {
        return; // stale — the meeting changed under us
      }
      this.accountStatus.set(st);
      if (st.serverConfigured && st.loggedIn && st.unlockedForSharing) {
        const shares = await this.ipc.listMyShares();
        if (this.meetingId() !== id) {
          return;
        }
        this.myShares.set(shares);
      } else {
        this.myShares.set([]);
      }
      // Org Brain state (best-effort, non-blocking): the section only appears
      // when the user is in an org. A failure leaves it hidden — never breaks
      // the link/person share UI above.
      void this.refreshOrg(id);
    } catch (e) {
      this.listError.set(this.errorCopy.humanize(e));
      this.gateError.set(this.errorCopy.humanize(e));
    } finally {
      this.loading.set(false);
    }
  }

  /** Load every joined org + this user's org shares (stale-guarded on the meeting id). */
  private async refreshOrg(id: string): Promise<void> {
    try {
      const orgs = await this.ipc.orgListStatuses();
      if (this.meetingId() !== id) {
        return;
      }
      this._orgs.set(orgs);
      if (orgs.length > 0) {
        // `listOrgShares` is now org-SCOPED (it used to silently answer for the first joined org
        // only). This panel's count spans every org the user belongs to, so ask each one and merge —
        // a per-org failure must not blank the others, hence the per-call catch.
        const perOrg = await Promise.all(
          orgs.map((o) => this.ipc.listOrgShares(o.orgId).catch(() => [])),
        );
        if (this.meetingId() !== id) {
          return;
        }
        this.orgShares.set(perOrg.flat());
        // Per-source live shares → the CTA "In Org Brain ✓" state + re-share block.
        const live = await this.ipc.orgLiveSharesForSource({ meetingId: id });
        if (this.meetingId() !== id) {
          return;
        }
        this._orgSourceShares.set(live);
      } else {
        this.orgShares.set([]);
        this._orgSourceShares.set([]);
      }
    } catch {
      // Org unavailable (older backend / not joined) — hide the section.
      this._orgs.set([]);
      this.orgShares.set([]);
      this._orgSourceShares.set([]);
    }
  }

  // --- Org Brain handlers ---------------------------------------------------

  /** Open the "Add to Org Brain" preview sheet (disabled while editing). */
  openOrgSheet(): void {
    if (this.editing() || !this.meetingId()) {
      return;
    }
    this.orgSheetOpen.set(true);
  }

  /** Close the org-share sheet (cancel / backdrop / Escape) — nothing left the device. */
  closeOrgSheet(): void {
    this.orgSheetOpen.set(false);
  }

  /** The sheet published successfully → close it, refresh org state, ping the shell. */
  async onOrgShared(): Promise<void> {
    this.orgSheetOpen.set(false);
    const id = this.meetingId();
    if (id) {
      await this.refreshOrg(id);
    }
    this.changed.emit();
  }

  /**
   * One-tap Touch ID unlock from the gate: presents a single biometric sheet,
   * restores the session MK, then re-reads status + loads this note's shares so
   * the gate opens straight into the share UI. On ANY failure it fails closed to
   * the password CTA (latch the fallback + surface a friendly gate message).
   */
  async unlockWithBiometric(): Promise<void> {
    if (this.unlocking()) {
      return;
    }
    this.unlocking.set(true);
    this.gateError.set(null);
    try {
      await this.ipc.unlockSharingWithBiometric();
      // Re-read the (now unlocked) status + load the shares for this note.
      await this.refresh();
      if (!this.accountStatus()?.unlockedForSharing) {
        // Resolved but still locked — fall back to the password path.
        this._biometricFailed.set(true);
        this.gateError.set(
          "Couldn't unlock this session. Use Unlock for sharing to unlock with your password.",
        );
      }
    } catch (e) {
      this._biometricFailed.set(true);
      // LOCK GATE. The copy is owned by `ErrorCopyService` under the "meeting-share" context, which
      // distinguishes a Touch ID CANCEL from a Touch ID FAILURE by the `[touch-id-*]` code rather
      // than by regex-matching the word "cancel" in whatever the keychain happened to say.
      //
      // The raw backend cause is no longer appended. It used to be, deliberately, so a signed build
      // could be field-debugged from the screen — but the same string carries keychain OSStatus
      // internals ("account-session mutex poisoned", "HKDF expand failed"), and a share gate is
      // exactly the surface where that is least acceptable. The gate itself is UNCHANGED: this
      // still fails closed, still sets `_biometricFailed`, still offers the password path.
      this.gateError.set(this.errorCopy.humanize(e, "meeting-share"));
    } finally {
      this.unlocking.set(false);
    }
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
      // Reflect the new consent locally so the button flips without a full reload.
      const s = this.accountStatus();
      if (s) {
        this.accountStatus.set({ ...s, shareConsented: true });
      }
    } catch (e) {
      this.createError.set(this.errorCopy.humanize(e));
    } finally {
      this.consenting.set(false);
    }
  }

  /**
   * Create the zero-knowledge link. Maps the form → the exact backend contract:
   * Never = omit `expiresDays`, No-password = omit `password`, unchecked limit =
   * omit `maxDownloads`. The URL lands ONLY in the transient `createdUrl` signal
   * (never logged) + the clipboard; the password is cleared right after.
   */
  async createLink(): Promise<void> {
    const id = this.meetingId();
    if (!id || this.creating() || this.editing() || !this.shareConsented()) {
      return;
    }
    const pw = this.noPassword() ? "" : this.password().trim();
    const hasPw = pw.length > 0;
    const expiryDays = this.expiry();
    const maxDl = this.limitOpens() ? Math.max(1, this.maxOpens()) : undefined;

    this.createError.set(null);
    this.creating.set(true);
    try {
      const url = await this.ipc.shareNoteToLink(id, {
        expiresDays: expiryDays ?? undefined,
        password: hasPw ? pw : undefined,
        maxDownloads: maxDl,
      });
      // Stash under its share_id so the just-created row is copyable this session.
      this.stashSessionUrl(url, hasPw);
      this.createdUrl.set(url);
      this.createdWithPassword.set(hasPw);
      this.createdExpiryLabel.set(
        expiryDays === null
          ? "Never expires"
          : `Expires in ${expiryDays} day${expiryDays === 1 ? "" : "s"}`,
      );
      this.createdMaxLabel.set(maxDl != null ? `${maxDl} opens` : null);
      this.password.set(""); // clear the transient password once baked into the link
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
      this.createError.set(this.errorCopy.humanize(e));
    } finally {
      this.creating.set(false);
    }
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
    // Optimistic: mark the local row revoked immediately.
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
      this.listError.set(this.errorCopy.humanize(e));
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

  /** Drop every transient (created URL, per-session URLs, revoke/copy confirms). */
  private resetTransient(): void {
    this.createdUrl.set(null);
    this.createdCopied.set(false);
    this.createdMaxLabel.set(null);
    this.step.set("configure");
    this.sessionShares.set(new Map());
    this.confirmingRevokeId.set(null);
    this.copiedRowId.set(null);
    this.orgSheetOpen.set(false);
    this.closePerson();
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

  // --- Person share (mode B) -----------------------------------------------

  openPerson(): void {
    if (this.editing()) {
      return;
    }
    this.personEmail.set("");
    this.personPreview.set(null);
    this.personError.set(null);
    this.personResult.set(null);
    this.personStep.set("email");
    this.verifyMode.set(null);
    this.personOpen.set(true);
  }

  closePerson(): void {
    this.personOpen.set(false);
    this.verifyMode.set(null);
    this.personStep.set("email");
    this.personEmail.set("");
    this.personPreview.set(null);
    this.personError.set(null);
    this.personResult.set(null);
  }

  onPersonEmailInput(event: Event): void {
    this.personEmail.set((event.target as HTMLInputElement).value);
  }

  async submitPersonEmail(): Promise<void> {
    if (this.personBusy()) {
      return;
    }
    const email = this.personEmail().trim();
    if (!email) {
      this.personError.set("Enter an email address.");
      return;
    }
    this.personError.set(null);
    this.personBusy.set(true);
    try {
      const st = await this.ipc.accountStatus();
      if (!st.serverConfigured) {
        this.personError.set("Set a sharing server in Settings → Account first.");
        return;
      }
      if (!st.loggedIn || !st.unlockedForSharing) {
        this.personError.set("Sign in to your sharing account first (Settings → Account).");
        return;
      }
      const preview = await this.ipc.previewShareRecipient(email);
      this.personPreview.set(preview);
      if (!preview.registered) {
        this.personStep.set("suggest-link");
      } else if (preview.keyChanged) {
        this.personOpen.set(false);
        this.verifyMode.set("key-changed");
      } else if (preview.firstContact) {
        this.personOpen.set(false);
        this.verifyMode.set("first-contact");
      } else {
        await this.sendToUser();
      }
    } catch (e) {
      this.personError.set(this.errorCopy.humanize(e));
    } finally {
      this.personBusy.set(false);
    }
  }

  /** Fall back from the person flow to a protected link (the Configure form). */
  sendProtectedLinkInstead(): void {
    this.closePerson();
    this.step.set("configure");
  }

  async inviteAnyway(): Promise<void> {
    if (this.personBusy()) {
      return;
    }
    this.personBusy.set(true);
    try {
      await this.sendToUser();
    } finally {
      this.personBusy.set(false);
    }
  }

  async confirmVerifiedSend(): Promise<void> {
    if (this.personBusy()) {
      return;
    }
    this.personBusy.set(true);
    try {
      await this.sendToUser();
    } finally {
      this.personBusy.set(false);
    }
  }

  async confirmPersonConsent(): Promise<void> {
    if (this.personBusy()) {
      return;
    }
    this.personBusy.set(true);
    this.personError.set(null);
    try {
      await this.ipc.consentToShareEgress();
      await this.sendToUser();
    } catch (e) {
      this.personError.set(this.errorCopy.humanize(e));
    } finally {
      this.personBusy.set(false);
    }
  }

  /** Perform `shareNoteToUser` + render the outcome. Callers own `personBusy`. */
  private async sendToUser(): Promise<void> {
    const id = this.meetingId();
    if (!id) {
      return;
    }
    const email = this.personEmail().trim();
    this.personError.set(null);
    try {
      const res = await this.ipc.shareNoteToUser(id, email);
      this.verifyMode.set(null);
      this.personOpen.set(true);
      this.personStep.set("result");
      this.personResult.set(
        res.status === "invited"
          ? `Invited — they'll get it when they join Murmur. Ask them to install Murmur (macOS) and sign in with ${email}.`
          : "Sent.",
      );
      await this.refresh();
      this.changed.emit();
    } catch (e) {
      // A missing one-time upload consent is not an error the user should read — it is a STEP.
      // Matched on the `[share-consent]` code (`errcode::SHARE_CONSENT`), not on the word
      // "consent" appearing somewhere in a backend sentence.
      if (this.errorCopy.is(e, "share-consent")) {
        this.verifyMode.set(null);
        this.personOpen.set(true);
        this.personStep.set("consent");
        this.personError.set(null);
      } else {
        this.personError.set(this.errorCopy.humanize(e, "meeting-share"));
      }
    }
  }
}
