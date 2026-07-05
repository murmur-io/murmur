import {
  ChangeDetectionStrategy,
  Component,
  inject,
  signal,
} from "@angular/core";
import { Router } from "@angular/router";
import { IpcService } from "../../../core/ipc.service";
import { SharingAuthFlowComponent } from "../sharing-auth-flow/sharing-auth-flow.component";

/**
 * SharingGatewayComponent — the first-run SHARING gateway at route `/welcome`.
 *
 * A full-bleed welcome (a shell child route, so the packaged WKWebView
 * style-resolves it — trap T4: a screen must be router-mounted, never rendered
 * directly in AppComponent's static host). Shown by `app.component.ts` (and the
 * onboarding handoff) only when `!cfg.sharingChoiceMade && !accountStatus.loggedIn`.
 *
 * Two doors ARE the first-run decision — there is no silent skip:
 *   - "Use Murmur locally — no account" → `markSharingChoiceMade()` then `/record`.
 *   - "Create or sign in to a sharing account" → the reusable `<app-sharing-auth-flow>`.
 *
 * The gate NEVER traps: every IPC failure is caught and falls through to
 * `/record`, so a broken/unavailable backend can never strand the user here.
 *
 * The gateway hosts the flow inside its OWN full-bleed frosted `.card` panel
 * (this is a whole PAGE, in-flow — not a floating popover, so the frosted card
 * is correct; the OPAQUE-overlay rule T3 is for the Settings modal host).
 */
@Component({
  selector: "app-sharing-gateway",
  standalone: true,
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [SharingAuthFlowComponent],
  templateUrl: "./sharing-gateway.component.html",
  styleUrl: "./sharing-gateway.component.scss",
})
export class SharingGatewayComponent {
  private readonly ipc = inject(IpcService);
  private readonly router = inject(Router);

  /** `pick` = the two doors; `account` = the reusable multi-step flow. */
  readonly mode = signal<"pick" | "account">("pick");
  /** Debounces the local-choice button (its IPC + navigate). */
  readonly busy = signal(false);

  /** Door (a): resolve the decision as local-only, then enter the app. */
  async chooseLocal(): Promise<void> {
    if (this.busy()) {
      return;
    }
    this.busy.set(true);
    try {
      await this.ipc.markSharingChoiceMade();
    } catch {
      // Never trap the user on a persistence failure — the gate re-offers next
      // launch, which is acceptable (they made no lasting decision).
    }
    await this.router.navigate(["/record"]);
  }

  /** Door (b): open the account flow inside the gateway's card panel. */
  openAccount(): void {
    this.mode.set("account");
  }

  /** The flow logged the user in → mark the choice made, then enter the app. */
  async onCompleted(): Promise<void> {
    try {
      await this.ipc.markSharingChoiceMade();
    } catch {
      // Non-fatal — proceed into the app regardless.
    }
    await this.router.navigate(["/record"]);
  }

  /** The user backed out of the flow → return to the two doors. */
  onDismissed(): void {
    this.mode.set("pick");
  }
}
