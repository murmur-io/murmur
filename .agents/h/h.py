#!/usr/bin/env python3
"""Mały harness: zadanie -> plan -> implementacja -> weryfikacja -> (max 2 poprawki) -> koniec.

Bez receiptów, bez attestacji, bez ledgerów, bez proof-gapów. Zamierzona granica:
ten plik ma zostać mały. Jeśli rośnie powyżej ~500 linii, coś tu nie pasuje.
"""
from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
HDIR = ROOT / ".agents" / "h"
CFG = json.loads((HDIR / "checks.json").read_text())
TASKS_ROOT = ROOT.parent / ".murmur-agent-tasks"
STATE_DIR = ROOT / ".git" / "h"
# Kazdy worktree ma wlasny, ZIMNY target/ — pelna kompilacja ML tree od zera (~15 min, 4 GB).
# Wskazanie wszystkich taskow na cieply target glownego checkoutu robi z tego kilkadziesiat
# sekund. Cargo trzyma wlasny file-lock, a artefakty sa kluczowane odciskiem zrodel, wiec
# rownolegle buildy sie serializuja zamiast psuc. Nadpisz H_TARGET_DIR, jesli chcesz osobny.
SHARED_TARGET = Path(os.environ.get("H_TARGET_DIR") or (ROOT / "target"))
MAX_FIX_ROUNDS = 2

VERIFY_SCHEMA = {
    "type": "object",
    "properties": {
        "werdykt": {"type": "string", "enum": ["DZIALA", "NIE_DZIALA", "NIE_WIEM"]},
        "co_nie_dziala": {"type": "string"},
        "jak_naprawic": {"type": "string"},
    },
    "required": ["werdykt", "co_nie_dziala", "jak_naprawic"],
    "additionalProperties": False,
}


# ---------- drobiazgi ----------

def log(msg: str) -> None:
    print(f"\033[36m[h]\033[0m {msg}", flush=True)


def die(msg: str) -> "NoReturn":  # type: ignore[valid-type]
    print(f"\033[31m[h] {msg}\033[0m", file=sys.stderr, flush=True)
    raise SystemExit(1)


def glob_re(pattern: str) -> re.Pattern:
    """Glob -> regex. `**` przechodzi przez `/`, `*` nie."""
    out, i = [], 0
    while i < len(pattern):
        c = pattern[i]
        if pattern.startswith("**/", i):
            out.append("(?:.*/)?"); i += 3
        elif pattern.startswith("**", i):
            out.append(".*"); i += 2
        elif c == "*":
            out.append("[^/]*"); i += 1
        elif c == "?":
            out.append("[^/]"); i += 1
        else:
            out.append(re.escape(c)); i += 1
    return re.compile("^" + "".join(out) + "$")


def matches(path: str, patterns: list) -> bool:
    return any(glob_re(p).search(path) for p in patterns)


def git(*args: str, cwd: Path = None, check: bool = True, strip: bool = True) -> str:
    """`strip=False` dla --porcelain: wiodaca spacja pierwszej linii NIESIE ZNACZENIE
    (kolumna X statusu), a .strip() ja zjadal i przesuwal parsowanie sciezki o dwa znaki."""
    r = subprocess.run(["git", *args], cwd=str(cwd or ROOT), capture_output=True, text=True)
    if check and r.returncode != 0:
        die(f"git {' '.join(args)} -> {r.stderr.strip()}")
    return r.stdout.strip() if strip else r.stdout


def state_path(task_id: str) -> Path:
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    return STATE_DIR / f"{task_id}.json"


def load_state(task_id: str) -> dict:
    p = state_path(task_id)
    return json.loads(p.read_text()) if p.exists() else {}


def save_state(task_id: str, **kw) -> dict:
    s = load_state(task_id); s.update(kw)
    state_path(task_id).write_text(json.dumps(s, indent=2, ensure_ascii=False))
    return s


# ---------- checki ----------

