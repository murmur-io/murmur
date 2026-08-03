/**
 * The frontend mirror of the persistence boundary's model-id rules.
 *
 * The backend stays authoritative — `dto_to_config` refuses a bad id whatever the UI does. This
 * exists so the UI never DISPLAYS a value `save_config` has already discarded, which is the field
 * telling the user something untrue.
 *
 * It lives in one file because it was written twice before and the copies disagreed: the Setup card
 * checked it, the role rows did not, so the same `-m` typed one row lower was kept on screen and
 * dropped in config.
 *
 * Mirrors, symbol for symbol:
 *   - `commands::connection_builds_argv`
 *   - `summarize::provider::{valid_model_id, valid_catalog_model_id}`
 *
 * NOTE ON ENCODING: every control character below is written as an ESCAPE (\u0000), never as the
 * byte itself. A literal NUL in the first 8000 bytes makes git classify a `.ts` file as binary, and
 * this file reached three reviewers once as `GIT binary patch / literal 4955` — a production module
 * that decides whether a stored model id is kept or cleared, delivered unreadable. The regex means
 * the same thing either way; only reviewability differs.
 */

/**
 * Connections that pass the model in a JSON body. EVERYTHING ELSE builds an argv command line.
 *
 * Written as the deny-list, not as `["claude_code", "codex_cli"]`, for the reason the backend
 * comment on `connection_builds_argv` gives: the obvious spelling fails OPEN. A CLI-backed engine
 * added later and forgotten here would inherit the loose ceiling in the UI while the backend
 * applied the strict one — the field would show an id as accepted that save then dropped, which is
 * precisely the defect this mirror exists to prevent. Spelled this way, a forgotten engine gets the
 * STRICTER rule, so drift is merely pessimistic rather than untruthful.
 */
const JSON_BODY_CONNECTION_IDS: readonly string[] = [
  "anthropic",
  "ollama",
  "gateway",
  "local",
  "off",
];

/** `provider::MODEL_ID_MAX_CHARS` — an argv `--model <id>` stays short. */
const CLI_MODEL_ID_MAX_CHARS = 64;
/** `provider::CATALOG_MODEL_ID_MAX_CHARS` — a storage bound, not a slug rule. */
const CATALOG_MODEL_ID_MAX_CHARS = 512;
/**
 * The ARGV character class: ASCII alphanumerics plus `. - _ : / @`.
 *
 * Applies to the CLI arms only. A JSON-body arm has no allowlist — mirroring
 * `provider::valid_catalog_model_id`, which dropped one because it refused real ids
 * (`vendor/model+preview`, anything non-ASCII) while preventing nothing.
 */
const ARGV_MODEL_ID_ALLOWED = /^[A-Za-z0-9._:/@-]+$/;

/**
 * Whitespace or a C0/DEL/C1 control character — never part of a real model id on any arm, and an
 * embedded newline would corrupt any log line carrying it. Mirrors `char::is_whitespace` +
 * `char::is_control` in `provider::valid_catalog_model_id` — which is true for C1
 * (U+0080-U+009F) too, not only C0 and DEL. Stopping at DEL left U+0085 accepted here and
 * refused there, the same mirror divergence as counting UTF-16 units instead of UTF-8 bytes.
 */
// eslint-disable-next-line no-control-regex
const MODEL_ID_FORBIDDEN_CHARS = /[\s\u0000-\u001f\u007f-\u009f]/;

/** Does this connection put the model on a command line? Fail-closed: unknown ⇒ yes. */
export function connectionBuildsArgv(connection: string): boolean {
  return !JSON_BODY_CONNECTION_IDS.includes(connection.trim());
}

/**
 * `""` on a role connection means INHERIT, so the model has to be judged against the connection it
 * will actually run on — not against the blank string. Mirrors `commands::effective_connection`.
 */
export function effectiveConnection(connection: string, defaultEngine: string): string {
  const value = connection.trim();
  return value === "" ? defaultEngine.trim() : value;
}

