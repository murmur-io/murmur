#!/usr/bin/env python3
"""Hook PreToolUse dla Bash — to, co ze starego hook_guard.py (2364 linie) zarobiło na siebie.

Zostaje: skan sekretów przed commitem, blokada bezpośredniego pusha na `murmur`.
Znika: parser komend basha. Na 137 odmów starego guarda 87 to były jego własne
błędy parsowania ("No closing quotation", "shell substitution unsupported") —
czyste tarcie, zero złapanych zagrożeń. Ciężkie cargo dostaje teraz ostrzeżenie
zamiast odmowy: pas zasobów to optymalizacja, nie granica bezpieczeństwa.

Wejście: JSON hooka na stdin. Wyjście: exit 0 = przepuść, exit 2 = zablokuj.
"""
import json
import re
import subprocess
import sys

PROTECTED_BRANCH = "murmur"

# Pisownia rozbita znakiem klasy, żeby ten plik nie wykrywał samego siebie.
SECRETS = [
    (re.compile(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----"), "klucz prywatny PEM"),
    (re.compile(r"sk[-]ant[-][A-Za-z0-9_-]{20,}"), "klucz API Anthropic"),
    (re.compile(r"sk[-]proj[-][A-Za-z0-9_-]{20,}"), "klucz projektu OpenAI"),
    (re.compile(r"gh[ps]_[A-Za-z0-9]{20,}"), "token GitHub"),
]
DEV_PLACEHOLDER = "0123456789abcdef" * 4


def deny(msg: str) -> "NoReturn":  # type: ignore[valid-type]
    print(msg, file=sys.stderr)
    raise SystemExit(2)


def git(*args: str) -> str:
    r = subprocess.run(["git", *args], capture_output=True, text=True)
    return r.stdout if r.returncode == 0 else ""


def secret_hits(added: str) -> list:
    hits = [label for pattern, label in SECRETS if pattern.search(added)]
    for line in added.splitlines():
        if re.search(r"[0-9a-fA-F]{64}", line):
            documented = "MURMUR_DEV_DEK" in line or "MURMUR_DEV_KEK" in line
            if not (DEV_PLACEHOLDER in line and documented):
                hits.append("64-hex wartość w kształcie DEK/KEK")
                break
    return sorted(set(hits))


def main() -> None:
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, ValueError):
        return
    cmd = (payload.get("tool_input") or {}).get("command") or ""
    if not cmd:
        return

    if re.search(r"\bgit\b[^|;&]*\bcommit\b", cmd):
        raw = git("diff", "--cached", "--no-color", "--no-ext-diff", "--unified=0", "--")
        added = "\n".join(l[1:] for l in raw.splitlines()
                          if l.startswith("+") and not l.startswith("+++"))
        hits = secret_hits(added)
        if hits:
            deny("ZABLOKOWANE: w zaindeksowanych zmianach jest materiał sekretny: "
                 + ", ".join(hits)
                 + "\nUsuń go z indeksu. Świadome obejście: MURMUR_ALLOW_SECRET=1.")

    if re.search(r"\bgit\b[^|;&]*\bpush\b", cmd):
        # `git push [flagi] [remote] [refspec]` — jawny refspec wygrywa nad HEAD.
        tail = cmd.split("push", 1)[1].split("|")[0].split(";")[0].split("&")[0]
        args = [t for t in tail.split() if not t.startswith("-")]
        if len(args) >= 2:                      # remote + refspec: liczy sie strona docelowa
            target = args[1].split(":")[-1].split("/")[-1]
        else:                                   # bez refspecu idzie biezacy branch
            target = git("rev-parse", "--abbrev-ref", "HEAD").strip()
        if target == PROTECTED_BRANCH:
            deny(f"ZABLOKOWANE: bezpośredni push na `{PROTECTED_BRANCH}`. "
                 "Zrób branch i PR — CI jest jedyną władzą mergującą.")

    if re.search(r"\b(cargo (test|build|clippy|check)|ng build|npm run test:e2e)\b", cmd) \
            and "agent-resource-run" not in cmd:
        print("uwaga: ciężka komenda poza pasem zasobów; przy równoległych worktree "
              "użyj `scripts/agent-resource-run -- …`", file=sys.stderr)


if __name__ == "__main__":
    main()
