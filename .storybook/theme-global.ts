export type ThemeGlobal = "dark" | "light" | "system";

/** Resolves the toolbar's Theme global the way the app does — `system` follows the OS. */
export function isLightTheme(theme: unknown): boolean {
  if (theme === "light") return true;
  if (theme === "system") {
    return typeof matchMedia !== "undefined" && matchMedia("(prefers-color-scheme: light)").matches;
  }
  return false;
}