def changed_paths(wt: Path) -> list:
    out = git("status", "--porcelain=v1", "--untracked-files=all", cwd=wt, strip=False)
    paths = []
    for line in out.splitlines():
        if len(line) < 4:
            continue
        p = line[3:].strip()
        if " -> " in p:
            p = p.split(" -> ", 1)[1]
        p = p.strip('"')
        # Pliki robocze harnessu nie sa zmiana w zadaniu.
        if p and not Path(p).name.startswith(".h-"):
            paths.append(p)
    return paths


def rust_filters(paths: list) -> list:
    """Nazwy modułów z dotkniętych plików .rs — filtry dla `cargo test --lib`.

    Zwraca [] (= PEŁNY suite, bez zawężania), gdy ruszony jest korzeń crate'a
    (`lib.rs`/`main.rs`) albo `build.rs`. Stara wersja brała dla nich nazwę katalogu
    rodzica albo stem pliku — czyli dosłownie `src` dla `lib.rs` i `build` dla
    `build.rs`. Zmierzone: `cargo test --lib -- src` odpala 1 test z 3548 i kończy
    zielono w 0,05 s, a `-- build` odpala 47 przypadkowych trafień po nazwie
    (`build_params_*`, `build_memory_brief_*`) w 1,6 s, z których żaden nie dotyka
    build scriptu. `lib.rs` to rejestr `generate_handler!`, a `build.rs` może zmienić
    `rustc-env`/`link-arg` dla całego cratea — fałszywa zieleń tam jest gorsza niż
    wolny check.
    """
    mods = []
    for p in paths:
        if not (p.endswith(".rs") and (p.startswith("src-tauri/") or p.startswith("crates/"))):
            continue
        if Path(p).name in ("lib.rs", "main.rs", "build.rs"):
            return []
        stem = Path(p).stem
        if stem == "mod":
            stem = Path(p).parent.name
        # `src` nigdy nie jest nazwą modułu — to katalog źródeł. Gdyby jakaś ścieżka
        # jednak się na to zmapowała, lecimy pełnym suitem zamiast filtrować w próżni.
        if stem == "src":
            return []
        if stem and stem not in mods:
            mods.append(stem)
    return mods


def playwright_filters(paths: list) -> list:
    return [p for p in paths if p.startswith("e2e/") and p.endswith(".spec.ts")]


def derive_checks(paths: list) -> list:
    """Zmienione ścieżki -> lista (id, cmd, cwd, budget). Najtańsze pierwsze."""
    picked = []
    for cid, spec in CFG["checks"].items():
        if not matches_any(paths, spec["when"]):
            continue
        cmd, cwd = spec["cmd"], spec.get("cwd")
        if spec.get("scoped"):
            cmd = scope_cmd(cid, cmd, paths)
        picked.append((cid, cmd, cwd, spec.get("budget_s", 900)))
    order = list(CFG["checks"].keys())
    picked.sort(key=lambda c: order.index(c[0]))
    return picked


def matches_any(paths: list, patterns: list) -> bool:
    return any(matches(p, patterns) for p in paths)


def scope_cmd(cid: str, cmd: str, paths: list) -> str:
    """Zawęź check do tego, co realnie zmienione. To jest cała oszczędność czasu."""
    limit = CFG.get("rust_test_scope_limit", 6)
    if cid == "rust-test":
        mods = rust_filters(paths)
        if mods and len(mods) <= limit:
            return cmd + " " + " ".join(mods)
    if cid == "playwright":
        specs = playwright_filters(paths)
        if specs and len(specs) <= limit:
            rel = " ".join(s[len("e2e/"):] for s in specs)
            return cmd + " " + rel
    return cmd


