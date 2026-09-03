import { Injectable, inject, signal } from "@angular/core";
import { IpcService } from "../core/ipc.service";
import type { UpdateInfo } from "../core/models";
import { ToastService } from "./toast.service";

/** The lifecycle of an update check, surfaced to the Settings "About" section. */
export type UpdateStatus =
  | "idle"
  | "checking"
  | "upToDate"
  | "available"
  | "error";

/**
 * Central update-check logic, shared by the startup nudge (AppComponent) and the
 * manual "Check for updates" button (Settings → About). It talks to the Rust
 * `check_for_update` / `open_release_page` commands through {@link IpcService}
 * and surfaces outcomes via {@link ToastService}.
 *
 * All state lives in signals (`status` / `latest`) so the Settings pane renders
 * reactively under zoneless change detection — no plain fields for template-read
 * state. Both public methods are async and never throw: the startup path is
 * silent on failure (a background check must not nag), the manual path surfaces
 * errors (it's user-initiated).
 */
@Injectable({ providedIn: "root" })
export class UpdateService {
  private readonly ipc = inject(IpcService);
  private readonly toast = inject(ToastService);

  private readonly _status = signal<UpdateStatus>("idle");
  /** Current check lifecycle state (drives the Settings button label + result line). */
  readonly status = this._status.asReadonly();

  private readonly _latest = signal<UpdateInfo | null>(null);
  /** The most-recent successful check result (null until a check succeeds). */
  readonly latest = this._latest.asReadonly();

  /**
   * Best-effort startup check. If an update is available, push a STICKY info
   * toast with a "Download" action that opens the release page. On ANY thrown
   * error (network / rate-limit) it swallows silently — a failed background
   * check must never nag the user. Never throws.
   */
  async checkOnStartup(): Promise<void> {
    try {
      // `manual: false` — the backend refuses this outright when the user has turned automatic
      // checks off, so the decision is not the frontend's to make or to route around.
      const info = await this.ipc.checkForUpdate(false);
      this._latest.set(info);
      if (info.updateAvailable) {
        this._status.set("available");
        this.pushDownloadToast(info);
      } else {
        this._status.set("upToDate");
      }
    } catch {
      // Silent on startup: a failed background check must not surface a toast.
    }
  }

  /**
   * User-initiated check (the Settings "About" button). Sets `status` to
   * "checking", then to "upToDate" / "available" on success — showing a toast
   * for BOTH outcomes — or "error" with a danger toast on failure. Never throws.
   */
  async checkManually(): Promise<void> {
    this._status.set("checking");
    try {
      // `manual: true` — pressing the button IS the consent, so this runs whatever the flag says.
      const info = await this.ipc.checkForUpdate(true);
      this._latest.set(info);
      if (info.updateAvailable) {
        this._status.set("available");
        this.pushDownloadToast(info);
      } else {
        this._status.set("upToDate");
        this.toast.success("You're on the latest version.");
      }
    } catch {
      this._status.set("error");
      this.toast.danger("Couldn't check for updates.");
    }
  }

  /** Push the sticky "New version …" toast with a Download action. */
  private pushDownloadToast(info: UpdateInfo): void {
    this.toast.push(
      `New version ${info.latestVersion} is available.`,
      "info",
      0,
      {
        label: "Download",
        run: () => void this.ipc.openReleasePage(info.releaseUrl),
      },
    );
  }
}
