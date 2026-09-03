#!/usr/bin/env python3
"""Fail when a registered Tauri command is never invoked from the Angular frontend.

WHY THIS EXISTS (2026-09-03 audit F2/F3). `generate_handler!` in `src-tauri/src/lib.rs` is the
only registry of IPC entry points, and every entry is callable by any JavaScript running in the
webview. A command that no `invoke` ever names is therefore pure surface with zero benefit: it
cannot be exercised by the product, so it silently rots, and a leak/gate regression inside it is
invisible to every gate we run. `cargo test --lib`, `ng lint` and `ng build` are all structurally
blind to it — the command compiles, the FE compiles, and nothing connects the two.

THE TWO TRAPS THIS CHECKER IS BUILT AGAINST (both found empirically while writing it, each of
which would have made a naive version report a confident falsehood):

  1. `#[tauri::command(rename = "…")]` — the WIRE name can differ from the Rust fn name.
     `check_for_update_guarded` is registered under the fn name but reachable as
     `check_for_update`, which the FE calls constantly. A checker comparing fn names calls a
     live command dead; "fixing" that by deleting it would have removed the update check.
  2. Substring matching — `"get_entity_dossier"` (an MCP/agent TOOL name, a different thing)
     CONTAINS `entity_dossier`, and command names also appear in prose comments on both sides.
     So this does not search per-command; it collects the SET of names actually passed to an
     `invoke(…)` call and compares sets. Comments and decoys cannot enter that set.

ONLY `src/` COUNTS AS A CALLER, never `e2e/`. Every `mockTauri` fixture is hand-written TypeScript
typed against the FE's own interface, so a mock DEFINES a shape rather than verifying one
(angular-zoneless.md T6). Counting a mocked name as a call would make a command that only the test
harness ever names look live — the exact false negative this checker exists to prevent.

`--self-test` runs fixtures that prove the checker still has teeth: an orphan that MUST be
reported, and a renamed-but-called command that MUST NOT be. A guard whose control is missing
is a guard you are not measuring (CLAUDE.md Track B).
"""

from __future__ import annotations

import argparse
import os
import re
import sys
import tempfile

# An `invoke` call site: the word, an optional type argument, then the paren. The type argument is
# bounded so the scan cannot leap over an unrelated statement to a later call — an unbounded
# `[^(]*` will happily cross an entire import line and read the wrong name.
INVOKE_SITE = re.compile(r"""\binvoke\b\s*(?:<[^;{}()]*>)?\s*\(""")
# …whose first argument must be a plain string literal for the name to be readable at all.
INVOKE_NAME = re.compile(r"""\s*["']([a-z0-9_]+)["']""")
# The attribute, then anything that may legally sit between it and the fn — further attributes,
# doc comments, ordinary comments, blank lines — then the fn name. The gap matters: a `///` between
# the attribute and `pub fn` used to drop the whole definition out of the rename map, which turned a
# LIVE renamed command into a reported orphan (found in review, 2026-09-03).
_GAP = r"""(?:\s|\#\[[^\]]*\]|///[^\n]*|//![^\n]*|//[^\n]*|/\*.*?\*/)*"""
COMMAND = re.compile(
    r"""\#\[tauri::command(?:\(([^)]*)\))?\]""" + _GAP + r"""pub\s+(?:async\s+)?fn\s+([a-z0-9_]+)""",
    re.S,
)
RENAME = re.compile(r"""rename\s*=\s*"([^"]+)\"""")
# A reason has to actually say something; "x" satisfies "non-empty" and explains nothing.
MIN_REASON = 15


