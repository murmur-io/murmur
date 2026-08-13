import {
  ChangeDetectionStrategy,
  Component,
  inject,
  signal,
} from "@angular/core";
import { AccountSessionService } from "../../../services/account-session.service";
import { SharingAuthFlowComponent } from "../sharing-auth-flow/sharing-auth-flow.component";

type AuthDoor = "signin" | "create";

/** Global, dismissible-for-this-run status for the optional sharing account. */
@Component({
  selector: "app-account-session-banner",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [SharingAuthFlowComponent],
  templateUrl: "./account-session-banner.component.html",
  styleUrl: "./account-session-banner.component.scss",
  host: { "(document:keydown.escape)": "closeAuth()" },
})
export class AccountSessionBannerComponent {
  readonly session = inject(AccountSessionService);
  readonly authDoor = signal<AuthDoor | null>(null);

  openAuth(door: AuthDoor): void {
    this.authDoor.set(door);
  }

  closeAuth(): void {
    this.authDoor.set(null);
  }

  onCompleted(): void {
    this.closeAuth();
  }

  unlock(): void {
    void this.session.unlockWithTouchId();
  }

  onScrim(event: MouseEvent): void {
    if (event.target === event.currentTarget) {
      this.closeAuth();
    }
  }
}
