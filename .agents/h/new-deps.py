#!/usr/bin/env python3
"""Check: nowa zaleznosc nie wchodzi po cichu.

Repo nie przyjmuje nowych crate'ow/pakietow bez decyzji czlowieka. Prompt planisty
o tym mowi, ale prompt to prosba — to jest bramka. Zmierzone: agent implementacyjny
dodal `strum` do Cargo.toml w rundzie poprawkowej, bo plan tak kazal, i nikt tego
nie zatwierdzil.

Porownuje ZBIORY nazw zaleznosci miedzy HEAD a drzewem roboczym — nie linie diffa.
Diff z --unified=0 gubi naglowek sekcji, wiec dodana linia `foo = "1"` jest
nieodroznialna od zmiany `version = "1"` w istniejacej zaleznosci.

Swiadome przepuszczenie: H_ALLOW_NEW_DEPS=1 scripts/h run ...
"""
import json
import os
import re
import subprocess
import sys
from pathlib import Path

SECTION = re.compile(r"^\s*\[([^\]]+)\]\s*$")
KEY = re.compile(r"^\s*([A-Za-z0-9_-]+)\s*=")
DEP_SECTION = re.compile(r"(^|\.)(dev-|build-)?dependencies$")


def cargo_deps(text: str) -> set:
    out, section = set(), None
    for line in text.splitlines():
        m = SECTION.match(line)
        if m:
            section = m.group(1)
            continue
        if not section or not DEP_SECTION.search(section):
            continue
        k = KEY.match(line)
        if k:
            out.add(f"{k.group(1)} [{section}]")
    return out


def npm_deps(text: str) -> set:
    try:
        d = json.loads(text)
    except json.JSONDecodeError:
        return set()
    return {f"{name} [{key}]" for key in ("dependencies", "devDependencies")
            for name in (d.get(key) or {})}


def head_text(path: str) -> str:
    r = subprocess.run(["git", "show", f"HEAD:{path}"], capture_output=True, text=True)
    return r.stdout if r.returncode == 0 else ""


def manifests() -> list:
    r = subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True)
    root = Path(r.stdout.strip() or ".")
    found = [p for p in ("Cargo.toml", "src-tauri/Cargo.toml", "package.json")
             if (root / p).exists()]
    found += [str(p.relative_to(root)) for p in root.glob("crates/*/Cargo.toml")]
    return found


def main() -> None:
    added = []
    for path in manifests():
        parse = npm_deps if path.endswith(".json") else cargo_deps
        before = parse(head_text(path))
        after = parse(Path(path).read_text())
        for d in sorted(after - before):
            added.append(f"{d}  w {path}")
    if not added:
        print("nowe zaleznosci: brak")
        return
    if os.environ.get("H_ALLOW_NEW_DEPS") == "1":
        print("nowe zaleznosci (przepuszczone przez H_ALLOW_NEW_DEPS=1):")
        for d in added:
            print(f"  + {d}")
        return
    print("ZABLOKOWANE: ta zmiana dodaje zaleznosci, ktorych nikt nie zatwierdzil:",
          file=sys.stderr)
    for d in added:
        print(f"  + {d}", file=sys.stderr)
    print("\nKilkanascie linii wlasnego kodu prawie zawsze bije nowy crate.\n"
          "Jesli zaleznosc jest naprawde potrzebna: H_ALLOW_NEW_DEPS=1 scripts/h run ...",
          file=sys.stderr)
    raise SystemExit(1)


if __name__ == "__main__":
    main()