def strip_comments(source: str) -> str:
    """Blank out // and /* */ comments, respecting string and template literals.

    Without this, a comment that merely CONTAINS `invoke("name")` — a JSDoc `@example`, a rationale
    note — silences a real orphan. Blanking rather than deleting keeps every byte offset, so any
    position reported later still points at the right place.
    """
    out = list(source)
    i, n = 0, len(source)
    while i < n:
        ch = source[i]
        if ch in "\"'`":
            quote, i = ch, i + 1
            while i < n:
                if source[i] == "\\":
                    i += 2
                    continue
                if source[i] == quote:
                    i += 1
                    break
                i += 1
            continue
        if ch == "/" and i + 1 < n and source[i + 1] == "/":
            while i < n and source[i] != "\n":
                out[i] = " "
                i += 1
            continue
        if ch == "/" and i + 1 < n and source[i + 1] == "*":
            while i < n and not (source[i] == "*" and i + 1 < n and source[i + 1] == "/"):
                if source[i] != "\n":
                    out[i] = " "
                i += 1
            for _ in range(2):
                if i < n:
                    out[i] = " "
                    i += 1
            continue
        i += 1
    return "".join(out)


def registered(lib_rs: str) -> list[str]:
    """Fn names inside `generate_handler![…]`, found by matching brackets, not by a lazy regex."""
    src = read(lib_rs)
    start = src.index("generate_handler![")
    i = start + len("generate_handler![")
    depth = 1
    while depth:
        if src[i] == "[":
            depth += 1
        elif src[i] == "]":
            depth -= 1
        i += 1
    return re.findall(r"commands::([a-z0-9_]+)", src[start:i])


def read(path: str) -> str:
    with open(path, encoding="utf-8", errors="replace") as fh:
        return fh.read()


def walk(root: str, suffixes: tuple[str, ...]) -> list[str]:
    out = []
    for base, dirs, files in os.walk(root):
        dirs[:] = [d for d in dirs if d not in {"target", "gen", "node_modules", ".git"}]
        out += [os.path.join(base, f) for f in files if f.endswith(suffixes)]
    return out


def wire_names(rust_root: str) -> dict[str, str]:
    """fn name -> the name the FE must invoke (the `rename` when present)."""
    names: dict[str, str] = {}
    for path in walk(rust_root, (".rs", ".inc")):
        for attr, fn in COMMAND.findall(read(path)):
            hit = RENAME.search(attr or "")
            names[fn] = hit.group(1) if hit else fn
    return names


def invoked(fe_root: str) -> tuple[set[str], list[str]]:
    """Names passed to an `invoke(…)`, plus call sites whose name this cannot read.

    An unreadable name — a constant, a template literal, a computed string — is REPORTED rather
    than ignored. Ignoring it would silently mark a live command dead, and guessing is worse: the
    checker says what it cannot see and asks for a literal, which is also what the one-method-per-
    command rule already wants.
    """
    names: set[str] = set()
    unreadable: list[str] = []
    for path in walk(fe_root, (".ts", ".html")):
        source = strip_comments(read(path))
        for site in INVOKE_SITE.finditer(source):
            tail = source[site.end() : site.end() + 80]
            hit = INVOKE_NAME.match(tail)
            if hit:
                names.add(hit.group(1))
            else:
                line = source.count("\n", 0, site.start()) + 1
                unreadable.append(f"{path}:{line}")
    return names, unreadable


def allowlist(path: str) -> tuple[dict[str, str], list[str]]:
    """`name: reason` per line. A reason is mandatory — an entry without one is a rubber stamp."""
    entries: dict[str, str] = {}
    errors: list[str] = []
    if not os.path.exists(path):
        return entries, errors
    seen: dict[str, int] = {}
    for lineno, raw in enumerate(read(path).splitlines(), 1):
        line = raw.split("#", 1)[0].strip() if raw.lstrip().startswith("#") else raw.strip()
        if not line:
            continue
        name, sep, reason = line.partition(":")
        name, reason = name.strip(), reason.strip()
        if not sep or not reason:
            errors.append(f"{path}:{lineno}: needs `name: reason` — an entry with no reason is a rubber stamp")
            continue
        if len(reason) < MIN_REASON:
            errors.append(
                f"{path}:{lineno}: `{name}`'s reason is {len(reason)} characters — say what the plan "
                f"is (at least {MIN_REASON}), or the ledger records nothing a reviewer can act on"
            )
            continue
        if name in seen:
            errors.append(
                f"{path}:{lineno}: `{name}` is already listed on line {seen[name]} — a duplicate "
                f"silently overrides the earlier reason, which is how a copy-paste mistake hides"
            )
            continue
        seen[name] = lineno
        entries[name] = reason
    return entries, errors


