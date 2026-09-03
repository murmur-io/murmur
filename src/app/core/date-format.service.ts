import { Injectable } from "@angular/core";

/**
 * The one place a date becomes a string a user reads.
 *
 * WHY (2026-09-02 audit, F1). Nine components had written their own `formatDate`, and they were not
 * copies — they disagreed on the thing that matters, which is what to show when the input is not a
 * date. Three echoed the raw value back, so a user could be shown
 * `2026-09-03T18:41:54.969Z` in the middle of a sentence; one returned an empty string, leaving a
 * label with nothing after it; two returned different placeholder wordings. Same broken data, five
 * different experiences.
 *
 * The fallback is therefore an EXPLICIT parameter rather than a hardcoded default: consolidating
 * silently would have changed behaviour at every call site at once, which is a refactor pretending
 * to be a cleanup. Each site passes what it means, and the sites that used to echo raw ISO now pass
 * readable words — an improvement stated out loud, not smuggled in.
 */
@Injectable({ providedIn: "root" })
export class DateFormatService {
  /** `12 Sep 2026` — the long-standing default across the app. */
  day(value: string | number | null | undefined, fallback = "Date unavailable"): string {
    const date = this.parse(value);
    return date
      ? date.toLocaleDateString(undefined, {
          year: "numeric",
          month: "short",
          day: "numeric",
        })
      : fallback;
  }

  /** `12 Sep 2026, 14:05` — for rows where the time of day carries meaning. */
  dayAndTime(
    value: string | number | null | undefined,
    fallback = "Date unavailable",
  ): string {
    const date = this.parse(value);
    return date
      ? date.toLocaleString(undefined, {
          year: "numeric",
          month: "short",
          day: "numeric",
          hour: "2-digit",
          minute: "2-digit",
        })
      : fallback;
  }

  /**
   * `null` for anything that is not a real instant.
   *
   * `new Date("nonsense")` is an Invalid Date rather than a throw, and it formats as the string
   * "Invalid Date" — which is how a broken timestamp reaches a user looking like a deliberate
   * label. Every caller goes through this check.
   */
  private parse(value: string | number | null | undefined): Date | null {
    if (value === null || value === undefined || value === "") return null;
    const date = new Date(value);
    return Number.isNaN(date.getTime()) ? null : date;
  }
}