def run_check(cid: str, cmd: str, cwd: str, budget: int, wt: Path) -> dict:
    where = wt / cwd if cwd else wt
    log(f"check {cid}: {cmd}")
    t0 = time.time()
    env = dict(os.environ, CARGO_BUILD_JOBS="2", CARGO_TARGET_DIR=str(SHARED_TARGET))
    try:
        r = subprocess.run(["/bin/zsh", "-f", "-c", cmd], cwd=str(where), capture_output=True,
                           text=True, timeout=budget, env=env)
        out, code = (r.stdout + r.stderr), r.returncode
    except subprocess.TimeoutExpired as e:
        out, code = ((e.stdout or b"").decode("utf-8", "replace") + "\n[TIMEOUT]"), 124
    dt = time.time() - t0
    ok = code == 0
    log(f"  {'OK ' if ok else 'FAIL'} {cid} ({dt:.0f}s)")
    if not ok:
        # Pokaz POWOD od razu. Bez tego patrzysz na "FAIL rust-clippy (35s)" i czekasz
        # na weryfikatora, zeby sie dowiedziec, co sie stalo.
        for line in out.strip().splitlines()[-25:]:
            print(f"      {line}")
    return {"id": cid, "ok": ok, "cmd": cmd, "seconds": round(dt), "tail": out[-4000:] if not ok else ""}


# ---------- modele ----------

def call_model(vendor: str, prompt: str, cwd: Path, *, write: bool, schema: dict = None,
               timeout: int = 2400, resume: bool = False) -> str:
    exe = shutil.which(vendor)
    if not exe:
        die(f"nie znaleziono `{vendor}` w PATH")
    if vendor == "codex":
        argv = [exe, "exec", "--skip-git-repo-check", "--cd", str(cwd),
                "--sandbox", "workspace-write" if write else "read-only"]
        # codex exec nie ma odpowiednika --continue; poprawka czyta wlasny kod z worktree.
        out_file = None
        if schema:
            sf = cwd / ".h-schema.json"; sf.write_text(json.dumps(schema))
            out_file = cwd / ".h-out.json"
            argv += ["--output-schema", str(sf), "-o", str(out_file)]
        argv.append("-")
    elif vendor == "claude":
        argv = [exe, "--print", "--add-dir", str(cwd),
                "--permission-mode", "acceptEdits" if write else "plan"]
        # Poprawka kontynuuje TE SAMA sesje w tym worktree — agent pamieta, co juz probowal,
        # zamiast odtwarzac rozumowanie z samego kodu.
        if resume:
            argv.append("--continue")
        out_file = None
        if schema:
            argv += ["--json-schema", json.dumps(schema)]
    else:
        die(f"nieznany vendor: {vendor}")

    env = dict(os.environ, CARGO_TARGET_DIR=str(SHARED_TARGET), CARGO_BUILD_JOBS="2")
    r = subprocess.run(argv, cwd=str(cwd), input=prompt, capture_output=True, text=True,
                       timeout=timeout, env=env)
    if r.returncode != 0:
        die(f"{vendor} zakończył się kodem {r.returncode}:\n{(r.stderr or r.stdout)[-1500:]}")
    if schema:
        if out_file and out_file.exists():
            text = out_file.read_text(); out_file.unlink(missing_ok=True)
            (cwd / ".h-schema.json").unlink(missing_ok=True)
            return text
        return r.stdout
    return r.stdout


def parse_json(text: str) -> dict:
    text = text.strip()
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        m = re.search(r"\{.*\}", text, re.S)
        if m:
            try:
                return json.loads(m.group(0))
            except json.JSONDecodeError:
                pass
    die(f"model nie zwrócił JSON-a:\n{text[:800]}")


def prompt_file(name: str) -> str:
    return (HDIR / "prompts" / f"{name}.md").read_text()


# ---------- fazy ----------

def phase_plan(task_id: str, task: str, wt: Path, vendor: str) -> str:
    log(f"plan ({vendor})…")
    p = f"{prompt_file('plan')}\n\n## Zadanie\n\n{task}\n"
    plan = call_model(vendor, p, wt, write=False).strip()
    (wt / ".h-plan.md").write_text(plan)
    print(f"\n\033[1m--- PLAN ---\033[0m\n{plan}\n")
    return plan


def phase_implement(task: str, plan: str, wt: Path, vendor: str, feedback: str = "") -> None:
    log(f"implementacja ({vendor})…" + (" [poprawka]" if feedback else ""))
    p = f"{prompt_file('implement')}\n\n## Zadanie\n\n{task}\n\n## Plan\n\n{plan}\n"
    if feedback:
        p += (f"\n## Weryfikacja odrzuciła poprzednią wersję\n\n{feedback}\n\n"
              "Popraw dokładnie to. Nie zaczynaj od zera, nie przepisuj reszty.\n")
    call_model(vendor, p, wt, write=True, resume=bool(feedback))