/**
 * The length the BACKEND measures: UTF-8 BYTES.
 *
 * Rust's `str::len()` counts bytes; JavaScript's `String.length` counts UTF-16 code units. While the
 * JSON arms carried an ASCII allowlist the two agreed by construction. Removing that allowlist —
 * correctly, since it refused real ids — made non-ASCII reachable and the two counts diverge:
 * 300 × `é` is 300 units here and 600 bytes there, so the field would show it accepted and
 * `dto_to_config` would discard it. Measuring the same unit is what keeps this a mirror.
 */
function utf8Length(value: string): number {
  return new TextEncoder().encode(value).length;
}

/** Shared by both arms: the refusals with no legitimate cost anywhere. */
function shapeIsSafeEverywhere(value: string, maxChars: number): boolean {
  return (
    value.length > 0 &&
    utf8Length(value) <= maxChars &&
    !value.startsWith("-") &&
    !value.split("/").some((part) => part === ".." || part === ".") &&
    !MODEL_ID_FORBIDDEN_CHARS.test(value)
  );
}

/**
 * Would the boundary keep this id for a model field bound to this connection?
 *
 * Blank is always valid — it means "let the engine pick". The two arms differ in exactly the way
 * `provider.rs` does: an argv arm adds a short-slug ceiling and a character allowlist, because the
 * id becomes `--model <id>`; a JSON-body arm adds neither, because it cannot.
 */
export function connectionKeepsModelId(id: string, connection: string): boolean {
  const value = id.trim();
  if (value === "") return true;
  if (!connectionBuildsArgv(connection)) {
    return shapeIsSafeEverywhere(value, CATALOG_MODEL_ID_MAX_CHARS);
  }
  return shapeIsSafeEverywhere(value, CLI_MODEL_ID_MAX_CHARS) && ARGV_MODEL_ID_ALLOWED.test(value);
}

/**
 * Would the boundary keep this id in `providerModel` — the DEFAULT-engine field?
 *
 * Judged by the SELECTED ENGINE, exactly like `dto_to_config` does. This was briefly unconditional
 * strict on both sides, on the belief that an explicit CLI role could inherit `provider_model` onto
 * an argv command line. It cannot — `roles::is_explicit` keys on the connection key alone — and the
 * over-broad rule refused legitimate long vendor ids under `anthropic`, a JSON-body arm.
 *
 * `defaultEngineKeepsModelId` is kept as its own name (rather than calling `connectionKeepsModelId`
 * at each site) because this field's connection is the engine, not a role connection, and the two
 * are easy to confuse.
 */
export function defaultEngineKeepsModelId(id: string, engine: string): boolean {
  return connectionKeepsModelId(id, engine);
}

/**
 * Does this connection READ a role's model id, such that carrying a stale one into it would change
 * behaviour?
 *
 * `local` DOES. `make_provider_resolved`'s local arm is
 * `if target.model.trim().is_empty() { config.brain_model_id } else { Some(target.model) }` followed
 * by `resolve_brain_model(...)?`. So the id is consumed as a REGISTRY KEY: one that matches
 * `BRAIN_MODELS` is a working per-role on-device override, and one that does not overrides
 * `brain_model_id` with nothing and fails the note with `Unavailable`. An earlier version of this
 * file claimed local ignored the model entirely, which was wrong in one direction; the fix then
 * cleared it unconditionally, which was wrong in the other. Membership decides — see
 * `SettingsStore.roleModelAfterConnectionChange`.
 *
 * `""` (inherit) does NOT: `roles::is_explicit` keys on the connection key alone, so an inheriting
 * role resolves through `legacy_default_target`, which never reads `role_*_model`.
 *
 * `off` does NOT either: `provider_for` refuses to build a provider for a reasoner-only target and
 * `reasoner_target` maps it to no reasoner, so the model is never read on any path.
 */
export function connectionConsumesRoleModel(connection: string): boolean {
  const value = connection.trim();
  return value !== "" && value !== "off";
}
