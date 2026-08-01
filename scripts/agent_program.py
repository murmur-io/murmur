#!/usr/bin/env python3
"""Run a manifest of Murmur tasks through headless agent sessions, one at a time.

This replaces a sentence. `.claude/rules/agentic-workflow.md` used to tell a multi-task program to
"use a session-scoped `/goal` that requires the next manifest action within 60 seconds after the
prior stable outcome" — an instruction to the model to remember to keep going, with nothing to
enforce it. Prose is the wrong layer for a scheduler: it is unobservable, it cannot resume, and
when it fails it fails silently, which is exactly how a declared task list gets abandoned
mid-program.

What this is NOT: an orchestrator. It does not plan, it does not decide what a task means, and it
does not verify anything. Each entry is handed to one headless session, which does the work under
the repo's ordinary rules; the gate then decides whether the program continues. The verifier-only
Harness stays the authority on whether a change is sound (`scripts/agent-harness`), and the CI gate
stays the only merge authority. This file only answers "what runs next, and did the last thing
hold?".

SEQUENTIAL BY DESIGN. Murmur's binding rule is one Cargo/rustc process machine-wide
(`.claude/rules/agentic-workflow.md`, "One shared resource lane"), and two agent sessions editing
one checkout produce conflicts nobody asked for. Parallelism belongs to the reviewers inside a
task, not to the tasks.

STOPS ON FIRST RED, and writes its state. A program that keeps going past a failing gate is
manufacturing work on a broken base; a human resumes with `--resume` after fixing it.

Manifest (JSON list):

    [
      {"id": "fix-foo", "prompt": "Fix the foo regression in ...", "gate": "quick"},
      {"id": "doc-bar", "prompt": "Document ...",                  "gate": "audit"}
    ]

`gate` selects what must be green after the entry (default `quick`):
  audit  — scripts/agent-config-audit --ci               (0.1s; the cheapest, always worth it)
  quick  — audit + the harness + hook selftests + the scaffold-eval graders
  full   — bash scripts/ci.sh, through the shared resource lane

Usage:
    scripts/agent-program run manifest.json [--agent 'claude -p --permission-mode acceptEdits']
    scripts/agent-program run manifest.json --dry-run
    scripts/agent-program resume manifest.json
    scripts/agent-program status
"""

from __future__ import annotations

import argparse
import json
import shlex
import subprocess
import sys
import time
from pathlib import Path
from typing import Any, Dict, List, Optional, Sequence

REPO = Path(__file__).resolve().parents[1]


def _git_common_dir() -> Path:
    """The repository's shared git directory, which is NOT `<repo>/.git` in a worktree.

    In a linked worktree `.git` is a FILE containing a gitdir pointer, so writing state under it
    fails outright. Asking git keeps one program state for the whole checkout family, which is what
    a resume needs: the program is about the repository, not about whichever worktree launched it.
    """

    try:
        out = subprocess.run(
            ["git", "rev-parse", "--git-common-dir"],
            cwd=str(REPO), capture_output=True, text=True, check=True,
        ).stdout.strip()
    except (OSError, subprocess.CalledProcessError) as error:
        raise ProgramError(f"not a git repository: {REPO} ({error})") from error
    common = Path(out)
    return common if common.is_absolute() else (REPO / common).resolve()


def state_path() -> Path:
    return _git_common_dir() / "agent-program" / "state.json"

DEFAULT_AGENT = "claude -p --permission-mode acceptEdits"

GATES: Dict[str, List[List[str]]] = {
    "audit": [
        ["scripts/agent-config-audit", "--ci"],
    ],
    "quick": [
        ["scripts/agent-config-audit", "--ci"],
        ["scripts/agent-harness", "selftest", "--ci"],
        ["bash", ".claude/hooks/selftest.sh"],
        ["bash", ".codex/hooks/selftest.sh"],
        ["python3", "eval/agents/runner.py", "--mode", "fake"],
    ],
    # `ci.sh` compiles the always-on ML tree, so it MUST go through the shared lane or it will
    # collide with any other Cargo process on this machine.
    "full": [
        ["scripts/agent-resource-run", "--", "bash", "scripts/ci.sh"],
    ],
}


class ProgramError(RuntimeError):
    """A failure in the program driver itself, not in the work it dispatches."""


def utc_now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def load_manifest(path: Path) -> List[Dict[str, Any]]:
    try:
        entries = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as error:
        raise ProgramError(f"cannot read manifest {path}: {error}") from error
    if not isinstance(entries, list) or not entries:
        raise ProgramError("manifest must be a non-empty JSON list")

    seen = set()
    for index, entry in enumerate(entries):
        if not isinstance(entry, dict):
            raise ProgramError(f"manifest entry {index} is not an object")
        for field in ("id", "prompt"):
            value = entry.get(field)
            if not isinstance(value, str) or not value.strip():
                raise ProgramError(f"manifest entry {index} needs a non-empty {field!r}")
        gate = entry.get("gate", "quick")
        if gate not in GATES:
            raise ProgramError(
                f"manifest entry {entry['id']!r} has unknown gate {gate!r}; "
                f"choose one of {', '.join(sorted(GATES))}"
            )
        if entry["id"] in seen:
            # Ids key the resume state, so duplicates would silently skip work.
            raise ProgramError(f"duplicate manifest id {entry['id']!r}")
        seen.add(entry["id"])
    return entries


