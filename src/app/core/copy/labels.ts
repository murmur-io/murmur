/**
 * The ONE place the frontend names a thing the backend also names.
 *
 * Every label here has a Rust counterpart. A label that lives in two places drifts; a label that
 * lives in three (as `CONNECTION_LABELS` did — `settings.store.ts`, `record.component.ts` and
 * `summarize/roles.rs`) drifts twice and nobody notices until two screens disagree about the same
 * connection.
 */

/**
 * Connection id → user-facing name.
 *
 * **DOCUMENTED MIRROR of `src-tauri/src/summarize/roles.rs::connection_display_name`.** They change
 * in the SAME commit or they drift — the Rust half is pinned by
 * `roles::tests::connection_labels_mirror_the_frontend_copy_module`.
 *
 * These are BRAND names, deliberately kept: "Claude Code", "Codex", "Anthropic API", "Ollama" and
 * "Kong AI Gateway" are what the user picked in Settings and what they will search for. Replacing a brand
 * with a category ("your AI service") would be vaguer, not plainer.
 */
export const CONNECTION_LABELS: Readonly<Record<string, string>> = {
  claude_code: "Claude Code",
  codex_cli: "Codex",
  anthropic: "Anthropic API",
  ollama: "Ollama",
  gateway: "Kong AI Gateway",
};

/** The connection's name, or a neutral stand-in when nothing is configured yet. */
export function connectionLabel(id: string | null | undefined): string {
  if (!id) {
    return "This provider";
  }
  return CONNECTION_LABELS[id] ?? id;
}

/**
 * Where a cloud-classified connection actually sends the redacted transcript.
 *
 * Only ever rendered after the backend's fail-closed classification refused, so the connection is
 * cloud BY DEFINITION here — for `ollama` that means its server address is not on this Mac, hence
 * "your remote Ollama server" without re-parsing anything in the frontend.
 */
export function cloudDestinationLabel(id: string | null | undefined): string {
  switch (id) {
    case "anthropic":
    case "claude_code":
      return "Anthropic's cloud";
    case "codex_cli":
      return "OpenAI's cloud";
    case "gateway":
      return "your Kong AI Gateway";
    case "ollama":
      return "your remote Ollama server";
    default:
      return "your provider's cloud";
  }
}

/**
 * Which part of a meeting a search hit came from.
 *
 * Only three values are REACHABLE from the meeting search the library and quick-search call
 * (`search_meetings` → `db::search_visible` → `search_snippet`, which returns exactly
 * `"title" | "transcript" | "note"`). The `semantic`/`entity`/`topic`/`temporal` values exist on
 * other search functions whose callers never render a hit list, so they are forward-looking only —
 * an unknown value renders nothing rather than a lie about where the match was.
 */
export function matchedInLabel(matchedIn: string | null | undefined): string {
  switch (matchedIn) {
    case "title":
      return "in the title";
    case "transcript":
      return "in the transcript";
    case "note":
      return "in the note";
    default:
      return "";
  }
}
