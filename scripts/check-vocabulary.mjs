#!/usr/bin/env node
/**
 * Keep developer vocabulary out of the product surface.
 *
 * WHY. A count of the Settings templates alone found `token` ×18, `Ollama` ×11,
 * `egress` ×10, `embedding` ×6, `GGUF` ×4, `base URL` ×3, `MCP` ×3. None of those
 * words means anything to the person this app is for, and one of the worst offenders
 * was a RUST string that reached a banner — so this scans both trees.
 *
 * WHAT IT IS NOT. It is not a spellchecker and it does not know whether a sentence is
 * good. It answers one narrow question: does a banned term reach a surface a user can
 * see? Everything subtler stays a human judgement.
 *
 * HONESTY CONSTRAINT, and the reason this tool is deliberately blunt: this app's
 * differentiator is telling the truth about what leaves the Mac. De-jargoning must make
 * the privacy story CLEARER, never vaguer. A rewrite that is shorter AND drops a noun
 * phrase is wrong, and no scanner can catch that — `e2e/settings/privacy-honesty.spec.ts`
 * pins those clauses by fact instead.
 *
 * TWO LISTS, deliberately separate:
 *   .vocabulary-baseline.json   transitional debt. Must only ever SHRINK. Not an excuse.
 *   .vocabulary-allowlist.json  permanent exemptions. Every entry states its reason.
 * Collapsing them into one file is how a baseline quietly becomes a rubber stamp.
 *
 * Usage:  node scripts/check-vocabulary.mjs [--strict] [--update-baseline]
 *   default  report and exit 0 (WARN mode — what CI runs today)
 *   --strict exit 1 on any hit outside the allowlist (P5 arms this)
 */
import { readFileSync, writeFileSync, existsSync } from "node:fs";
import { execFileSync } from "node:child_process";
import path from "node:path";

const ROOT = path.resolve(import.meta.dirname, "..");
const BASELINE = path.join(ROOT, ".vocabulary-baseline.json");
const ALLOWLIST = path.join(ROOT, ".vocabulary-allowlist.json");

/** Terms that must never reach a user-visible string, with the plain replacement. */
const BANNED = [
  ["egress", "what leaves your Mac"],
  ["GGUF", "(drop it — say the model's name)"],
  ["quantiz", "(drop it — say the size or speed)"],
  ["embedding", "search index"],
  ["\\btokens?\\b", "words (or drop the count)"],
  ["\\bMCP\\b", "the local connection for other apps"],
  ["base URL", "web address"],
  ["\\bDEK\\b|\\bKEK\\b|\\bCK\\b", "(never show a key name)"],
  ["sidecar", "(drop it — name what it does)"],
  ["\\bIPC\\b", "(drop it)"],
  ["SQLCipher", "encrypted database"],
  ["\\bNER\\b", "name masking"],
  ["prefix cache|KV cache", "(drop it)"],
  ["\\bRAG\\b", "search"],
  ["reranker", "result ordering"],
  // The hierarchy's CODE word. A user never made a "container" — they made a Workspace or a
  // folder — and it leaked into three of `container-view`'s own empty and error states while
  // every other surface said something else. `core/hierarchy-vocabulary.ts` is the one source.
  //
  // Deliberately NOT banning "space": it is ordinary English used correctly elsewhere ("Free up
  // space"), and banning it would train people to ignore this list. What stops "space" being used
  // as a HIERARCHY noun is the `ContainerNoun` type — a domain identifier no longer typechecks
  // where a sentence is written, which is a guard a word list cannot provide.
  // FE ONLY, for now. `core/hierarchy-vocabulary.ts` made the frontend consistent, but the
  // BACKEND still says "container" in 69 `AppError` bodies that reach the user as toasts
  // ("unlock this container before moving a dashboard into it"). Those live in files another PR
  // is currently rewriting, and silently widening the baseline to admit them would be the exact
  // rubber stamp this gate exists to prevent — so they are a recorded task, not a hidden pass.
  ["\\bcontainers?\\b", "Workspace or folder", ["fe"]],
];

/** Files whose strings a user can actually read. */
const FE_GLOBS = ["src/app/**/*.html", "src/app/**/*.ts"];
const RS_GLOBS = ["src-tauri/src/**/*.rs"];

function tracked(globs) {
  const out = execFileSync("git", ["ls-files", "--", ...globs], {
    cwd: ROOT,
    encoding: "utf8",
  });
  return out.split("\n").filter(Boolean);
}

/**
 * Strip what a user cannot see. Without this the raw counts are dominated by comments,
 * import paths and class bindings — noise that trains people to ignore the tool.
 */
