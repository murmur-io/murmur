import { Injectable, signal } from "@angular/core";
import type { AppLog, AppLogSession } from "../../core/models";

/**
 * Root-persisted backing signals for {@link LogsComponent} — the list-view rule
 * (angular-zoneless §8) applied to the log: `/developer/logs` is destroyed and
 * recreated on every navigate-away-and-back, so a component-local
 * `signal<AppLog | null>(null)` would blank the view to a spinner every return.
 * A root instance outlives the component, so the last-known entries render
 * instantly while the (still unconditional) refetch replaces them underneath.
 *
 * A thin signal holder by design: {@link LogsComponent} keeps owning the
 * orchestration (fetch, auto-refresh cadence, filtering), it just reads and
 * writes THESE signals instead of component-local ones.
 */
@Injectable({ providedIn: "root" })
export class LogsStore {
  /** The last window fetched, or `null` before the first successful read. */
  readonly log = signal<AppLog | null>(null);

  /** Which generation is being shown. Survives leaving the view. */
  readonly session = signal<AppLogSession>("current");

  /** A read is in flight. Never used alone to hide already-cached rows. */
  readonly loading = signal(false);

  /** Last read failure, or `null`. A MISSING log is not a failure. */
  readonly error = signal<string | null>(null);
}
