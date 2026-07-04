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
import { IpcService } from "../../core/ipc.service";
import type {
  AccountStatus,
  MyShareEntry,
  RecipientPreview,
} from "../../core/models";
import {
  ShareVerifySheetComponent,
  type ShareVerifyMode,
} from "./share-verify-sheet.component";

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
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ShareVerifySheetComponent],
  template: `
    <div class="share-panel">
      @if (!gateReady()) {
        <!-- ============================================================== -->
        <!-- 2.2 PRECONDITION GATE — fail closed, honest about WHY.          -->
        <!-- Renders only the FAILING preconditions + the right CTA.         -->
        <!-- ============================================================== -->
        <div class="panel-card gate empty-state">
          <span class="gate-mark" aria-hidden="true">
            <svg viewBox="0 0 24 24" width="26" height="26" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">
              <path d="M10 13a5 5 0 0 0 7.07 0l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71" />
              <path d="M14 11a5 5 0 0 0-7.07 0l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71" />
            </svg>
          </span>
          <p class="empty-title">Sharing isn't set up yet</p>
          <p class="gate-copy">
            Murmur can share this note as an end-to-end encrypted link. It uploads
            only the <strong>encrypted</strong> note — the decryption key never
            leaves your Mac.
          </p>
          <ul class="gate-reasons">
            @for (r of gateReasons(); track r.key) {
              <li>{{ r.text }}</li>
            }
          </ul>
          @if (gateError(); as err) {
            <p class="msg msg-error" role="alert">{{ err }}</p>
          }
          <!-- When the ONLY missing precondition is the session share-key and a
               cached account key exists, offer a one-tap Touch ID unlock inline;
               otherwise route to the password path via the shell (setupSharing). -->
          @if (canBiometricUnlock()) {
            <button
              type="button"
              class="btn btn-primary"
              (click)="unlockWithBiometric()"
              [disabled]="unlocking()"
            >
              {{ unlocking() ? "Unlocking…" : "Unlock with Touch ID" }}
            </button>
          } @else {
            <button type="button" class="btn btn-primary" (click)="setupSharing.emit()">
              {{ gateCta() }}
            </button>
          }
        </div>
      } @else {
        <!-- ============================================================== -->
        <!-- CONFIGURE (2.3) — password FIRST → expires → open limit →       -->
        <!-- consent → Create link. Replaced by CREATED after a create.     -->
        <!-- ============================================================== -->
        @if (step() === "configure") {
          <div class="panel-card create" role="group" aria-label="Create a share link">
            <span class="section-label">Create a share link</span>

            <!-- PASSWORD (first, focused). -->
            <div class="field">
              <span class="field-label">Password</span>
              <div class="pw-row">
                <input
                  [type]="showPassword() ? 'text' : 'password'"
                  class="input pw-input"
                  [value]="password()"
                  (input)="onPasswordInput($event)"
                  [disabled]="noPassword()"
                  placeholder="Add a password"
                  autocomplete="new-password"
                  aria-label="Share-link password"
                />
                <button
                  type="button"
                  class="btn btn-ghost pw-toggle"
                  (click)="showPassword.set(!showPassword())"
                  [attr.aria-label]="showPassword() ? 'Hide password' : 'Show password'"
                  [attr.aria-pressed]="showPassword()"
                >
                  {{ showPassword() ? "Hide" : "Show" }}
                </button>
              </div>
              @if (!noPassword() && password().length > 0) {
                <div class="pw-strength" [attr.data-level]="strength().level" aria-hidden="true">
                  <span class="pw-bars">
                    <i></i><i></i><i></i><i></i>
                  </span>
                  <span class="pw-strength-label">{{ strength().label }}</span>
                </div>
              }
              <span class="hint">
                A password strengthens the encryption, not just a gate. Share it
                out of band — never in the same message as the link.
              </span>
              <label class="check">
                <input
                  type="checkbox"
                  [checked]="noPassword()"
                  (change)="onNoPasswordChange($event)"
                />
                <span>No password <span class="hint-inline">(the link key alone protects it)</span></span>
              </label>
            </div>

            <!-- EXPIRES (.seg, default 7 days). -->
            <div class="field">
              <span class="field-label">Expires</span>
              <div class="seg" role="group" aria-label="Link expiry">
                @for (o of expiryOptions; track o.value) {
                  <button
                    type="button"
                    class="seg-btn"
                    [class.is-active]="expiry() === o.value"
                    [attr.aria-pressed]="expiry() === o.value"
                    (click)="expiry.set(o.value)"
                  >
                    {{ o.label }}
                  </button>
                }
              </div>
            </div>

            <!-- OPEN LIMIT (checkbox → stepper). -->
            <div class="field">
              <span class="field-label">Open limit</span>
              <label class="check">
                <input
                  type="checkbox"
                  [checked]="limitOpens()"
                  (change)="onLimitOpensChange($event)"
                />
                <span>Limit the number of opens</span>
              </label>
              @if (limitOpens()) {
                <div class="stepper" role="group" aria-label="Maximum opens">
                  <button
                    type="button"
                    class="btn btn-ghost step-btn"
                    (click)="stepOpens(-1)"
                    [disabled]="maxOpens() <= 1"
                    aria-label="Fewer opens"
                  >
                    −
                  </button>
                  <span class="step-value" aria-live="polite">{{ maxOpens() }}</span>
                  <button
                    type="button"
                    class="btn btn-ghost step-btn"
                    (click)="stepOpens(1)"
                    aria-label="More opens"
                  >
                    +
                  </button>
                  <span class="hint-inline">opens</span>
                </div>
              }
            </div>

            <!-- One-time share-egress CONSENT, inline (iff not yet consented). -->
            @if (!shareConsented()) {
              <div class="consent" role="group">
                <p class="consent-copy">
                  <span class="consent-mark" aria-hidden="true">ⓘ</span>
                  Uploads the <strong>encrypted</strong> note to your sharing
                  server. The decryption key stays on your Mac.
                </p>
                <button type="button" class="btn btn-ghost" (click)="grantConsent()" [disabled]="consenting()">
                  {{ consenting() ? "…" : "I understand" }}
                </button>
              </div>
            }

            @if (createError(); as err) {
              <p class="msg msg-error" role="alert">{{ err }}</p>
            }

            <button
              type="button"
              class="btn btn-primary create-btn"
              (click)="createLink()"
              [disabled]="!shareConsented() || creating() || editing()"
            >
              {{ creating() ? "Creating…" : "Create link" }}
            </button>
          </div>
        }

        <!-- ============================================================== -->
        <!-- CREATED (2.4) — one-time reveal. The URL lives ONLY in the      -->
        <!-- transient createdUrl signal, cleared on Done / navigate.        -->
        <!-- ============================================================== -->
        @if (step() === "created" && createdUrl(); as url) {
          <div class="panel-card created" role="group" aria-label="Share link created">
            <span class="section-label created-head">
              <span class="created-tick" aria-hidden="true">✓</span> Link created
            </span>

            <div class="link-row">
              <input
                type="text"
                class="input link-input"
                [value]="url"
                readonly
                aria-label="Share link"
              />
              <button type="button" class="btn" (click)="copyCreated()">
                {{ createdCopied() ? "Copied" : "Copy" }}
              </button>
            </div>

            <p class="warn">
              <span class="warn-mark" aria-hidden="true">⚠</span>
              This is the only time we can show this link. The decryption key
              lives in the link itself and is never stored. If you lose it, revoke
              and create a new one — we can't show it again.
            </p>

            <div class="created-meta">
              @if (createdWithPassword()) {
                <span class="meta-chip">🔒 Password-protected</span>
                <span class="dot" aria-hidden="true">·</span>
              }
              <span class="meta-chip">{{ createdExpiryLabel() }}</span>
              @if (createdMaxLabel(); as ml) {
                <span class="dot" aria-hidden="true">·</span>
                <span class="meta-chip">{{ ml }}</span>
              }
            </div>

            <div class="created-actions">
              <button type="button" class="btn btn-primary" (click)="createAnother()">
                Create another
              </button>
              <button type="button" class="btn btn-ghost" (click)="done()">Done</button>
            </div>
          </div>
        }

        <!-- ============================================================== -->
        <!-- MANAGE (2.5) — active links for THIS note. Always visible.      -->
        <!-- No Copy for links not created this session (key not stored).    -->
        <!-- ============================================================== -->
        <div class="panel-card manage">
          <div class="manage-head">
            <span class="section-label">Active links for this note</span>
            <span class="count">{{ linkRows().length }}</span>
            <button
              type="button"
              class="btn btn-ghost mini-btn"
              (click)="refresh()"
              [disabled]="loading()"
            >
              {{ loading() ? "Refreshing…" : "Refresh" }}
            </button>
            @if (listError(); as err) {
              <span class="msg msg-error" role="alert">{{ err }}</span>
            }
          </div>

          @if (loading() && !linkRows().length) {
            <div class="skeleton" aria-hidden="true">
              <span></span><span></span><span></span>
            </div>
          } @else {
            <ul class="list">
              @for (r of linkRows(); track r.shareId) {
                <li class="row">
                  <div class="row-meta">
                    @switch (r.state) {
                      @case ("revoked") {
                        <span class="pill is-muted"><i class="pill-dot"></i>Revoked</span>
                      }
                      @case ("limit") {
                        <span class="pill is-warning"><i class="pill-dot"></i>Limit reached</span>
                      }
                      @case ("expired") {
                        <span class="pill is-muted"><i class="pill-dot"></i>Expired</span>
                      }
                      @default {
                        <span class="pill is-success"><i class="pill-dot"></i>Active</span>
                      }
                    }
                    @if (r.locked) {
                      <span class="row-lock">🔒 Locked</span>
                    } @else {
                      <span class="row-when">Created {{ formatShareDate(r.createdAt) }}</span>
                      <span class="row-sep" aria-hidden="true">·</span>
                      @if (r.copyUrl) {
                        <span>{{ r.passwordProtected ? "🔒" : "no pw" }}</span>
                        <span class="row-sep" aria-hidden="true">·</span>
                      }
                      <span>{{ r.usageLabel }}</span>
                      <span class="row-sep" aria-hidden="true">·</span>
                      <span>{{ r.expiryLabel }}</span>
                    }
                  </div>
                  <div class="row-actions">
                    @if (r.state !== "revoked") {
                      <button
                        type="button"
                        class="btn btn-ghost mini-btn"
                        (click)="copyRow(r.shareId)"
                        [disabled]="!r.copyUrl"
                        [attr.title]="
                          r.copyUrl
                            ? null
                            : 'The link key isn’t stored on this device — revoke and create a new link to share again.'
                        "
                      >
                        {{ copiedRowId() === r.shareId ? "Copied" : "Copy" }}
                      </button>
                      @if (confirmingRevokeId() === r.shareId) {
                        <span class="hint-inline">Revoke?</span>
                        <button
                          type="button"
                          class="btn btn-danger mini-btn"
                          (click)="revokeRow(r.shareId)"
                          [disabled]="revokingId() === r.shareId"
                        >
                          {{ revokingId() === r.shareId ? "Revoking…" : "Confirm" }}
                        </button>
                        <button
                          type="button"
                          class="btn btn-ghost mini-btn"
                          (click)="confirmingRevokeId.set(null)"
                          [disabled]="revokingId() === r.shareId"
                        >
                          Cancel
                        </button>
                      } @else {
                        <button
                          type="button"
                          class="btn btn-ghost mini-btn revoke-btn"
                          (click)="confirmingRevokeId.set(r.shareId)"
                        >
                          Revoke
                        </button>
                      }
                    }
                  </div>
                </li>
              } @empty {
                <li class="row-empty">
                  <p class="empty">No active links for this note. Create one above.</p>
                </li>
              }
            </ul>
            @if (linkRows().length > 0) {
              <p class="hint manage-note">
                Links can't be shown again after creation. To re-share, create a new
                link.
              </p>
            }
          }
        </div>

        <!-- ============================================================== -->
        <!-- 2.6 SHARE WITH A PERSON (mode B) — same gate, existing flow.    -->
        <!-- ============================================================== -->
        <div class="panel-card person">
          <div class="manage-head">
            <span class="section-label">Share with a person</span>
            @if (peopleShareCount() > 0) {
              <span class="hint-inline">
                Shared with {{ peopleShareCount() }}
                {{ peopleShareCount() === 1 ? "person" : "people" }}
              </span>
            }
          </div>

          @if (!personOpen() && !verifyMode()) {
            <button
              type="button"
              class="btn btn-ghost person-btn"
              (click)="openPerson()"
              [disabled]="personBusy() || editing()"
            >
              Share with a Murmur user
            </button>
          }

          @if (personOpen()) {
            <div class="person-flow" role="group" aria-label="Share with a person">
              @switch (personStep()) {
                @case ("email") {
                  <p class="consent-copy">
                    Share this note directly with another Murmur user. It's
                    end-to-end encrypted to their account key.
                  </p>
                  <div class="person-row">
                    <input
                      type="email"
                      class="input person-input"
                      [value]="personEmail()"
                      (input)="onPersonEmailInput($event)"
                      (keydown.enter)="submitPersonEmail()"
                      placeholder="colleague@example.com"
                      autocomplete="off"
                      spellcheck="false"
                      aria-label="Recipient email"
                      [disabled]="personBusy()"
                    />
                    <button type="button" class="btn btn-primary" (click)="submitPersonEmail()" [disabled]="personBusy() || !personEmail().trim()">
                      {{ personBusy() ? "Checking…" : "Continue" }}
                    </button>
                    <button type="button" class="btn btn-ghost" (click)="closePerson()" [disabled]="personBusy()">
                      Cancel
                    </button>
                  </div>
                }
                @case ("suggest-link") {
                  <p class="consent-copy">
                    They don't use Murmur yet — send a protected link instead?
                    <span class="pill is-accent">Recommended</span>
                  </p>
                  <div class="person-row">
                    <button type="button" class="btn btn-primary" (click)="sendProtectedLinkInstead()" [disabled]="personBusy()">
                      Send a protected link
                    </button>
                    <button type="button" class="btn btn-ghost" (click)="inviteAnyway()" [disabled]="personBusy()">
                      {{ personBusy() ? "Inviting…" : "Invite them anyway" }}
                    </button>
                    <button type="button" class="btn btn-ghost" (click)="closePerson()" [disabled]="personBusy()">
                      Cancel
                    </button>
                  </div>
                }
                @case ("consent") {
                  <p class="consent-copy">
                    This uploads the encrypted note to your sharing server. The note
                    is end-to-end encrypted — the server can't read it — but it does
                    leave this Mac.
                  </p>
                  <div class="person-row">
                    <button type="button" class="btn btn-primary" (click)="confirmPersonConsent()" [disabled]="personBusy()">
                      {{ personBusy() ? "Sharing…" : "Confirm & share" }}
                    </button>
                    <button type="button" class="btn btn-ghost" (click)="closePerson()" [disabled]="personBusy()">
                      Cancel
                    </button>
                  </div>
                }
                @case ("result") {
                  <p class="person-result">{{ personResult() }}</p>
                  <div class="person-row">
                    <button type="button" class="btn" (click)="closePerson()">Done</button>
                  </div>
                }
              }
              @if (personError(); as perr) {
                <p class="msg msg-error" role="alert">{{ perr }}</p>
              }
            </div>
          }
        </div>

        @if (verifyMode(); as mode) {
          <app-share-verify-sheet
            [email]="personEmail()"
            [fingerprint]="personPreview()?.fingerprint ?? ''"
            [mode]="mode"
            [busy]="personBusy()"
            [error]="personError()"
            (confirm)="confirmVerifiedSend()"
            (cancelled)="closePerson()"
          />
        }
      }
    </div>
  `,
  styles: [
    `
      :host {
        display: block;
      }
      .share-panel {
        display: flex;
        flex-direction: column;
        /* §5 rhythm: --space-6 between top-level sections (calm whitespace). */
        gap: var(--space-6);
        /* Fill the note-detail content column (like the Note panel) — no inset
           cap, so the Share cards + controls use the full width instead of
           leaving dead space on the right. */
        width: 100%;
        animation: rise 320ms var(--transition) both;
      }
      .panel-card {
        padding: var(--space-5);
      }
      .msg {
        margin: 0;
        color: var(--text-secondary);
        font-size: 0.85rem;
      }
      .msg-error {
        color: var(--danger);
      }
      .hint {
        margin: 0;
        color: var(--text-secondary);
        font-size: 0.8125rem;
        line-height: 1.5;
      }
      .hint-inline {
        color: var(--text-muted);
        font-size: 0.8125rem;
      }
      .input {
        width: 100%;
        height: 38px;
        padding: 0 var(--space-3);
        border: 1px solid var(--border);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        color: var(--text-primary);
        font-size: 0.9rem;
      }

      /* --- 2.2 Precondition gate --- */
      .gate {
        gap: var(--space-3);
      }
      .gate-mark {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 52px;
        height: 52px;
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        border: 1px solid var(--border);
        color: var(--accent-hover);
      }
      .gate-copy {
        margin: 0;
        max-width: 30rem;
        color: var(--text-secondary);
        font-size: 0.9rem;
        line-height: 1.55;
      }
      .gate-copy strong {
        color: var(--text-primary);
      }
      .gate-reasons {
        margin: var(--space-1) 0 0;
        padding: 0;
        list-style: none;
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        color: var(--text-secondary);
        font-size: 0.85rem;
      }
      .gate-reasons li::before {
        content: "•";
        margin-right: var(--space-2);
        color: var(--text-muted);
      }

      /* --- 2.3 Configure --- */
      .create {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }
      .field {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .field-label {
        color: var(--text-primary);
        font-size: 0.9rem;
        font-weight: 600;
      }
      .pw-row {
        display: flex;
        align-items: center;
        gap: var(--space-2);
      }
      .pw-input {
        flex: 1 1 auto;
        min-width: 0;
      }
      .pw-toggle {
        flex: none;
        height: 38px;
      }
      .pw-strength {
        display: flex;
        align-items: center;
        gap: var(--space-2);
      }
      .pw-bars {
        display: inline-flex;
        gap: 3px;
      }
      .pw-bars i {
        width: 22px;
        height: 4px;
        border-radius: var(--radius-pill);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
      }
      .pw-strength[data-level="1"] .pw-bars i:nth-child(-n + 1),
      .pw-strength[data-level="2"] .pw-bars i:nth-child(-n + 2),
      .pw-strength[data-level="3"] .pw-bars i:nth-child(-n + 3),
      .pw-strength[data-level="4"] .pw-bars i:nth-child(-n + 4) {
        background: var(--accent);
        border-color: transparent;
      }
      .pw-strength[data-level="4"] .pw-bars i {
        background: var(--success);
      }
      .pw-strength-label {
        color: var(--text-muted);
        font-size: 0.8125rem;
      }
      .check {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        color: var(--text-secondary);
        font-size: 0.875rem;
        cursor: pointer;
      }
      .check input {
        flex: none;
      }
      .stepper {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
      }
      .step-btn {
        width: 34px;
        height: 34px;
        padding: 0;
        font-size: 1.1rem;
        line-height: 1;
      }
      .step-value {
        min-width: 2ch;
        text-align: center;
        color: var(--text-primary);
        font-family: var(--font-mono);
        font-size: 0.95rem;
      }
      .consent {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-3);
        padding: var(--space-3);
        border: 1px solid var(--border-subtle);
        border-radius: var(--radius-md);
        background: var(--surface-input);
      }
      .consent-copy {
        margin: 0;
        flex: 1 1 18rem;
        min-width: 0;
        color: var(--text-secondary);
        font-size: 0.85rem;
        line-height: 1.5;
      }
      .consent-copy strong {
        color: var(--text-primary);
      }
      .consent-mark {
        color: var(--accent-hover);
      }
      .create-btn {
        align-self: flex-start;
      }

      /* --- 2.4 Created --- */
      .created {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .created-head {
        display: inline-flex;
        align-items: center;
        gap: var(--space-2);
        color: var(--success);
      }
      .created-tick {
        color: var(--success);
      }
      .link-row {
        display: flex;
        align-items: center;
        gap: var(--space-2);
      }
      .link-input {
        flex: 1 1 20rem;
        min-width: 0;
        font-family: var(--font-mono);
        font-size: 0.85rem;
        user-select: text;
        -webkit-user-select: text;
      }
      .link-row .btn {
        flex: none;
      }
      .warn {
        margin: 0;
        padding: var(--space-3);
        border-radius: var(--radius-md);
        background: var(--warning-soft);
        color: var(--text-primary);
        font-size: 0.85rem;
        line-height: 1.55;
      }
      .warn-mark {
        color: var(--warning);
        font-weight: 700;
      }
      .created-meta {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: var(--space-2);
        color: var(--text-secondary);
        font-size: 0.85rem;
      }
      .dot {
        color: var(--text-muted);
      }
      .created-actions {
        display: flex;
        gap: var(--space-2);
        flex-wrap: wrap;
      }

      /* --- 2.5 Manage --- */
      .manage,
      .person {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .manage-head {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: var(--space-2);
      }
      .mini-btn {
        height: 32px;
        padding: 0 var(--space-3);
        font-size: 0.8125rem;
      }
      .revoke-btn {
        color: var(--danger);
      }
      .list {
        list-style: none;
        margin: 0;
        padding: 0;
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .row {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-2);
        padding: var(--space-3);
        border: 1px solid var(--border-subtle);
        border-radius: var(--radius-md);
      }
      .row-meta,
      .row-actions {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: var(--space-2);
      }
      .row-meta {
        min-width: 0;
        color: var(--text-secondary);
        font-size: 0.8125rem;
        font-variant-numeric: tabular-nums;
      }
      .row-sep {
        color: var(--text-muted);
      }
      .row-lock {
        color: var(--text-muted);
      }
      .pill.is-muted {
        background: var(--surface-input);
        border-color: transparent;
        color: var(--text-muted);
      }
      .row-empty {
        list-style: none;
        padding: var(--space-4) var(--space-2);
        text-align: center;
      }
      .manage-note {
        color: var(--text-muted);
      }
      .skeleton {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .skeleton span {
        height: 44px;
        border-radius: var(--radius-md);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
        animation: pulse 1.4s ease-in-out infinite;
      }
      .skeleton span:nth-child(2) {
        animation-delay: 0.15s;
      }
      .skeleton span:nth-child(3) {
        animation-delay: 0.3s;
      }
      @keyframes pulse {
        0%,
        100% {
          opacity: 0.5;
        }
        50% {
          opacity: 0.85;
        }
      }

      /* --- 2.6 Person --- */
      .person-btn {
        align-self: flex-start;
      }
      .person-flow {
        display: flex;
        flex-direction: column;
        gap: var(--space-3);
      }
      .person-row {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        gap: var(--space-2);
      }
      .person-row .btn {
        flex: none;
      }
      .person-input {
        flex: 1 1 18rem;
        min-width: 0;
      }
      .person-result {
        margin: 0;
        color: var(--text-primary);
        font-size: 0.9rem;
        line-height: 1.55;
      }

      @media (prefers-reduced-motion: reduce) {
        .share-panel {
          animation: none;
        }
        .skeleton span {
          animation: none;
        }
      }
    `,
  ],
})
export class SharePanelComponent {
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);

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

  /** Emitted after any create/revoke so the shell can refresh a dependent count. */
  readonly changed = output<void>();
  /** Emitted when the gate's CTA is pressed → the shell routes to Settings › Sharing. */
  readonly setupSharing = output<void>();

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
      { injector: this.injector, allowSignalWrites: true },
    );

    // A meeting change clears the transient created-link + per-session URLs — a
    // stashed URL must NEVER survive into another note's list.
    effect(
      () => {
        this.meetingId();
        this.resetTransient();
      },
      { injector: this.injector, allowSignalWrites: true },
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
    } catch (e) {
      this.listError.set(String(e));
      this.gateError.set(String(e));
    } finally {
      this.loading.set(false);
    }
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
      this.gateError.set(this.friendlyUnlockError(String(e)));
    } finally {
      this.unlocking.set(false);
    }
  }

  /** Turn a raw biometric-unlock error into a friendly fall-back message. */
  private friendlyUnlockError(raw: string): string {
    if (/cancel/i.test(raw)) {
      return "Touch ID was cancelled. Use Unlock for sharing to unlock with your password.";
    }
    return "Couldn't unlock with Touch ID. Use Unlock for sharing to unlock with your password.";
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
      this.createError.set(String(e));
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
      this.createError.set(String(e));
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

  /** Drop every transient (created URL, per-session URLs, revoke/copy confirms). */
  private resetTransient(): void {
    this.createdUrl.set(null);
    this.createdCopied.set(false);
    this.createdMaxLabel.set(null);
    this.step.set("configure");
    this.sessionShares.set(new Map());
    this.confirmingRevokeId.set(null);
    this.copiedRowId.set(null);
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
      this.personError.set(String(e));
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
      this.personError.set(String(e));
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
      const msg = String(e);
      if (/consent/i.test(msg)) {
        this.verifyMode.set(null);
        this.personOpen.set(true);
        this.personStep.set("consent");
        this.personError.set(null);
      } else {
        this.personError.set(msg);
      }
    }
  }
}