function visibleText(file, source) {
  let text = source;
  text = text.replace(/\/\*[\s\S]*?\*\//g, " ");
  text = text.replace(/^\s*(\/\/|#).*$/gm, " ");
  if (file.endsWith(".ts")) {
    // Only STRING LITERALS can reach a user. Scanning raw source made identifiers the
    // dominant "hit" — `this.ipc.unlinkItems`, `token: Tokens.Image` — 774 findings of
    // which almost none were copy. A checker that noisy teaches people to ignore it,
    // which is worse than not having one.
    const literals = [];
    const re = /(['"`])((?:\\.|(?!\1)[^\\])*)\1/g;
    let m;
    // Extract from the COMMENT-STRIPPED text, not the raw source: an apostrophe inside a
    // prose comment ("doesn't") opens a bogus string literal that swallows the next few
    // hundred characters of code, and the whole comment then reads as user-facing copy.
    while ((m = re.exec(text))) {
      // An interpolated expression is CODE, not copy: in `Created a folder in ${container.name}`
      // the user reads "Created a folder in Projects" — `container` is a variable name that
      // happens to sit inside a string. Leaving these in made every hierarchy term look like it
      // leaked into 80 sentences it never reached, which is precisely the noise this function's
      // own comment warns teaches people to ignore the tool.
      const value = m[2].replace(/\$\{[^}]*\}/g, " ");
      // Prose, not an identifier / path / css class / event name.
      if (!/\s/.test(value)) continue;
      if (/^[./#@]/.test(value)) continue;
      if (!/[a-z]{3}/i.test(value)) continue;
      literals.push(value);
    }
    text = literals.join("\n");
  }
  if (file.endsWith(".html")) {
    // Attribute VALUES a user never reads: bindings, ids, classes, test hooks.
    text = text.replace(/\s(?:\[|\()?[\w.-]+(?:\)|\])?="[^"]*"/g, (m) =>
      /^\s(?:aria-label|title|placeholder|alt|matTooltip)=/.test(m) ? m : " ",
    );
    text = text.replace(/<!--[\s\S]*?-->/g, " ");
    // Angular control flow and interpolation EXPRESSIONS are code. A user reading
    // `{{ container.name }}` sees "Projects"; `@if (node(); as container)` they never see at all.
    // Left in, they made every hierarchy term look like it leaked into dozens of sentences it
    // never reached — the same false-positive class the `.ts` branch already strips for `${…}`.
    text = text.replace(/\{\{[\s\S]*?\}\}/g, " ");
    text = text.replace(/@(?:if|else if|for|switch|case|defer|placeholder|loading|empty)\s*\([^)]*\)/g, " ");
    // Element NAMES are never copy — `<app-container-share-sheet>` is a selector. The attribute
    // pass above deliberately keeps aria-label/title/placeholder/alt, so whole tags cannot simply
    // be stripped; the tag NAME can.
    text = text.replace(/<\/?[a-zA-Z][\w-]*/g, " ");
  }
  if (file.endsWith(".rs")) {
    // Only strings that can REACH a user: AppError bodies and role/map display fields.
    const keep = [];
    const re = /AppError::\w+\(\s*(?:format!\()?\s*"([^"]{4,})"/g;
    let m;
    while ((m = re.exec(source))) keep.push(m[1]);
    text = keep.join("\n");
  }
  return text;
}

function scan() {
  const hits = [];
  for (const [globs, kind] of [
    [FE_GLOBS, "fe"],
    [RS_GLOBS, "rs"],
  ]) {
    for (const file of tracked(globs)) {
      const source = readFileSync(path.join(ROOT, file), "utf8");
      const text = visibleText(file, source);
      if (!text.trim()) continue;
      const lines = text.split("\n");
      for (const [term, replacement, kinds] of BANNED) {
        // A term may name a surface it applies to; the default is every surface.
        if (kinds && !kinds.includes(kind)) continue;
        const re = new RegExp(term, "i");
        lines.forEach((line, i) => {
          if (re.test(line)) {
            hits.push({
              file,
              kind,
              term,
              replacement,
              sample: line.trim().slice(0, 100),
            });
          }
        });
      }
    }
  }
  return hits;
}

function loadJson(file, fallback) {
  return existsSync(file) ? JSON.parse(readFileSync(file, "utf8")) : fallback;
}

const args = new Set(process.argv.slice(2));
const hits = scan();
const allow = loadJson(ALLOWLIST, { entries: [] });
const baseline = loadJson(BASELINE, { count: Number.MAX_SAFE_INTEGER, files: {} });

const allowed = new Set(allow.entries.map((e) => `${e.file}::${e.term}`));
const live = hits.filter((h) => !allowed.has(`${h.file}::${h.term}`));

if (args.has("--update-baseline")) {
  const files = {};
  for (const h of live) files[h.file] = (files[h.file] ?? 0) + 1;
  writeFileSync(
    BASELINE,
    JSON.stringify(
      {
        _comment:
          "Transitional debt only. This number must never grow. Entries are removed by " +
          "rewriting the copy, never by adding to the allowlist without a reason.",
        count: live.length,
        files,
      },
      null,
      2,
    ) + "\n",
  );
  console.log(`baseline written: ${live.length} hits`);
  process.exit(0);
}

const byFile = new Map();
for (const h of live) {
  if (!byFile.has(h.file)) byFile.set(h.file, []);
  byFile.get(h.file).push(h);
}
for (const [file, list] of [...byFile.entries()].sort()) {
  console.log(`\n${file}`);
  for (const h of list.slice(0, 6)) {
    console.log(`  ${h.term}  →  ${h.replacement}`);
    console.log(`      ${h.sample}`);
  }
  if (list.length > 6) console.log(`  … ${list.length - 6} more`);
}

console.log(
  `\nvocabulary: ${live.length} user-visible hits ` +
    `(baseline ${baseline.count}, allowlisted ${allow.entries.length})`,
);

if (live.length > baseline.count) {
  console.error(
    `\nREGRESSION: ${live.length - baseline.count} new hits above the baseline. ` +
      `The baseline may only shrink.`,
  );
  process.exit(1);
}
if (args.has("--strict") && live.length > 0) {
  console.error("\nstrict mode: no user-visible jargon is permitted");
  process.exit(1);
}
process.exit(0);
