import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  OnInit,
  afterNextRender,
  computed,
  inject,
  output,
  input,
  signal,
  viewChild,
} from "@angular/core";
import { FormControl, ReactiveFormsModule } from "@angular/forms";
import { IpcService } from "../../../core/ipc.service";
import type { AccountStatus } from "../../../core/models";
import { ErrorCopyService } from "../../../core/copy/error-copy.service";
import { AccountSessionService } from "../../../services/account-session.service";

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
 * discipline). Account status lands directly in the root session service;
 * `completed` is deliberately payload-free so identity never crosses a
 * component-output boundary.
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
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [ReactiveFormsModule],
  templateUrl: "./sharing-auth-flow.component.html",
  styleUrl: "./sharing-auth-flow.component.scss",
})
export class SharingAuthFlowComponent implements OnInit {
  private readonly ipc = inject(IpcService);
  private readonly injector = inject(Injector);
  private readonly errorCopy = inject(ErrorCopyService);
  private readonly accountSession = inject(AccountSessionService);

  /** Payload-free completion; session identity stays inside AccountSessionService. */
  readonly completed = output<void>();
  /** Fired when the user backs all the way out of the 'choose' step. */
  readonly dismissed = output<void>();
  /** Lets a global banner open the existing flow at the requested door. */
  readonly initialDoor = input<"choose" | "signin" | "create">("choose");

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

  ngOnInit(): void {
    if (this.initialDoor() === "signin") {
      this.startSignin();
    } else if (this.initialDoor() === "create") {
      this.startCreate();
    }
  }

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
      this.accountSession.accept(status);
      // Clear the secrets immediately after the successful round-trip.
      this.passwordControl.setValue("");
      this.confirmControl.setValue("");
      this.capturedStatus = status;
      if (phrase) {
        this.recoveryPhrase.set(phrase);
        this.goto("create-recovery");
      } else {
        this.goto("done");
        this.completed.emit();
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
      this.completed.emit();
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
      this.accountSession.accept(status);
      this.signinPwControl.setValue("");
      this.goto("done");
      this.completed.emit();
    } catch (e) {
      this.error.set(this.friendly(e));
    } finally {
      this.busy.set(false);
    }
  }

  /**
   * Map a backend failure to the sentence for this form.
   *
   * This used to sniff the raw string for connectivity words and, on a miss, strip the `AppError`
   * variant prefix and render whatever was left. On a rejected sign-in code that meant the user
   * read `verify_code: rejected (400)` — the shape of failure P3 exists to delete. The four
   * `sharing-*` codes (`share/client.rs::status_err`) now carry the distinction that mattered:
   * unreachable is connectivity, 429 is rate-limiting, 4xx is a bad or expired code, 401 is a
   * signed-out session.
   */
  private friendly(e: unknown): string {
    return this.errorCopy.humanize(e);
  }
}
