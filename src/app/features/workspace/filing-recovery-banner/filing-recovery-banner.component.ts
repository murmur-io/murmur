import {
  ChangeDetectionStrategy,
  Component,
  ElementRef,
  Injector,
  afterNextRender,
  computed,
  effect,
  inject,
  signal,
  untracked,
  viewChild,
} from "@angular/core";

import { MurBannerComponent } from "../../../design-system/banner/banner.component";
import { MurIconComponent } from "../../../design-system/icon/icon.component";
import { FilingRecoveryService } from "../../../services/filing-recovery.service";

/** Persistent, content-free recovery warning plus one-issue confirmation flow. */
@Component({
  selector: "app-filing-recovery-banner",
  changeDetection: ChangeDetectionStrategy.OnPush,
  imports: [MurBannerComponent, MurIconComponent],
  templateUrl: "./filing-recovery-banner.component.html",
  styleUrl: "./filing-recovery-banner.component.scss",
})
export class FilingRecoveryBannerComponent {
  readonly recovery = inject(FilingRecoveryService);
  private readonly injector = inject(Injector);
  private readonly confirmationToken = signal<string | null>(null);
  private readonly keepExistingButton =
    viewChild<ElementRef<HTMLButtonElement>>("keepExistingButton");
  private readonly confirmButton =
    viewChild<ElementRef<HTMLButtonElement>>("confirmButton");
  readonly confirming = computed(() => this.confirmationToken() !== null);

  /** A refreshed status invalidates the exact destructive choice under review. */
  private readonly _closeStaleConfirmation = effect(() => {
    const token = this.confirmationToken();
    const status = this.recovery.status();
    if (token && (!status?.degraded || status.issueToken !== token)) {
      untracked(() => this.confirmationToken.set(null));
    }
  });

  openConfirmation(): void {
    const status = this.recovery.status();
    if (
      status?.degraded &&
      status.canKeepExisting &&
      status.issueToken &&
      this.recovery.action() === null
    ) {
      // Capture the exact reviewed issue. A focus refresh can advance the
      // service status, but the backend treats this opaque stale token as a no-op.
      this.confirmationToken.set(status.issueToken);
      afterNextRender(() => this.confirmButton()?.nativeElement.focus(), {
        injector: this.injector,
      });
    }
  }

  closeConfirmation(): void {
    if (this.recovery.action() === null) {
      this.confirmationToken.set(null);
      afterNextRender(() => this.keepExistingButton()?.nativeElement.focus(), {
        injector: this.injector,
      });
    }
  }

  async confirmKeepExisting(): Promise<void> {
    const token = this.confirmationToken();
    if (!token) {
      return;
    }
    if (await this.recovery.keepExisting(token)) {
      this.confirmationToken.set(null);
    }
  }

  onScrimClick(event: MouseEvent): void {
    if (event.target === event.currentTarget) {
      this.closeConfirmation();
    }
  }
}
