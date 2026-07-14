import {
  coerceForKind,
  formatForYaml,
  type PropertyKind,
  type PropertyValue,
} from "./property-field-types";

/**
 * Unit tests for the pure typed-property bridge (Feature C). These assert the
 * `coerceForKind` → `formatForYaml` round-trip that keeps the underlying
 * `properties` Record<string, string> (and therefore `serializeDoc`'s byte-exact
 * YAML) unchanged — the critical invariant of this feature.
 */
describe("property-field-types", () => {
  describe("coerceForKind", () => {
    it("text keeps the raw string verbatim", () => {
      expect(coerceForKind("hello world", "text")).toEqual({
        kind: "text",
        value: "hello world",
      });
    });

    it("select keeps the value verbatim (in-options)", () => {
      expect(coerceForKind("In progress", "select")).toEqual({
        kind: "select",
        value: "In progress",
      });
    });

    it("select preserves an out-of-options value (passthrough)", () => {
      expect(coerceForKind("Some legacy status", "select")).toEqual({
        kind: "select",
        value: "Some legacy status",
      });
    });

    it("date keeps a YYYY-MM-DD string", () => {
      expect(coerceForKind("2026-07-14", "date")).toEqual({
        kind: "date",
        value: "2026-07-14",
      });
    });

    it("checkbox reads truthy spellings as true", () => {
      for (const raw of ["true", "TRUE", "yes", "on", "1", "checked", "x"]) {
        expect(coerceForKind(raw, "checkbox")).toEqual({
          kind: "checkbox",
          value: true,
        });
      }
    });

    it("checkbox reads anything else as false", () => {
      for (const raw of ["false", "no", "0", "", "nope"]) {
        expect(coerceForKind(raw, "checkbox")).toEqual({
          kind: "checkbox",
          value: false,
        });
      }
    });

    it("number parses a numeric string", () => {
      expect(coerceForKind("42", "number")).toEqual({
        kind: "number",
        value: 42,
      });
      expect(coerceForKind("3.14", "number")).toEqual({
        kind: "number",
        value: 3.14,
      });
    });

    it("number falls back to 0 for a non-numeric string", () => {
      expect(coerceForKind("abc", "number")).toEqual({
        kind: "number",
        value: 0,
      });
      expect(coerceForKind("", "number")).toEqual({ kind: "number", value: 0 });
    });
  });

  describe("formatForYaml", () => {
    it("checkbox → true/false", () => {
      expect(formatForYaml({ kind: "checkbox", value: true })).toBe("true");
      expect(formatForYaml({ kind: "checkbox", value: false })).toBe("false");
    });

    it("number → its string form", () => {
      expect(formatForYaml({ kind: "number", value: 42 })).toBe("42");
    });

    it("select → verbatim", () => {
      expect(formatForYaml({ kind: "select", value: "Done" })).toBe("Done");
    });

    it("text/date → as-is", () => {
      expect(formatForYaml({ kind: "text", value: "hi" })).toBe("hi");
      expect(formatForYaml({ kind: "date", value: "2026-07-14" })).toBe(
        "2026-07-14",
      );
    });
  });

  describe("round-trip: formatForYaml(coerceForKind(raw, kind))", () => {
    const cases: { raw: string; kind: PropertyKind; expected: string }[] = [
      { raw: "hello", kind: "text", expected: "hello" },
      { raw: "In progress", kind: "select", expected: "In progress" },
      { raw: "Legacy value", kind: "select", expected: "Legacy value" },
      { raw: "2026-07-14", kind: "date", expected: "2026-07-14" },
      { raw: "true", kind: "checkbox", expected: "true" },
      { raw: "no", kind: "checkbox", expected: "false" },
      { raw: "42", kind: "number", expected: "42" },
      { raw: "oops", kind: "number", expected: "0" },
    ];

    for (const { raw, kind, expected } of cases) {
      it(`${kind} "${raw}" → "${expected}"`, () => {
        const value: PropertyValue = coerceForKind(raw, kind);
        expect(formatForYaml(value)).toBe(expected);
      });
    }
  });
});
