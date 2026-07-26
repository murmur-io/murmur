/**
 * Design System — the ONE meeting-status → `.pill` vocabulary.
 *
 * This mapping used to be copy-pasted in FOUR components (library,
 * meetings-table-view, detail, analytics) and every copy painted `RECORDING`
 * with `is-danger` — an in-progress recording rendered exactly like a failure.
 * In-progress is now `is-live` (the warm "recording" accent that the record
 * surface already uses; `.pill.is-live` ships in `primitives.css`), and
 * `is-danger` is reserved for `ERROR`.
 *
 * ⚠️ EXTENDING THIS: a `QUEUED` status arrives with the background-processing
 * queue (program workstream W1). Add it HERE — one switch, four call sites
 * follow — instead of re-forking the mapping per component. `QUEUED` is
 * expected to read as "waiting, not failed", i.e. NOT `is-danger`.
 *
 * Deliberately typed on `string`, not `MeetingStatus`: `detail` reads the
 * status off a DTO field typed as `string`, and an unknown value must fall
 * through to the neutral pill rather than fail to compile.
 */
export function meetingStatusPillClass(status: string): string {
  switch (status) {
    case "RECORDING":
      // In progress — NOT a failure. See the note above.
      return "is-live";
    case "ERROR":
      return "is-danger";
    case "TRANSCRIBED":
    case "SUMMARIZED":
      return "is-accent";
    case "EXPORTED":
      return "is-success";
    default:
      return "";
  }
}

/** `RECORDING` → `Recording`. The label half of the same vocabulary. */
export function meetingStatusLabel(status: string): string {
  return status.charAt(0) + status.slice(1).toLowerCase();
}