def check(repo: str) -> list[str]:
    lib_rs = os.path.join(repo, "src-tauri", "src", "lib.rs")
    allow_path = os.path.join(repo, "scripts", "dead-commands-allowlist.txt")
    names = wire_names(os.path.join(repo, "src-tauri", "src"))
    used, unreadable = invoked(os.path.join(repo, "src"))
    allowed, errors = allowlist(allow_path)
    for site in unreadable:
        errors.append(
            f"{site}: invoke() is called with a name this checker cannot read (not a plain string "
            f"literal). Pass the command name as a literal so the registry stays checkable."
        )

    reg = registered(lib_rs)
    wire_of = {fn: names.get(fn, fn) for fn in reg}
    orphans = {fn for fn, w in wire_of.items() if w not in used}

    for fn in sorted(orphans - set(allowed)):
        w = wire_of[fn]
        as_wire = f' (wire "{w}")' if w != fn else ""
        errors.append(
            f"{lib_rs}: `{fn}`{as_wire} is registered in generate_handler! but no invoke() names it. "
            f"Wire it up, delete it, or add `{fn}: <why it stays>` to {allow_path}."
        )
    # A stale allowlist is how a lint goes quietly vacuous, so both directions of rot are errors.
    for fn in sorted(set(allowed) - orphans):
        why = "is invoked from the FE now" if fn in reg else "is no longer registered"
        errors.append(f"{allow_path}: `{fn}` {why} — drop its allowlist entry.")
    return errors


FIXTURE_LIB = '''
fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            commands::live_one,
            commands::renamed_one,
            commands::orphan_one,
        ])
}
'''
FIXTURE_CMDS = '''
#[tauri::command]
pub fn live_one() {}

/// A decoy in prose: orphan_one is named here and must not count as a call.
#[tauri::command(rename = "wire_name")]
pub async fn renamed_one() {}

// Regression (review, 2026-09-03): a doc comment BETWEEN the attribute and `pub fn` used to drop
// this definition out of the rename map, reporting a LIVE command as dead.
#[tauri::command(rename  =  "documented_wire")]
/// Doc comment sitting between the attribute and the signature.
#[allow(clippy::needless_pass_by_value)]
pub async fn documented_rename() {}

#[tauri::command]
pub fn orphan_one() {}
'''
FIXTURE_FE = '''
// A comment naming "orphan_one" must not make it look used.
/* A block comment containing a literal call: invoke("orphan_one") — still not a call. */
const url = "https://example.invalid/not//a/comment";
const a = await invoke<void>("live_one");
const b = await invoke<number>("wire_name", { x: 1 });
const d = await invoke<Record<string, number>>("documented_wire");
const c = await invoke<void>("get_orphan_one");  // substring decoy
'''
FIXTURE_FE_DYNAMIC = '''
const NAME = "live_one";
const a = await invoke<void>(NAME);
'''