def phase_check(wt: Path) -> tuple:
    paths = changed_paths(wt)
    if not paths:
        return [], paths
    picked = derive_checks(paths)
    if not picked:
        log("żaden check nie pasuje do zmienionych ścieżek")
        return [], paths
    # Dopiero teraz — po tym, jak agent skonczyl edytowac — wiadomo, czy zaleznosci FE
    # sie nie zmienily. Zob. docstring.
    if any(c[0] in ("ng-lint", "ng-build", "playwright") for c in picked):
        link_shared_node_modules(wt)
    results = []
    for cid, cmd, cwd, budget in picked:
        results.append(run_check(cid, cmd, cwd, budget, wt))
    return results, paths


def phase_verify(task: str, plan: str, wt: Path, checks: list, vendor: str) -> dict:
    log(f"weryfikacja ({vendor})…")
    diff = git("diff", "HEAD", cwd=wt, check=False)
    untracked = [p for p in changed_paths(wt) if not (wt / p).is_dir()]
    for p in untracked:
        f = wt / p
        try:
            if f.exists() and f.stat().st_size < 200_000 and p not in diff:
                diff += f"\n--- NOWY PLIK: {p} ---\n{f.read_text(errors='replace')}\n"
        except (OSError, UnicodeDecodeError):
            pass
    if len(diff) > 400_000:
        diff = diff[:400_000] + "\n[... diff obcięty ...]"
    csum = "\n".join(
        f"- {c['id']}: {'OK' if c['ok'] else 'FAIL'} ({c['seconds']}s)"
        + (f"\n```\n{c['tail'][-2500:]}\n```" if not c["ok"] else "")
        for c in checks
    ) or "(brak checków dla tych ścieżek)"
    p = (f"{prompt_file('verify')}\n\n## Zadanie\n\n{task}\n\n## Plan i akceptacja\n\n{plan}\n"
         f"\n## Wynik checków\n\n{csum}\n\n## Diff\n\n```diff\n{diff}\n```\n")
    return parse_json(call_model(vendor, p, wt, write=False, schema=VERIFY_SCHEMA))


# ---------- komendy ----------

def link_shared_target(wt: Path) -> None:
    """Podepnij `target/` worktree pod cieply target glownego checkoutu.

    Symlink, nie zmienna srodowiskowa: Claude Code odtwarza wlasne srodowisko ze
    shell-snapshotu, wiec CARGO_TARGET_DIR wstrzykniety w proces modelu NIE dochodzi
    do jego wywolan basha — zmierzone. Symlinka czyta kazdy, kto uruchomi cargo.
    `target/` jest w .gitignore, wiec nie brudzi diffa zadania.

    Zmierzone na tym repo: zimny target w worktree = ~15 min i 4 GB; przez symlink
    ten sam zawezony test = 1 min 53 s.
    """
    link = wt / "target"
    if link.is_symlink():
        return
    if link.exists():                       # prawdziwy katalog z poprzedniego przebiegu
        shutil.rmtree(link, ignore_errors=True)
    if not SHARED_TARGET.exists():
        log(f"uwaga: {SHARED_TARGET} nie istnieje — build bedzie zimny")
        return
    link.symlink_to(SHARED_TARGET)
    log(f"target -> {SHARED_TARGET} (wspoldzielony, cieply)")


