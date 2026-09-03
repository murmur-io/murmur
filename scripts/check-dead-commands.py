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

# `invoke` optionally carries a type argument (`invoke<OrgTask[]>(…)`), so skip anything up to the
# call paren — type arguments never contain one — then take the first string literal.
INVOKE = re.compile(r"""invoke[^(]*\(\s*["']([a-z0-9_]+)["']""")
# The attribute, any further attributes stacked under it, then the fn name.
COMMAND = re.compile(
    r"""\#\[tauri::command(?:\(([^)]*)\))?\]((?:\s*\#\[[^\]]*\])*)\s*pub\s+(?:async\s+)?fn\s+([a-z0-9_]+)"""
)
RENAME = re.compile(r"""rename\s*=\s*"([^"]+)\"""")


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
        for attr, _stacked, fn in COMMAND.findall(read(path)):
            hit = RENAME.search(attr or "")
            names[fn] = hit.group(1) if hit else fn
    return names


def invoked(fe_root: str) -> set[str]:
    return {
        name
        for path in walk(fe_root, (".ts", ".html"))
        for name in INVOKE.findall(read(path))
    }


def allowlist(path: str) -> tuple[dict[str, str], list[str]]:
    """`name: reason` per line. A reason is mandatory — an entry without one is a rubber stamp."""
    entries: dict[str, str] = {}
    errors: list[str] = []
    if not os.path.exists(path):
        return entries, errors
    for lineno, raw in enumerate(read(path).splitlines(), 1):
        line = raw.split("#", 1)[0].strip() if raw.lstrip().startswith("#") else raw.strip()
        if not line:
            continue
        name, sep, reason = line.partition(":")
        name, reason = name.strip(), reason.strip()
        if not sep or not reason:
            errors.append(f"{path}:{lineno}: needs `name: reason` — an entry with no reason is a rubber stamp")
            continue
        entries[name] = reason
    return entries, errors


def check(repo: str) -> list[str]:
    lib_rs = os.path.join(repo, "src-tauri", "src", "lib.rs")
    allow_path = os.path.join(repo, "scripts", "dead-commands-allowlist.txt")
    names = wire_names(os.path.join(repo, "src-tauri", "src"))
    used = invoked(os.path.join(repo, "src"))
    allowed, errors = allowlist(allow_path)

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

#[tauri::command]
pub fn orphan_one() {}
'''
FIXTURE_FE = '''
// A comment naming "orphan_one" must not make it look used.
const a = await invoke<void>("live_one");
const b = await invoke<number>("wire_name", { x: 1 });
const c = await invoke<void>("get_orphan_one");  // substring decoy
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
            fh.write("orphan_one: ok\nlive_one: stale\n")
        if not any("`live_one` is invoked from the FE now" in e for e in check(root)):
            failures.append("a stale allowlist entry must be reported")

        for f in failures:
            print(f"self-test FAIL: {f}", file=sys.stderr)
        if failures:
            return 1
        print("check-dead-commands self-test: 5 checks passed (orphan detected, rename honoured, "
              "decoy+comment ignored, reasonless entry rejected, stale entry reported)")
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