def self_test() -> int:
    with tempfile.TemporaryDirectory() as root:
        os.makedirs(os.path.join(root, "src-tauri", "src"))
        os.makedirs(os.path.join(root, "src"))
        os.makedirs(os.path.join(root, "scripts"))
        with open(os.path.join(root, "src-tauri", "src", "lib.rs"), "w") as fh:
            fh.write(FIXTURE_LIB)
        with open(os.path.join(root, "src-tauri", "src", "commands.rs"), "w") as fh:
            fh.write(FIXTURE_CMDS)
        with open(os.path.join(root, "src", "ipc.service.ts"), "w") as fh:
            fh.write(FIXTURE_FE)

        errors = check(root)
        failures = []
        if len(errors) != 1 or "`orphan_one`" not in errors[0]:
            failures.append(f"expected exactly one finding naming orphan_one, got: {errors}")
        if any("renamed_one" in e for e in errors):
            failures.append("renamed_one is reachable as its wire name and must not be reported")
        if any("documented_rename" in e for e in errors):
            failures.append(
                "documented_rename carries a doc comment between the attribute and the signature; "
                "its rename must still be honoured or a live command gets reported dead"
            )
        if any("live_one" in e for e in errors):
            failures.append("live_one is invoked directly and must not be reported")

        # A rubber-stamp entry (no reason) must be rejected, and a good one must silence the finding.
        allow = os.path.join(root, "scripts", "dead-commands-allowlist.txt")
        with open(allow, "w") as fh:
            fh.write("orphan_one\n")
        if not any("rubber stamp" in e for e in check(root)):
            failures.append("an allowlist entry with no reason must be rejected")
        with open(allow, "w") as fh:
            fh.write("orphan_one: backend-complete, UI pending\n")
        if check(root):
            failures.append(f"a reasoned allowlist entry must silence the finding, got: {check(root)}")
        # And a stale entry (naming a command that IS invoked) must be reported.
        with open(allow, "w") as fh:
            fh.write("orphan_one: backend-complete, UI pending\nlive_one: stale entry, wired since\n")
        if not any("`live_one` is invoked from the FE now" in e for e in check(root)):
            failures.append("a stale allowlist entry must be reported")

        # A reason has to say something. "x" is non-empty and explains nothing.
        with open(allow, "w") as fh:
            fh.write("orphan_one: x\n")
        if not any("characters" in e for e in check(root)):
            failures.append("a one-character reason must be rejected as substanceless")

        # A duplicate silently overrode the earlier reason before this check existed.
        with open(allow, "w") as fh:
            fh.write("orphan_one: backend-complete, UI pending\norphan_one: pasted twice by mistake\n")
        if not any("already listed on line" in e for e in check(root)):
            failures.append("a duplicate allowlist key must be reported")

        # A name the checker cannot read must be an ERROR, never a silent pass: ignoring it marks a
        # live command dead, and guessing is worse.
        with open(allow, "w") as fh:
            fh.write("orphan_one: backend-complete, UI pending\n")
        with open(os.path.join(root, "src", "dynamic.ts"), "w") as fh:
            fh.write(FIXTURE_FE_DYNAMIC)
        dynamic = check(root)
        if not any("cannot read" in e for e in dynamic):
            failures.append("an invoke() whose name is not a literal must be reported, not ignored")
        os.remove(os.path.join(root, "src", "dynamic.ts"))
        if check(root):
            failures.append("removing the dynamic call site must return the repo to clean")

        for f in failures:
            print(f"self-test FAIL: {f}", file=sys.stderr)
        if failures:
            return 1
        print("check-dead-commands self-test: 9 checks passed (orphan detected, rename honoured "
              "incl. a doc comment between attribute and signature, line+block comment and "
              "substring decoys ignored, reasonless / substanceless / duplicate entries rejected, "
              "stale entry reported, unreadable invoke name reported)")
        return 0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--repo", default=os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    ap.add_argument("--self-test", action="store_true", help="prove the checker still has teeth")
    args = ap.parse_args()
    if args.self_test:
        return self_test()
    errors = check(args.repo)
    for e in errors:
        print(f"dead-command: {e}", file=sys.stderr)
    if errors:
        print(f"\n{len(errors)} dead-command finding(s).", file=sys.stderr)
        return 1
    print("check-dead-commands: every registered Tauri command is invoked or explained.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