def link_shared_node_modules(wt: Path) -> None:
    """Podepnij `node_modules/` worktree pod drzewo glownego checkoutu.

    Bez tego swiezy worktree NIE MA lokalnego Angulara: `ng` nie jest zainstalowany
    globalnie, a harness nigdy nie robi `npm ci`, wiec `npx ng lint` / `npx ng build`
    / `npm run test:e2e` musza isc po CLI do rejestru — wolno i bez zaleznosci
    projektu. `node_modules/` jest w .gitignore, wiec symlink nie brudzi diffa.

    Podpinamy TYLKO wtedy, gdy lockfile worktree jest bajt w bajt taki sam jak w
    glownym checkoucie. Inaczej zadanie zmienia zaleznosci i wspoldzielone drzewo
    byloby klamstwem — wtedy mowimy operatorowi, zeby odpalil `npm ci`.

    Wolane z `phase_check`, NIE z `cmd_run`: gdyby link powstawal przy tworzeniu
    worktree, agent moglby potem podbic wersje zaleznosci (`new-deps.py` porownuje
    ZBIORY NAZW, nie wersje), a checki FE i tak lecialyby na starym drzewie —
    zielony `ng build`, ktory nigdy nie kompilowal sie wobec zadeklarowanych wersji.
    Sprawdzenie przy kazdej rundzie checkow widzi te zmiane.

    UWAGA dla ludzi: to jest symlink, wiec `npm install` odpalony WEWNATRZ worktree
    pisze do drzewa glownego checkoutu. Jesli musisz zainstalowac cokolwiek na
    potrzeby zadania, najpierw usun dowiazanie i zrob `npm ci` lokalnie.
    """
    link = wt / "node_modules"
    if link.is_symlink() and not link.exists():
        link.unlink()                       # dowiazanie wisi (np. po `clean` glownego drzewa)
    elif link.is_symlink() or link.exists():
        return
    shared = ROOT / "node_modules"
    if not shared.is_dir():
        log("uwaga: brak node_modules w glownym checkoucie — checki FE beda zimne")
        return
    lock, shared_lock = wt / "package-lock.json", ROOT / "package-lock.json"
    try:
        same = lock.read_bytes() == shared_lock.read_bytes()
    except OSError:
        same = False
    if not same:
        log("package-lock.json rozni sie od glownego — odpal `npm ci` w worktree "
            "przed checkami FE")
        return
    link.symlink_to(shared)
    log(f"node_modules -> {shared} (wspoldzielony)")


def cmd_run(a) -> None:
    task_id, task = a.task_id, a.prompt
    wt = TASKS_ROOT / task_id
    if not wt.exists():
        TASKS_ROOT.mkdir(parents=True, exist_ok=True)
        log(f"worktree {wt}")
        git("worktree", "add", "-b", f"h/{task_id}", str(wt), "HEAD")
    else:
        log(f"worktree istnieje: {wt}")
    link_shared_target(wt)
    save_state(task_id, task=task, worktree=str(wt), started=time.time())

    plan = phase_plan(task_id, task, wt, a.planner) if not a.no_plan else task
    save_state(task_id, plan=plan)

    feedback, t0 = "", time.time()
    for rnd in range(MAX_FIX_ROUNDS + 1):
        phase_implement(task, plan, wt, a.dev, feedback)
        checks, paths = phase_check(wt)
        if not paths:
            die("agent nic nie zmienił w worktree")
        failed = [c for c in checks if not c["ok"]]
        v = phase_verify(task, plan, wt, checks, a.verifier)
        save_state(task_id, rounds=rnd + 1, last_verdict=v, checks=checks)

        verdict = v.get("werdykt")
        if verdict == "DZIALA" and not failed:
            print(f"\n\033[32m\033[1m=== DZIALA ===\033[0m  ({time.time()-t0:.0f}s, rund: {rnd+1})")
            print(f"worktree: {wt}\nbranch:   h/{task_id}")
            print(f"zmienione: {len(paths)} plików | checki: "
                  + ", ".join(f"{c['id']} {c['seconds']}s" for c in checks))
            print(f"\nDiff:   git -C {wt} diff HEAD")
            print(f"Merge:  git -C {ROOT} merge h/{task_id}")
            print(f"Koniec: .agents/h/h.py clean {task_id}")
            return

        why = v.get("co_nie_dziala") or ""
        how = v.get("jak_naprawic") or ""
        if failed and verdict == "DZIALA":
            why = "Weryfikator uznał zadanie za zrobione, ale check padł: " + \
                  ", ".join(c["id"] for c in failed)
            how = "Napraw padający check, nie zmieniając zachowania, które przeszło weryfikację."
        print(f"\n\033[33m--- {verdict} (runda {rnd+1}/{MAX_FIX_ROUNDS+1}) ---\033[0m\n{why}\n")
        if rnd == MAX_FIX_ROUNDS:
            print(f"\033[31m\033[1m=== STOP po {MAX_FIX_ROUNDS+1} rundach ===\033[0m")
            print(f"Ostatni werdykt: {verdict}\n{why}\n\nSugestia weryfikatora:\n{how}")
            print(f"\nworktree: {wt}  (nic nie usunięte, popraw ręcznie albo zmień zadanie)")
            raise SystemExit(2)
        feedback = f"Werdykt: {verdict}\n\nCo nie działa:\n{why}\n\nJak naprawić:\n{how}"


