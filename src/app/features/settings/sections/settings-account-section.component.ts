import {
  ChangeDetectionStrategy,
  Component,
  inject,
  signal,
} from "@angular/core";
import { FormControl, ReactiveFormsModule } from "@angular/forms";
import { IpcService } from "../../../core/ipc.service";
import type { AccountStatus } from "../../../core/models";

/**
 * Settings → Account section (M3-CLIENT): the sharing account behind zero-knowledge
 * note LINK shares. Lean, signals-first surface mirroring the sibling
 * settings-privacy-section shape (`:host { display: contents }` + `.section-stack`
 * + frosted `.card`, global `.btn`/`.btn-primary`/`.btn-ghost`, `var(--token)`).
 *
 * Everything talks to the Rust core through {@link IpcService}: `accountStatus`
 * loads once into a signal on init; login / signup / logout write the resulting
 * `AccountStatus` back into that signal. The sharing-server base URL round-trips
 * through the normal config path (`getConfig` → mutate `shareBaseUrl` → `saveConfig`).
 *
 * SECURITY: the password lives ONLY in a per-form FormControl and is cleared after
 * each submit — it is never stored in a persistent signal and never logged.
 */
@Component({
  selector: "app-settings-account-section",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
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
              @if (!st.unlockedForSharing) {
                <p class="text-secondary account-note">
                  Sign in again to share — your account key isn't loaded in this
                  session.
                </p>
              }
            </div>
          } @else {
            <!-- (2b) Login form (primary path) -->
            <div class="account-section">
              <span class="account-section-label text-muted">Sign in</span>
              <div class="field-grid">
                <input
                  type="email"
                  class="text-input"
                  [formControl]="emailControl"
                  placeholder="you@example.com"
                  autocomplete="username"
                  spellcheck="false"
                  aria-label="Email"
                />
                <input
                  type="password"
                  class="text-input"
                  [formControl]="passwordControl"
                  placeholder="Password"
                  autocomplete="current-password"
                  aria-label="Password"
                />
              </div>
              <div class="account-actions">
                <button
                  type="button"
                  class="btn btn-primary"
                  (click)="login()"
                  [disabled]="busy()"
                >
                  {{ busy() ? "Signing in…" : "Sign in" }}
                </button>
                <button
                  type="button"
                  class="btn btn-ghost"
                  (click)="toggleSignup()"
                >
                  {{ showSignup() ? "Cancel" : "Create an account" }}
                </button>
              </div>
              @if (loginError(); as lerr) {
                <p class="text-danger account-note">{{ lerr }}</p>
              }
            </div>

            <!-- (2c) Sign-up (secondary, collapsed under a toggle) -->
            @if (showSignup()) {
              <div class="account-section signup-section">
                <span class="account-section-label text-muted"
                  >Create an account</span
                >
                <p class="text-secondary account-note">
                  Enter your email to receive a 6-digit code, then set a
                  password. Your password never leaves this Mac.
                </p>
                <div class="field-grid">
                  <input
                    type="email"
                    class="text-input"
                    [formControl]="signupEmailControl"
                    placeholder="you@example.com"
                    autocomplete="username"
                    spellcheck="false"
                    aria-label="Email"
                  />
                  <input
                    type="text"
                    inputmode="numeric"
                    class="text-input"
                    [formControl]="signupCodeControl"
                    placeholder="6-digit code"
                    autocomplete="one-time-code"
                    spellcheck="false"
                    aria-label="Verification code"
                  />
                  <input
                    type="password"
                    class="text-input"
                    [formControl]="signupPasswordControl"
                    placeholder="Choose a password"
                    autocomplete="new-password"
                    aria-label="Password"
                  />
                </div>
                <label class="toggle-row">
                  <span class="toggle-copy">
                    <span class="toggle-title">Save a recovery phrase</span>
                    <span class="text-secondary toggle-sub">
                      Generates a 24-word recovery phrase — the only way to
                      recover a forgotten password. You can skip this.
                    </span>
                  </span>
                  <input type="checkbox" [formControl]="saveRecoveryControl" />
                </label>
                <div class="account-actions">
                  <button
                    type="button"
                    class="btn btn-primary"
                    (click)="signup()"
                    [disabled]="busy()"
                  >
                    {{ busy() ? "Creating…" : "Create account" }}
                  </button>
                </div>
                @if (signupError(); as serr) {
                  <p class="text-danger account-note">{{ serr }}</p>
                }
                @if (signupNotice(); as note) {
                  <p class="text-secondary account-note">{{ note }}</p>
                }
                @if (recoveryPhrase(); as phrase) {
                  <div class="recovery">
                    <span class="account-section-label text-muted"
                      >Recovery phrase</span
                    >
                    <p class="recovery-block" role="text">{{ phrase }}</p>
                    <div class="account-actions">
                      <button
                        type="button"
                        class="btn"
                        (click)="copyRecovery(phrase)"
                      >
                        {{ recoveryCopied() ? "Copied" : "Copy" }}
                      </button>
                    </div>
                    <p class="text-secondary account-note">
                      Save this — it's the only way to recover a forgotten
                      password.
                    </p>
                  </div>
                }
              </div>
            }
          }
        } @else {
          <p class="text-muted account-note">Loading account…</p>
        }
      </div>
    </div>
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
      .signup-section {
        padding-top: var(--space-4);
        border-top: 1px solid var(--border-subtle);
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

      .field-grid {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        max-width: 24rem;
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

      /* Recovery-phrase toggle (mirrors the privacy-section toggle rows). */
      .toggle-row {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-4);
        cursor: pointer;
        max-width: 24rem;
      }
      .toggle-copy {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
      }
      .toggle-title {
        color: var(--text-primary);
        font-size: 0.95rem;
        font-weight: 550;
      }
      .toggle-sub {
        font-size: 0.85rem;
        line-height: 1.55;
      }

      /* The revealed recovery phrase — a quiet inset well, selectable. */
      .recovery {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
        padding: var(--space-4);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        border: 1px solid var(--border-subtle);
      }
      .recovery-block {
        margin: 0;
        padding: var(--space-3);
        border-radius: var(--radius-md);
        background: var(--surface-raised);
        border: 1px solid var(--glass-border);
        color: var(--text-primary);
        font-family: var(--font-mono);
        font-size: 0.875rem;
        line-height: 1.7;
        user-select: text;
        -webkit-user-select: text;
        overflow-wrap: anywhere;
      }
    `,
  ],
})
export class SettingsAccountSectionComponent {
  private readonly ipc = inject(IpcService);

  /** The current sharing-account session; `null` until the first load resolves. */
  private readonly _status = signal<AccountStatus | null>(null);
  readonly status = this._status.asReadonly();

  /** True while any login/signup/logout IPC call is in flight (debounces clicks). */
  private readonly _busy = signal(false);
  readonly busy = this._busy.asReadonly();

  /** Whether the secondary sign-up affordance is expanded (login stays primary). */
  private readonly _showSignup = signal(false);
  readonly showSignup = this._showSignup.asReadonly();

  // ── Server base URL ──────────────────────────────────────────────────────
  readonly serverControl = new FormControl("", { nonNullable: true });
  private readonly _savingServer = signal(false);
  readonly savingServer = this._savingServer.asReadonly();
  private readonly _serverError = signal<string | null>(null);
  readonly serverError = this._serverError.asReadonly();

  // ── Login form (password lives ONLY here; cleared after submit) ───────────
  readonly emailControl = new FormControl("", { nonNullable: true });
  readonly passwordControl = new FormControl("", { nonNullable: true });
  private readonly _loginError = signal<string | null>(null);
  readonly loginError = this._loginError.asReadonly();

  // ── Sign-up form ─────────────────────────────────────────────────────────
  readonly signupEmailControl = new FormControl("", { nonNullable: true });
  readonly signupCodeControl = new FormControl("", { nonNullable: true });
  readonly signupPasswordControl = new FormControl("", { nonNullable: true });
  readonly saveRecoveryControl = new FormControl(false, { nonNullable: true });
  private readonly _signupError = signal<string | null>(null);
  readonly signupError = this._signupError.asReadonly();
  private readonly _signupNotice = signal<string | null>(null);
  readonly signupNotice = this._signupNotice.asReadonly();
  /** The 24-word recovery phrase, shown once after a `saveRecovery` signup. */
  private readonly _recoveryPhrase = signal<string | null>(null);
  readonly recoveryPhrase = this._recoveryPhrase.asReadonly();
  private readonly _recoveryCopied = signal(false);
  readonly recoveryCopied = this._recoveryCopied.asReadonly();

  constructor() {
    // Fire-and-forget one-shot load — the section fetches its state exactly once
    // on construction (no signal is read, so no NG0600 / no effect needed).
    void this.reload();
  }

  /** Load the current account status + seed the server URL input from config. */
  private async reload(): Promise<void> {
    try {
      const st = await this.ipc.accountStatus();
      this._status.set(st);
    } catch (e) {
      this._loginError.set(String(e));
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
  }

  toggleSignup(): void {
    this._showSignup.update((v) => !v);
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

  /** Sign in (OPAQUE) — writes the returned AccountStatus into the status signal. */
  async login(): Promise<void> {
    if (this._busy()) return;
    const email = this.emailControl.value.trim();
    const password = this.passwordControl.value;
    if (!email || !password) {
      this._loginError.set("Enter your email and password.");
      return;
    }
    this._loginError.set(null);
    this._busy.set(true);
    try {
      const st = await this.ipc.accountLogin(email, password);
      this._status.set(st);
      this.passwordControl.setValue(""); // never persist the password
    } catch (e) {
      this._loginError.set(String(e));
    } finally {
      this._busy.set(false);
    }
  }

  /**
   * Create a sharing account. `accountSignup` returns the 24-word recovery
   * phrase ONLY when saveRecovery is checked (else null → "now sign in").
   */
  async signup(): Promise<void> {
    if (this._busy()) return;
    const email = this.signupEmailControl.value.trim();
    const code = this.signupCodeControl.value.trim();
    const password = this.signupPasswordControl.value;
    const saveRecovery = this.saveRecoveryControl.value;
    if (!email || !code || !password) {
      this._signupError.set("Enter your email, the 6-digit code, and a password.");
      return;
    }
    this._signupError.set(null);
    this._signupNotice.set(null);
    this._recoveryPhrase.set(null);
    this._busy.set(true);
    try {
      const phrase = await this.ipc.accountSignup(
        email,
        code,
        password,
        saveRecovery,
      );
      if (phrase) {
        this._recoveryPhrase.set(phrase);
      } else {
        this._signupNotice.set("Account created — now sign in.");
      }
      // Prefill the login form for the freshly-created account; clear secrets.
      this.emailControl.setValue(email);
      this.signupPasswordControl.setValue("");
      this.signupCodeControl.setValue("");
    } catch (e) {
      this._signupError.set(String(e));
    } finally {
      this._busy.set(false);
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
      this._loginError.set(String(e));
    } finally {
      this._busy.set(false);
    }
  }

  /** Copy the recovery phrase to the clipboard and briefly confirm. */
  async copyRecovery(phrase: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(phrase);
      this._recoveryCopied.set(true);
    } catch {
      // Clipboard unavailable — the phrase stays visible and selectable.
    }
  }
}
