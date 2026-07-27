/**
 * The ONE place the frontend turns a whisper-model byte figure into words.
 *
 * The figures themselves are authored in Rust (`transcribe/catalog.rs`) and reach
 * the FE through `whisper_recommendation`. Before P2 the frontend carried its own
 * hardcoded size tables in TWO places (`settings.store.ts`'s `hints` map and
 * `onboarding.component.ts`'s `SIZE_HINTS`) which had already drifted from each
 * other and from the `<option>` labels; both are gone, and this module only ever
 * FORMATS what the backend states.
 */

/** Binary units, matching how the Rust catalog states the figures. */
export function formatModelBytes(bytes: number): string {
  const mb = bytes / (1024 * 1024);
  if (mb < 1) return `${Math.max(1, Math.round(bytes / 1024))} KB`;
  if (mb < 1000) return `${Math.round(mb)} MB`;
  return `${(mb / 1024).toFixed(1)} GB`;
}

/**
 * A download size for a button/label. `null` means the backend states no figure for
 * that size, and it renders as **"size unknown"** — NEVER as "free" or as an empty
 * string that reads like nothing will be transferred. Under-disclosing a multi-GB
 * download is precisely the dishonesty this workstream removes.
 */
export function modelSizeLabel(bytes: number | null): string {
  return bytes === null ? "size unknown" : formatModelBytes(bytes);
}