def cmd_check(a) -> None:
    wt = Path(load_state(a.task_id).get("worktree", ROOT)) if a.task_id else ROOT
    manual = CFG["manual_only"]
    if a.check_id in manual:
        spec = manual[a.check_id]
        r = run_check(a.check_id, spec["cmd"], spec.get("cwd"), 1800, wt)
    else:
        checks, paths = phase_check(wt)
        r = {"ok": all(c["ok"] for c in checks)}
        print(json.dumps(checks, indent=2, ensure_ascii=False))
    raise SystemExit(0 if r["ok"] else 1)


def cmd_status(a) -> None:
    s = load_state(a.task_id)
    if not s:
        die(f"nie ma taska {a.task_id}")
    print(json.dumps(s, indent=2, ensure_ascii=False))


def cmd_clean(a) -> None:
    s = load_state(a.task_id)
    wt = s.get("worktree")
    if wt and Path(wt).exists():
        # Symlink `target` zostaje. Sprawdzone: ani `rm -rf`, ani `git worktree remove
        # --force` nie ida po symlinku — usuwaja dowiazanie, nie cel. Wczesniejsza
        # wersja zdejmowala go profilaktycznie i zostawiala worktree bez targetu, gdy
        # remove odmowil z powodu niezacommitowanej pracy.
        git("worktree", "remove", wt, *(["--force"] if a.force else []))
        log(f"usunięto worktree {wt}")
    git("branch", "-D", f"h/{a.task_id}", check=False)
    state_path(a.task_id).unlink(missing_ok=True)
    log(f"task {a.task_id} zamknięty")


def cmd_list(a) -> None:
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    rows = sorted(STATE_DIR.glob("*.json"))
    if not rows:
        print("brak otwartych tasków")
        return
    for p in rows:
        s = json.loads(p.read_text())
        v = (s.get("last_verdict") or {}).get("werdykt", "-")
        print(f"{p.stem:<40} rundy={s.get('rounds','-')} werdykt={v}")


def main() -> None:
    ap = argparse.ArgumentParser(prog="h", description="mały harness")
    sub = ap.add_subparsers(dest="cmd", required=True)

    r = sub.add_parser("run", help="zadanie -> plan -> kod -> weryfikacja -> koniec")
    r.add_argument("task_id")
    r.add_argument("--prompt", required=True)
    r.add_argument("--planner", default=os.environ.get("H_PLANNER", "codex"))
    r.add_argument("--dev", default=os.environ.get("H_DEV", "claude"))
    r.add_argument("--verifier", default=os.environ.get("H_VERIFIER", "codex"))
    r.add_argument("--no-plan", action="store_true", help="pomiń planistę, zadanie idzie wprost")
    r.set_defaults(fn=cmd_run)

    c = sub.add_parser("check", help="odpal checki (albo jeden manualny)")
    c.add_argument("check_id", nargs="?", default="")
    c.add_argument("--task-id", default="")
    c.set_defaults(fn=cmd_check)

    for name, fn, hlp in (("status", cmd_status, "stan taska"), ("clean", cmd_clean, "zamknij task")):
        s = sub.add_parser(name, help=hlp)
        s.add_argument("task_id")
        if name == "clean":
            s.add_argument("--force", action="store_true")
        s.set_defaults(fn=fn)

    sub.add_parser("list", help="otwarte taski").set_defaults(fn=cmd_list)
    a = ap.parse_args()
    a.fn(a)


if __name__ == "__main__":
    main()
