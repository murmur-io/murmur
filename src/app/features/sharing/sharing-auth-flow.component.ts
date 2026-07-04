import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  inject,
  output,
  signal,
  viewChild,
} from "@angular/core";
import { FormControl, ReactiveFormsModule } from "@angular/forms";
import { IpcService } from "../../core/ipc.service";
import type { AccountStatus } from "../../core/models";

/**
 * The reusable multi-step sharing-account flow. One state machine, two entry
 * doors (create / sign in), driven by the SAME component whether it is launched
 * from the init gateway (`/welcome`) or from Settings → Account.
 */
type Step =
  | "choose" // "Create account" | "I already have one"
  | "create-email" // email input + [Send code]
  | "create-code" // 6-digit code input (verified later, inside account_signup)
  | "create-password" // password + confirm + optional recovery → [Create account]
  | "create-recovery" // reveal the 24-word phrase (only when saveRecovery)
  | "signin" // email + password → [Sign in]
  | "done"; // brief success, completed already emitted

/**
 * SharingAuthFlowComponent — the ONE reusable account surface (contract §4).
 *
 * SURFACE-AGNOSTIC: `:host { display: contents }` and NO own frosted/opaque
 * background — the HOST owns the surface (the gateway wraps this in a full-bleed
 * `.card` over the aurora; Settings wraps it in an OPAQUE `--surface-overlay`
 * modal). That is what keeps this clear of trap T3 — this component never paints
 * a floating panel of its own.
 *
 * State: non-secret step state lives in signals; passwords live ONLY in
 * transient `FormControl`s, read at submit and cleared immediately after — never
 * a persistent signal, never logged (mirrors the retired inline form's
 * discipline). IPC results (an `AccountStatus`) land in the `completed` output.
 *
 * THE BUG THIS FIXES: the create path's first leg calls `accountSendCode(email)`
 * → `account_send_code` → `ShareClient::signup` → `POST /v1/auth/signup`, which
 * is what actually triggers the server to EMAIL the 6-digit code. The retired
 * form never issued that call, so no code was ever sent. After collecting the
 * code we call the existing `account_signup` (which verify_email-exchanges the
 * code internally) and CHAIN `account_login` so the user ends up signed in in
 * one pass (account_signup opens no session).
 */