def read_state() -> Dict[str, Any]:
    path = state_path()
    if not path.exists():
        return {"completed": [], "failed": None, "history": []}
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        # An unreadable state file must not silently re-run a whole program.
        raise ProgramError(f"state file is unreadable: {path}")


def write_state(state: Dict[str, Any]) -> None:
    path = state_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    tmp.write_text(json.dumps(state, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(path)


def run_gate(name: str, dry_run: bool) -> Optional[str]:
    """Run a gate's commands in order. Returns None when green, else a failure description."""

    for command in GATES[name]:
        if dry_run:
            print(f"    [dry-run] {' '.join(command)}")
            continue
        print(f"    $ {' '.join(command)}")
        completed = subprocess.run(command, cwd=str(REPO), check=False)
        if completed.returncode != 0:
            return f"gate {name!r} failed at: {' '.join(command)} (exit {completed.returncode})"
    return None


def run_entry(entry: Dict[str, Any], agent: Sequence[str], dry_run: bool) -> Optional[str]:
    """Dispatch one manifest entry to a headless session. Returns None on success."""

    if dry_run:
        print(f"    [dry-run] {' '.join(agent)} <prompt: {entry['prompt'][:60]}...>")
        return None

    argv = list(agent) + [entry["prompt"]]
    try:
        completed = subprocess.run(
            argv,
            cwd=str(REPO),
            check=False,
            # A headless session that blocks on a TTY read is a hung program, not a task in
            # progress: there is nobody to answer it.
            stdin=subprocess.DEVNULL,
        )
    except FileNotFoundError as missing:
        raise ProgramError(f"agent command not found: {argv[0]} ({missing})") from missing
    if completed.returncode != 0:
        return f"agent exited {completed.returncode}"
    return None


def command_run(args: argparse.Namespace, resume: bool) -> int:
    manifest = load_manifest(Path(args.manifest))
    agent = shlex.split(args.agent)
    state = read_state() if resume else {"completed": [], "failed": None, "history": []}
    done = set(state.get("completed", []))

    if resume and state.get("failed"):
        print(f"resuming after failure on {state['failed']['id']!r}", file=sys.stderr)

    for entry in manifest:
        if entry["id"] in done:
            print(f"-- {entry['id']}: already complete, skipping")
            continue

        print(f"== {entry['id']}")
        started = utc_now()
        failure = run_entry(entry, agent, args.dry_run)
        if failure is None:
            failure = run_gate(entry.get("gate", "quick"), args.dry_run)

        record = {
            "id": entry["id"],
            "gate": entry.get("gate", "quick"),
            "started_utc": started,
            "finished_utc": utc_now(),
            "failure": failure,
        }
        state.setdefault("history", []).append(record)

        if failure is not None:
            state["failed"] = record
            if not args.dry_run:
                write_state(state)
            print(f"\nSTOPPED on {entry['id']!r}: {failure}", file=sys.stderr)
            print(
                "Fix it, then re-run with `resume`. The program does not continue past a red gate:\n"
                "every later entry would be built on a base that is known broken.",
                file=sys.stderr,
            )
            return 1

        state["completed"] = sorted(done | {entry["id"]})
        done.add(entry["id"])
        state["failed"] = None
        if not args.dry_run:
            write_state(state)
        print(f"-- {entry['id']}: green\n")

    print(f"program complete: {len(done)} entr{'y' if len(done) == 1 else 'ies'}")
    return 0


def command_status(_args: argparse.Namespace) -> int:
    state = read_state()
    completed = state.get("completed", [])
    print(f"completed: {len(completed)}")
    for entry_id in completed:
        print(f"  + {entry_id}")
    failed = state.get("failed")
    if failed:
        print(f"failed: {failed['id']} — {failed['failure']}")
    return 0


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)

    for name in ("run", "resume"):
        node = sub.add_parser(name)
        node.add_argument("manifest")
        node.add_argument("--agent", default=DEFAULT_AGENT, help=f"headless CLI (default: {DEFAULT_AGENT!r})")
        node.add_argument("--dry-run", action="store_true", help="print the plan without dispatching or gating")

    sub.add_parser("status")

    args = parser.parse_args(argv)
    try:
        if args.command == "run":
            return command_run(args, resume=False)
        if args.command == "resume":
            return command_run(args, resume=True)
        return command_status(args)
    except ProgramError as error:
        print(f"agent-program: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
