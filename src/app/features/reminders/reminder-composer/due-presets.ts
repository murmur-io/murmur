/**
 * Due-time presets for the reminder composer.
 *
 * A pure function of `now` so it is table-testable against a frozen clock — the
 * composer never reads the wall clock itself.
 *
 * The arithmetic follows Fastmail's published snooze spec, which is the
 * best-documented version of this pattern:
 *   - "Later today" is three hours from the TOP of the current hour, so 10:30
 *     resolves to 13:00 and never 13:30. Round times read as intentional;
 *     13:37 reads as a bug.
 *   - It is HIDDEN once it would land at or after 18:00 rather than silently
 *     rolling into tomorrow. A preset that quietly becomes a different day is
 *     the classic wrong-time bug; three chips in the evening is honest.
 *
 * 09:00 is not invented here: `reminder_audit.rs::first_valid_due_at` already
 * pins every extracted Smart suggestion to 09:00 local. The manual composer
 * used to contradict its own backend by opening on now+1h, which is where the
 * absurd 00:09-style defaults came from.
 */

export type PresetId = "later-today" | "tomorrow" | "weekend" | "next-week";

export interface DuePreset {
  readonly id: PresetId;
  readonly labelKey: PresetId;
  /** Epoch ms, local. */
  readonly at: number;
  /** Hidden presets keep their slot in the array so tests can assert on them. */
  readonly hidden: boolean;
}

const HOUR_MS = 60 * 60 * 1000;
const DEFAULT_HOUR = 9;
/** Local hour at or past which "Later today" stops being offered. */
const EVENING_CUTOFF_HOUR = 18;

function atLocalTime(base: Date, hour: number, dayOffset = 0): number {
  const value = new Date(
    base.getFullYear(),
    base.getMonth(),
    base.getDate() + dayOffset,
    hour,
    0,
    0,
    0,
  );
  return value.getTime();
}

/** Days to add to reach the coming Saturday; today-is-Saturday returns 7. */
function daysUntilSaturday(day: number): number {
  const delta = (6 - day + 7) % 7;
  return delta === 0 ? 7 : delta;
}

/** Days to add to reach the next Monday; today-is-Monday returns 7. */
function daysUntilNextMonday(day: number): number {
  const delta = (1 - day + 7) % 7;
  return delta === 0 ? 7 : delta;
}

export function resolvePresets(now: Date): DuePreset[] {
  // Top of the current hour, then +3h. Derived from a fresh local Date so a DST
  // transition inside the window is resolved by the platform, not by us adding
  // raw milliseconds across the discontinuity.
  const topOfHour = new Date(
    now.getFullYear(),
    now.getMonth(),
    now.getDate(),
    now.getHours(),
    0,
    0,
    0,
  );
  const laterToday = topOfHour.getTime() + 3 * HOUR_MS;
  const laterTodayDate = new Date(laterToday);
  // Hidden when it lands in the evening OR when +3h has crossed into another
  // day — "Later today" must never silently mean tomorrow.
  const laterTodayHidden =
    laterTodayDate.getHours() >= EVENING_CUTOFF_HOUR ||
    laterTodayDate.getDate() !== now.getDate();

  const day = now.getDay();

  return [
    {
      id: "later-today",
      labelKey: "later-today",
      at: laterToday,
      hidden: laterTodayHidden,
    },
    {
      id: "tomorrow",
      labelKey: "tomorrow",
      at: atLocalTime(now, DEFAULT_HOUR, 1),
      hidden: false,
    },
    {
      id: "weekend",
      labelKey: "weekend",
      at: atLocalTime(now, DEFAULT_HOUR, daysUntilSaturday(day)),
      hidden: false,
    },
    {
      id: "next-week",
      labelKey: "next-week",
      at: atLocalTime(now, DEFAULT_HOUR, daysUntilNextMonday(day)),
      hidden: false,
    },
  ];
}

/** The epoch the composer opens on: the first preset still on offer. */
export function defaultDueAt(now: Date): number {
  const visible = resolvePresets(now).filter((preset) => !preset.hidden);
  return visible[0].at;
}