@Component({
  selector: "app-sharing-auth-flow",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
  template: `
    <div class="flow">
      @switch (step()) {
        <!-- ─────────────────────────── CHOOSE ─────────────────────────── -->
        @case ("choose") {
          <div class="flow-head">
            <h2 class="flow-title">Sharing account</h2>
            <p class="flow-sub text-secondary">
              Share a note as a private, end-to-end-encrypted link. Only
              ciphertext and wrapped keys ever leave this Mac — the server can
              never read your notes.
            </p>
          </div>
          <div class="doors">
            <button
              type="button"
              class="door"
              (click)="startCreate()"
              [disabled]="busy()"
            >
              <span class="door-icon" aria-hidden="true">
                <svg viewBox="0 0 20 20" width="20" height="20" fill="none">
                  <circle
                    cx="10"
                    cy="7"
                    r="3.2"
                    stroke="currentColor"
                    stroke-width="1.5"
                  />
                  <path
                    d="M4.5 16.2c.9-2.6 3-4 5.5-4s4.6 1.4 5.5 4"
                    stroke="currentColor"
                    stroke-width="1.5"
                    stroke-linecap="round"
                  />
                </svg>
              </span>
              <span class="door-name">Create an account</span>
              <span class="door-sub text-secondary">
                New here — set up sharing in a few steps.
              </span>
            </button>

            <button
              type="button"
              class="door"
              (click)="startSignin()"
              [disabled]="busy()"
            >
              <span class="door-icon" aria-hidden="true">
                <svg viewBox="0 0 20 20" width="20" height="20" fill="none">
                  <path
                    d="M9 3.5H5.5A1.5 1.5 0 0 0 4 5v10a1.5 1.5 0 0 0 1.5 1.5H9M12.5 13l3-3-3-3M15 10H8"
                    stroke="currentColor"
                    stroke-width="1.5"
                    stroke-linecap="round"
                    stroke-linejoin="round"
                  />
                </svg>
              </span>
              <span class="door-name">I already have one</span>
              <span class="door-sub text-secondary">
                Sign in to this Mac to start sharing.
              </span>
            </button>
          </div>

          <div class="flow-nav">
            <button
              type="button"
              class="btn btn-ghost"
              (click)="cancel()"
              [disabled]="busy()"
            >
              Cancel
            </button>
          </div>
        }

        <!-- ─────────────────────────── CREATE — EMAIL ──────────────────── -->
        @case ("create-email") {
          <div class="flow-head">
            <h2 class="flow-title">Create your account</h2>
            <p class="flow-sub text-secondary">
              Enter your email and we'll send a 6-digit code to confirm it.
            </p>
          </div>
          <label class="field">
            <span class="flow-label">Email</span>
            <input
              #emailField
              type="email"
              class="field-input"
              [value]="email()"
              (input)="onEmail($event)"
              (keydown.enter)="sendCode()"
              placeholder="you@example.com"
              autocomplete="username"
              spellcheck="false"
              [disabled]="busy()"
            />
          </label>

          @if (error(); as err) {
            <p class="flow-error text-danger" role="alert">{{ err }}</p>
          }

          <div class="flow-nav">
            <button
              type="button"
              class="btn btn-ghost"
              (click)="backToChoose()"
              [disabled]="busy()"
            >
              Back
            </button>
            <span class="flow-nav-spacer"></span>
            <button
              type="button"
              class="btn btn-primary"
              (click)="sendCode()"
              [disabled]="busy()"
            >
              {{ busy() ? "Sending…" : "Send code" }}
            </button>
          </div>
        }

        <!-- ─────────────────────────── CREATE — CODE ───────────────────── -->
        @case ("create-code") {
          <div class="flow-head">
            <h2 class="flow-title">Enter the code</h2>
            <p class="flow-sub text-secondary">
              We sent a 6-digit code to
              <span class="flow-email">{{ email() }}</span
              >. Enter it below. If it doesn't arrive, check your spam or resend.
            </p>
          </div>
          <label class="field">
            <span class="flow-label">Verification code</span>
            <input
              #codeField
              type="text"
              inputmode="numeric"
              class="field-input code-input"
              [value]="code()"
              (input)="onCode($event)"
              (keydown.enter)="codeContinue()"
              placeholder="000000"
              autocomplete="one-time-code"
              spellcheck="false"
              maxlength="6"
              [disabled]="busy()"
            />
          </label>

          @if (notice(); as note) {
            <p class="flow-note text-secondary" role="status">{{ note }}</p>
          }
          @if (error(); as err) {
            <p class="flow-error text-danger" role="alert">{{ err }}</p>
          }

          <div class="flow-nav">
            <button
              type="button"
              class="btn btn-ghost"
              (click)="goEmail()"
              [disabled]="busy()"
            >
              Back
            </button>
            <button
              type="button"
              class="link-btn text-muted resend"
              (click)="resendCode()"
              [disabled]="busy()"
            >
              {{ busy() ? "Sending…" : "Resend code" }}
            </button>
            <span class="flow-nav-spacer"></span>
            <button
              type="button"
              class="btn btn-primary"
              (click)="codeContinue()"
              [disabled]="busy()"
            >
              Continue
            </button>
          </div>
        }

        <!-- ─────────────────────────── CREATE — PASSWORD ───────────────── -->
        @case ("create-password") {
          <div class="flow-head">
            <h2 class="flow-title">Choose a password</h2>
            <p class="flow-sub text-secondary">
              Your password never leaves this Mac — it unlocks your account key
              on-device.
            </p>
          </div>
          <label class="field">
            <span class="flow-label">Password</span>
            <input
              #passwordField
              type="password"
              class="field-input"
              [formControl]="passwordControl"
              (keydown.enter)="createAccount()"
              placeholder="At least 8 characters"
              autocomplete="new-password"
            />
          </label>
          <label class="field">
            <span class="flow-label">Confirm password</span>
            <input
              type="password"
              class="field-input"
              [formControl]="confirmControl"
              (keydown.enter)="createAccount()"
              placeholder="Re-enter your password"
              autocomplete="new-password"
            />
          </label>

          <label class="toggle-row">
            <span class="toggle-copy">
              <span class="toggle-title">Save a recovery phrase</span>
              <span class="text-secondary toggle-sub">
                Generates a 24-word recovery phrase — the only way to recover a
                forgotten password. You can skip this.
              </span>
            </span>
            <input
              type="checkbox"
              [checked]="saveRecovery()"
              (change)="onSaveRecovery($event)"
              [disabled]="busy()"
            />
          </label>

          @if (error(); as err) {
            <p class="flow-error text-danger" role="alert">{{ err }}</p>
          }

          <div class="flow-nav">
            <button
              type="button"
              class="btn btn-ghost"
              (click)="goCode()"
              [disabled]="busy()"
            >
              Back
            </button>
            <span class="flow-nav-spacer"></span>
            <button
              type="button"
              class="btn btn-primary"
              (click)="createAccount()"
              [disabled]="busy()"
            >
              {{ busy() ? "Creating…" : "Create account" }}
            </button>
          </div>
        }

        <!-- ─────────────────────────── CREATE — RECOVERY ───────────────── -->
        @case ("create-recovery") {
          <div class="flow-head">
            <h2 class="flow-title">Save your recovery phrase</h2>
            <p class="flow-sub text-secondary">
              These 24 words are the ONLY way to recover a forgotten password.
              Write them down and keep them somewhere safe — they're shown once.
            </p>
          </div>
          @if (recoveryPhrase(); as phrase) {
            <p class="recovery-block" role="text">{{ phrase }}</p>
            <div class="flow-nav">
              <button type="button" class="btn" (click)="copyRecovery(phrase)">
                {{ recoveryCopied() ? "Copied" : "Copy" }}
              </button>
              <span class="flow-nav-spacer"></span>
              <button
                type="button"
                class="btn btn-primary"
                (click)="recoveryDone()"
              >
                I've saved it — continue
              </button>
            </div>
          }
        }

        <!-- ─────────────────────────── SIGN IN ─────────────────────────── -->
        @case ("signin") {
          <div class="flow-head">
            <h2 class="flow-title">Sign in</h2>
            <p class="flow-sub text-secondary">
              Sign in to load your account key on this Mac so you can share.
            </p>
          </div>
          <label class="field">
            <span class="flow-label">Email</span>
            <input
              #signinEmailField
              type="email"
              class="field-input"
              [value]="email()"
              (input)="onEmail($event)"
              (keydown.enter)="signIn()"
              placeholder="you@example.com"
              autocomplete="username"
              spellcheck="false"
              [disabled]="busy()"
            />
          </label>
          <label class="field">
            <span class="flow-label">Password</span>
            <input
              type="password"
              class="field-input"
              [formControl]="signinPwControl"
              (keydown.enter)="signIn()"
              placeholder="Password"
              autocomplete="current-password"
            />
          </label>

          @if (error(); as err) {
            <p class="flow-error text-danger" role="alert">{{ err }}</p>
          }

          <div class="flow-nav">
            <button
              type="button"
              class="btn btn-ghost"
              (click)="backToChoose()"
              [disabled]="busy()"
            >
              Back
            </button>
            <span class="flow-nav-spacer"></span>
            <button
              type="button"
              class="btn btn-primary"
              (click)="signIn()"
              [disabled]="busy()"
            >
              {{ busy() ? "Signing in…" : "Sign in" }}
            </button>
          </div>
        }

        <!-- ─────────────────────────── DONE ────────────────────────────── -->
        @case ("done") {
          <div class="flow-done">
            <span class="done-mark" aria-hidden="true">
              <svg viewBox="0 0 24 24" width="26" height="26">
                <path
                  d="M5 12.5 10 17.5 19.5 7"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="2.4"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                />
              </svg>
            </span>
            <h2 class="flow-title">You're signed in</h2>
            <p class="flow-sub text-secondary">Your account is ready to share.</p>
          </div>
        }
      }

      <!-- Step progress (create sub-flow only). -->
      @if (createProgress(); as prog) {
        <div class="flow-progress" role="group" aria-label="Account setup progress">
          <div class="dots">
            @for (i of progressDots(); track i) {
              <span
                class="dot"
                [class.is-done]="i < prog.index - 1"
                [class.is-active]="i === prog.index - 1"
                aria-hidden="true"
              ></span>
            }
          </div>
          <span class="step-count">Step {{ prog.index }} of {{ prog.total }}</span>
        </div>
      }
    </div>
  `,
  styles: [
    `
      /* Surface-agnostic: the host paints NO background of its own — the gateway
         card / settings modal that hosts this owns the surface (trap T3). */
      :host {
        display: contents;
      }
      .flow {
        display: flex;
        flex-direction: column;
        gap: var(--space-4);
      }

      .flow-head {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .flow-title {
        margin: 0;
        font-size: 1.35rem;
        font-weight: 650;
        letter-spacing: -0.02em;
      }
      .flow-sub {
        margin: 0;
        font-size: 0.95rem;
        line-height: 1.55;
      }
      .flow-email {
        color: var(--text-primary);
        font-weight: 600;
        overflow-wrap: anywhere;
      }
      .flow-note {
        margin: 0;
        font-size: 0.85rem;
        line-height: 1.55;
      }
      .flow-error {
        margin: 0;
        font-size: 0.875rem;
        line-height: 1.5;
      }

      /* Two big choice doors on the 'choose' step. */
      .doors {
        display: grid;
        grid-template-columns: 1fr 1fr;
        gap: var(--space-3);
      }
      @media (max-width: 520px) {
        .doors {
          grid-template-columns: 1fr;
        }
      }
      .door {
        display: flex;
        flex-direction: column;
        gap: var(--space-1);
        text-align: left;
        padding: var(--space-4);
        border: 1px solid var(--glass-border);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        color: var(--text-primary);
        font-family: inherit;
        cursor: pointer;
        transition:
          border-color var(--transition),
          background var(--transition),
          transform var(--transition-fast);
      }
      .door:hover {
        border-color: var(--border-strong);
        background: var(--surface-hover);
      }
      .door:active {
        transform: translateY(1px);
      }
      .door:focus-visible {
        outline: none;
        box-shadow: 0 0 0 3px var(--accent-ring);
      }
      .door:disabled {
        opacity: 0.55;
        cursor: default;
      }
      .door-icon {
        display: inline-flex;
        color: var(--accent-hover);
        margin-bottom: var(--space-1);
      }
      .door-name {
        font-size: 0.95rem;
        font-weight: 600;
        letter-spacing: -0.01em;
      }
      .door-sub {
        font-size: 0.8125rem;
        line-height: 1.45;
      }

      /* Fields. */
      .field {
        display: flex;
        flex-direction: column;
        gap: var(--space-2);
      }
      .flow-label {
        color: var(--text-secondary);
        font-size: 0.85rem;
        font-weight: 550;
      }
      .field-input {
        height: 38px;
        padding: 0 var(--space-3);
        border: 1px solid var(--border);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        color: var(--text-primary);
        font: inherit;
        font-size: 0.9rem;
      }
      .field-input::placeholder {
        color: var(--text-muted);
      }
      .field-input:focus-visible {
        outline: none;
        border-color: var(--accent-hover);
        box-shadow: 0 0 0 3px var(--accent-soft);
      }
      .code-input {
        font-family: var(--font-mono);
        letter-spacing: 0.35em;
        font-size: 1.05rem;
      }

      .link-btn {
        padding: 0;
        border: none;
        background: none;
        font: inherit;
        font-size: 0.8125rem;
        font-weight: 550;
        cursor: pointer;
        text-align: left;
      }
      .link-btn:hover {
        color: var(--text-secondary);
      }
      .link-btn:focus-visible {
        outline: none;
        text-decoration: underline;
      }
      .link-btn:disabled {
        opacity: 0.55;
        cursor: default;
      }

      /* Recovery-phrase toggle (mirrors the retired section's toggle rows). */
      .toggle-row {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-4);
        cursor: pointer;
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

      .recovery-block {
        margin: 0;
        padding: var(--space-4);
        border-radius: var(--radius-md);
        background: var(--surface-input);
        border: 1px solid var(--glass-border);
        color: var(--text-primary);
        font-family: var(--font-mono);
        font-size: 0.9rem;
        line-height: 1.8;
        user-select: text;
        -webkit-user-select: text;
        overflow-wrap: anywhere;
      }

      /* Footer nav. */
      .flow-nav {
        display: flex;
        align-items: center;
        gap: var(--space-2);
        flex-wrap: wrap;
      }
      .flow-nav-spacer {
        flex: 1;
      }
      .resend {
        font-size: 0.85rem;
      }

      /* Done. */
      .flow-done {
        display: flex;
        flex-direction: column;
        align-items: center;
        text-align: center;
        gap: var(--space-2);
        padding: var(--space-4) 0;
      }
      .done-mark {
        display: inline-flex;
        align-items: center;
        justify-content: center;
        width: 56px;
        height: 56px;
        margin-bottom: var(--space-1);
        border-radius: 50%;
        background: var(--success-soft);
        color: var(--success);
      }

      /* Step progress dots. */
      .flow-progress {
        display: flex;
        align-items: center;
        justify-content: space-between;
        gap: var(--space-4);
        padding-top: var(--space-1);
      }
      .dots {
        display: flex;
        align-items: center;
        gap: var(--space-2);
      }
      .dot {
        width: 7px;
        height: 7px;
        border-radius: var(--radius-pill);
        background: var(--border-strong);
        transition:
          width var(--transition),
          background var(--transition);
      }
      .dot.is-done {
        background: var(--accent);
      }
      .dot.is-active {
        width: 22px;
        background: var(--accent-gradient);
      }
      .step-count {
        color: var(--text-muted);
        font-family: var(--font-mono);
        font-size: 0.72rem;
        font-variant-numeric: tabular-nums;
      }
    `,
  ],
})
export class SharingAuthFlowComponent {
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);

  /** Fired once the session is logged in (after sign-in, or signup → auto-login). */
  readonly completed = output<AccountStatus>();
  /** Fired when the user backs all the way out of the 'choose' step. */
  readonly dismissed = output<void>();

  // ── Non-secret step state (signals) ──────────────────────────────────────
  readonly step = signal<Step>("choose");
  /** Shared across create + sign-in. */
  readonly email = signal("");
  readonly code = signal("");
  readonly saveRecovery = signal(false);
  /** Debounces every IPC leg. */
  readonly busy = signal(false);
  readonly error = signal<string | null>(null);
  /** A neutral "code sent" notice (never reveals whether the email exists). */
  readonly notice = signal<string | null>(null);
  /** Gates advance from create-email (a code was requested at least once). */
  readonly codeSent = signal(false);
  /** The 24-word phrase, shown once on create-recovery when saveRecovery is on. */
  readonly recoveryPhrase = signal<string | null>(null);
  readonly recoveryCopied = signal(false);

  // ── Secrets: FormControls ONLY, read at submit + cleared right after ──────
  readonly passwordControl = new FormControl("", { nonNullable: true });
  readonly confirmControl = new FormControl("", { nonNullable: true });
  readonly signinPwControl = new FormControl("", { nonNullable: true });

  /** The AccountStatus captured at auto-login, emitted after the recovery step. */
  private capturedStatus: AccountStatus | null = null;

  // ── Per-step focus targets ───────────────────────────────────────────────
  private readonly emailField =
    viewChild<ElementRef<HTMLInputElement>>("emailField");
  private readonly codeField =
    viewChild<ElementRef<HTMLInputElement>>("codeField");
  private readonly passwordField =
    viewChild<ElementRef<HTMLInputElement>>("passwordField");
  private readonly signinEmailField =
    viewChild<ElementRef<HTMLInputElement>>("signinEmailField");

  /** Create sub-flow progress (null on choose/signin/done). */
  readonly createProgress = computed<{ index: number; total: number } | null>(
    () => {
      const order: Step[] = [
        "create-email",
        "create-code",
        "create-password",
        "create-recovery",
      ];
      const idx = order.indexOf(this.step());
      if (idx < 0) {
        return null;
      }
      return { index: idx + 1, total: this.saveRecovery() ? 4 : 3 };
    },
  );

  /** The dot indices to render for the current create-progress total. */
  readonly progressDots = computed<number[]>(() => {
    const prog = this.createProgress();
    if (!prog) {
      return [];
    }
    return Array.from({ length: prog.total }, (_, i) => i);
  });

  // ── Input handlers (non-secret signals) ──────────────────────────────────
  onEmail(event: Event): void {
    this.email.set((event.target as HTMLInputElement).value);
  }

  onCode(event: Event): void {
    const digits = (event.target as HTMLInputElement).value
      .replace(/\D/g, "")
      .slice(0, 6);
    this.code.set(digits);
  }

  onSaveRecovery(event: Event): void {
    this.saveRecovery.set((event.target as HTMLInputElement).checked);
  }

  // ── Navigation ───────────────────────────────────────────────────────────
  startCreate(): void {
    this.error.set(null);
    this.notice.set(null);
    this.goto("create-email");
  }

  startSignin(): void {
    this.error.set(null);
    this.goto("signin");
  }

  backToChoose(): void {
    this.error.set(null);
    this.goto("choose");
  }

  goEmail(): void {
    this.error.set(null);
    this.goto("create-email");
  }

  goCode(): void {
    this.error.set(null);
    this.goto("create-code");
  }

  cancel(): void {
    this.dismissed.emit();
  }

  private goto(step: Step): void {
    this.step.set(step);
    // Focus this step's primary field AFTER it renders — the sanctioned zoneless
    // pattern (rule §5). Called from click handlers (outside the field-init
    // injection context), so pass the injector.
    afterNextRender(
      () => {
        switch (step) {
          case "create-email":
            this.emailField()?.nativeElement.focus();
            break;
          case "create-code":
            this.codeField()?.nativeElement.focus();
            break;
          case "create-password":
            this.passwordField()?.nativeElement.focus();
            break;
          case "signin":
            this.signinEmailField()?.nativeElement.focus();
            break;
          default:
            break;
        }
      },
      { injector: this.injector },
    );
  }

  // ── create-email [Send code] ─────────────────────────────────────────────
  async sendCode(): Promise<void> {
    if (this.busy()) {
      return;
    }
    const email = this.email().trim();
    if (!email) {
      this.error.set("Enter your email.");
      return;
    }
    this.error.set(null);
    this.notice.set(null);
    this.busy.set(true);
    try {
      await this.ipc.accountSendCode(email);
      this.codeSent.set(true);
      // The server always 202s (anti-enumeration), so this notice is honest and
      // privacy-preserving — never a "user exists" signal.
      this.notice.set(`If ${email} is valid, a 6-digit code is on its way.`);
      this.goto("create-code");
    } catch (e) {
      this.error.set(this.friendly(e));
    } finally {
      this.busy.set(false);
    }
  }

  /** Re-send the code without advancing (stays on create-code). */
  async resendCode(): Promise<void> {
    if (this.busy()) {
      return;
    }
    const email = this.email().trim();
    if (!email) {
      this.error.set("Enter your email.");
      return;
    }
    this.error.set(null);
    this.busy.set(true);
    try {
      await this.ipc.accountSendCode(email);
      this.notice.set("Code re-sent — check your inbox.");
    } catch (e) {
      this.error.set(this.friendly(e));
    } finally {
      this.busy.set(false);
    }
  }

  // ── create-code [Continue] (no server call — verified inside account_signup) ─
  codeContinue(): void {
    if (this.code().trim().length === 0) {
      this.error.set("Enter the 6-digit code from your email.");
      return;
    }
    this.error.set(null);
    this.goto("create-password");
  }

  // ── create-password [Create account] → auto-login chain ──────────────────
  async createAccount(): Promise<void> {
    if (this.busy()) {
      return;
    }
    const password = this.passwordControl.value;
    const confirm = this.confirmControl.value;
    if (password.length < 8) {
      this.error.set("Password must be at least 8 characters.");
      return;
    }
    if (password !== confirm) {
      this.error.set("Passwords do not match.");
      return;
    }
    this.error.set(null);
    this.busy.set(true);
    try {
      const phrase = await this.ipc.accountSignup(
        this.email().trim(),
        this.code().trim(),
        password,
        this.saveRecovery(),
      );
      // account_signup opens NO session — chain a login so the user ends up
      // signed in in one pass.
      const status = await this.ipc.accountLogin(this.email().trim(), password);
      // Clear the secrets immediately after the successful round-trip.
      this.passwordControl.setValue("");
      this.confirmControl.setValue("");
      this.capturedStatus = status;
      if (phrase) {
        this.recoveryPhrase.set(phrase);
        this.goto("create-recovery");
      } else {
        this.goto("done");
        this.completed.emit(status);
      }
    } catch (e) {
      // Bad/expired code, weak password, or email taken — surface it and let the
      // user step Back to the code to correct it.
      this.error.set(this.friendly(e));
    } finally {
      this.busy.set(false);
    }
  }

  // ── create-recovery [I've saved it] ──────────────────────────────────────
  recoveryDone(): void {
    this.goto("done");
    if (this.capturedStatus) {
      this.completed.emit(this.capturedStatus);
    }
  }

  async copyRecovery(phrase: string): Promise<void> {
    try {
      await navigator.clipboard.writeText(phrase);
      this.recoveryCopied.set(true);
    } catch {
      // Clipboard unavailable — the phrase stays visible and selectable.
    }
  }

  // ── signin [Sign in] ─────────────────────────────────────────────────────
  async signIn(): Promise<void> {
    if (this.busy()) {
      return;
    }
    const email = this.email().trim();
    const password = this.signinPwControl.value;
    if (!email || !password) {
      this.error.set("Enter your email and password.");
      return;
    }
    this.error.set(null);
    this.busy.set(true);
    try {
      const status = await this.ipc.accountLogin(email, password);
      this.signinPwControl.setValue("");
      this.goto("done");
      this.completed.emit(status);
    } catch (e) {
      this.error.set(this.friendly(e));
    } finally {
      this.busy.set(false);
    }
  }

  /** Map a raw backend error to a friendly, non-crashy inline message. */
  private friendly(e: unknown): string {
    const raw = String(e);
    // Only a genuine connectivity problem gets the "can't reach" guidance. A 4xx
    // (wrong or expired code, too many tries, bad password) is NOT unreachability — it arrives as a
    // clear sentence we surface as-is (below).
    if (/could not reach|unreachable|no sharing server|failed to build|network|timed? ?out/i.test(raw)) {
      return "Can't reach the sharing server. Check your connection, then try again.";
    }
    // AppError serializes as "invalid argument: <msg>" / "authentication error: <msg>" etc. — strip
    // the variant prefix so the user reads the clean message.
    return raw.replace(
      /^(invalid argument|authentication error|provider unavailable|config error|storage error|secrets error|locked): /i,
      "",
    );
  }
}
