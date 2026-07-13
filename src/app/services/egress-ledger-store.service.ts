import { Injectable, signal } from "@angular/core";
import type { EgressLedger } from "../core/models";

/** Selectable rolling-window lengths (in days) — mirrors the component's own `RANGES`. */
type Range = 7 | 30 | 90;

/**
 * Root-persisted backing signals for {@link EgressLedgerComponent} — split
 * out from the component itself so the DATA survives a destroy+recreate
 * (leaving `/analytics` for another tab, then coming back). `/analytics` is
 * NOT covered by `TabRouteReuseStrategy` (only `meeting/:id` / `notes/:id` /
 * `org-item/:id` are — see its doc), so this component — nested inside
 * `AnalyticsComponent` — is genuinely destroyed and recreated on every
 * navigate-away and back; component-local `signal<EgressLedger | null>(null)`
 * + `signal<Range>(30)` would wipe to their initial values every time,
 * forcing a "Loading…" flash and resetting the 7/30/90 toggle back to 30d. A
 * root service instance outlives the component, so the ledger (and the
 * user's chosen window) render with the LAST-KNOWN state INSTANTLY on
 * return while the component's existing load effect (unchanged — still a
 * real refetch every visit) quietly replaces the ledger underneath.
 *
 * Deliberately a thin signal holder, NOT a service with its own load()
 * method: `EgressLedgerComponent` keeps owning the fetch (its `_load`
 * effect) — it just reads/writes THESE signals instead of component-local
 * ones. Mirrors `MeetingsListStore` (see `angular-zoneless.md` §8).
 */
@Injectable({ providedIn: "root" })
export class EgressLedgerStore {
  readonly days = signal<Range>(30);
  readonly ledger = signal<EgressLedger | null>(null);
  readonly loading = signal(true);
  readonly error = signal<string | null>(null);
}
