/**
 * Typed note-property helpers — a NEW, PARALLEL layer sitting BESIDE the byte-exact
 * YAML round-trip in `front-matter.ts` (Feature C). This file NEVER touches
 * `parseDoc`/`serializeDoc`: the note's `properties` signal stays a plain
 * `Record<string, string>`, and `serializeDoc` still emits it unchanged. The
 * "typing" is purely a UI concern — a folder-level schema names each property's
 * KIND (text / select / date / checkbox / number) so the editor can render the
 * right widget, and this module is the pure bridge between a widget's raw string
 * and a typed value (and back to the string the properties map + YAML store).
 *
 * Contract (fixed — the Rust backend implements the matching commands + DTOs):
 *  - `PropertyKind`        — the five supported kinds.
 *  - `PropertySchemaField` — one folder-schema entry: `{ key, kind, options }`.
 *  - `PropertyValue`       — an adjacently-tagged `{ kind, value }` typed value.
 *
 * The three DTO types live canonically in `core/models.ts` (with every other IPC
 * DTO); they are RE-EXPORTED here so a consumer of the helpers imports the type +
 * its coercion from one place. The round-trip that matters:
 * `formatForYaml(coerceForKind(raw, kind))` maps a widget's raw string to the
 * canonical string stored in the `properties` map (and therefore the YAML). A
 * `select` value that is NOT one of the schema's options is preserved VERBATIM
 * (passthrough) so an out-of-schema value a note already carried is never dropped.
 */

import type {
  PropertyKind,
  PropertyValue,
} from "../../../core/models";

export type {
  PropertyKind,
  PropertySchemaField,
  PropertyValue,
  TypedNoteRow,
} from "../../../core/models";

/**
 * Coerce a raw front-matter STRING into a typed {@link PropertyValue} for the
 * given kind. Total + tolerant (never throws) — a value that doesn't fit its
 * kind falls back sensibly so a widget always has something to render:
 *  - `checkbox` — truthy YAML/Obsidian spellings (`true`/`yes`/`on`/`1`/`checked`,
 *    case-insensitive) ⇒ `true`; anything else ⇒ `false`.
 *  - `number`   — parsed with `Number(...)`; a non-numeric raw ⇒ `0`.
 *  - `date`     — kept as the raw string (an `<input type="date">` reads/writes
 *    `YYYY-MM-DD`; a non-conforming raw is preserved so it isn't lost).
 *  - `select`   — kept VERBATIM (passthrough — the caller decides whether it's
 *    one of the schema options; an out-of-schema value must survive).
 *  - `text`     — the raw string unchanged.
 */
export function coerceForKind(raw: string, kind: PropertyKind): PropertyValue {
  const trimmed = raw.trim();
  switch (kind) {
    case "checkbox":
      return { kind: "checkbox", value: isTruthy(trimmed) };
    case "number": {
      const n = trimmed === "" ? Number.NaN : Number(trimmed);
      return { kind: "number", value: Number.isFinite(n) ? n : 0 };
    }
    case "date":
      return { kind: "date", value: trimmed };
    case "select":
      return { kind: "select", value: raw };
    case "text":
    default:
      return { kind: "text", value: raw };
  }
}

/**
 * Collapse a typed {@link PropertyValue} back to the canonical STRING stored in
 * the `properties` map (and therefore emitted by `serializeDoc` into the YAML
 * front-matter). Inverse of {@link coerceForKind} for the round-trip:
 *  - `checkbox` — `"true"` / `"false"` (lower-case, YAML-native booleans).
 *  - `number`   — the number's `String(...)` form (`0` when non-finite).
 *  - `date`     — the date string as-is.
 *  - `select`   — the value VERBATIM (an out-of-options value is preserved).
 *  - `text`     — the string as-is.
 */
export function formatForYaml(v: PropertyValue): string {
  switch (v.kind) {
    case "checkbox":
      return v.value ? "true" : "false";
    case "number":
      return Number.isFinite(v.value) ? String(v.value) : "0";
    case "date":
      return v.value;
    case "select":
    case "text":
    default:
      return v.value;
  }
}

/** The truthy string spellings a `checkbox` recognises (case-insensitive). */
const TRUTHY = new Set(["true", "yes", "on", "1", "checked", "x", "☑", "done"]);

/** Whether a raw string reads as a checked checkbox. */
function isTruthy(raw: string): boolean {
  return TRUTHY.has(raw.toLowerCase());
}
