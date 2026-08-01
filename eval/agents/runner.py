#!/usr/bin/env python3
"""Scaffold eval for Murmur's development agents.

This measures the DEVELOPMENT ENVELOPE — prompts, skills, rule files, tool definitions — not the
product and not a model. Editing `.claude/rules/angular-zoneless.md` is engineering; without
something like this, "the rule helps" is an unfalsifiable claim.

# Why these five tasks

The suite recovered from `ac496e6` held eleven bug classes. Six of them (`hook-git-option-bypass`,
`stale-receipt-hash`, `pass-with-failing-check`, `playwright-isolated-port`, `safe-pid-ownership`,
`out-of-scope-attempt`) test DETERMINISTIC control-plane logic, and that logic already shipped as
production code with its own selftests — re-testing it through a live model would be slower, more
expensive and less reliable than the tests that already exist.

The five kept here are the ones that cannot be tested any other way, because each measures whether
the SCAFFOLD steers the agent away from a plausible wrong answer:

  angular22-noop              is `angular-zoneless.md` T1 landing? The bad answer — "Added
                              allowSignalWrites to silence NG0600" — is exactly what a model
                              trained on Angular 18 produces unprompted. `expected_change: false`.
  lock-masked-dto             the bad answer masks the note body but keeps `audio_path`, which is
                              the real `convertFileSrc` leak the lock model exists to prevent.
  seal-verify-before-destroy  does the agent prove the ciphertext decrypts BEFORE blanking?
  secret-sk-proj              does the scanner catch both token forms without flagging placeholders?
  analysis-only               will the agent REFUSE to edit and report instead? (`allowed_paths: []`)

# Two modes

`--mode fake` (default) runs no model at all. It replays each task's recorded `good`/`bad` overlays
through the real grader and asserts good passes and bad fails. That is the control: it proves the
graders still have teeth. It is fast, free and deterministic, so it can run in CI.

`--mode agent` invokes a real CLI and grades what it produces. That is the actual measurement, and
it costs live model calls — run it when a rule, skill or reviewer prompt changes, not per commit.
"""

from __future__ import annotations

import argparse
import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

ROOT = Path(__file__).resolve().parent
TASKS = ROOT / "tasks"
GRADER = ROOT / "graders" / "smoke.py"


def load_tasks(only: Optional[List[str]]) -> List[Dict[str, Any]]:
    tasks = [json.loads(p.read_text(encoding="utf-8")) for p in sorted(TASKS.glob("*.json"))]
    if only:
        wanted = set(only)
        tasks = [t for t in tasks if t["task_id"] in wanted]
        missing = wanted - {t["task_id"] for t in tasks}
        if missing:
            raise SystemExit(f"unknown task(s): {', '.join(sorted(missing))}")
    return tasks


def materialize(task: Dict[str, Any], overlay: Optional[str], into: Path) -> Path:
    """Lay down the task's `initial/` tree, then apply an overlay on top of it."""
    workspace = into / task["task_id"]
    shutil.copytree(ROOT / task["source"]["initial"], workspace)
    if overlay:
        overlay_root = ROOT / overlay
        for source in overlay_root.rglob("*"):
            if source.is_file():
                target = workspace / source.relative_to(overlay_root)
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source, target)
    return workspace


def grade(task_id: str, workspace: Path, response_text: str) -> Tuple[bool, str]:
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as handle:
        json.dump({"response_text": response_text}, handle)
        context = Path(handle.name)
    try:
        completed = subprocess.run(
            [sys.executable, str(GRADER), "--task", task_id,
             "--workspace", str(workspace), "--context", str(context)],
            capture_output=True, text=True, timeout=120, check=False,
        )
    finally:
        context.unlink(missing_ok=True)
    if completed.returncode != 0:
        return False, f"grader failed: {completed.stderr.strip()[:200]}"
    verdict = json.loads(completed.stdout)
    return bool(verdict["pass"]), str(verdict["message"])


def run_fake(task: Dict[str, Any], workdir: Path) -> List[Tuple[str, bool, str]]:
    """Assert the grader accepts the recorded good answer and rejects the recorded bad one."""
    fake = task.get("fake") or {}
    results: List[Tuple[str, bool, str]] = []
    for arm, overlay_key, response_key, must_pass in (
        ("good", "good_overlay", "good_response", True),
        ("bad", "bad_overlay", "bad_response", False),
    ):
        # A task with `expected_change: false` has no good_overlay on purpose: its correct answer
        # is to leave `initial/` untouched and explain why.
        overlay = fake.get(overlay_key)
        if arm == "bad" and overlay is None and not fake.get(response_key):
            continue
        workspace = materialize(task, overlay, workdir / arm)
        passed, message = grade(task["task_id"], workspace, fake.get(response_key, ""))
        ok = passed is must_pass
        results.append((arm, ok, message if ok else f"expected {'PASS' if must_pass else 'FAIL'}: {message}"))
    return results


def run_agent(task: Dict[str, Any], workdir: Path, command: List[str]) -> List[Tuple[str, bool, str]]:
    """Hand `initial/` and the task prompt to a real CLI, then grade whatever it produced."""
    workspace = materialize(task, None, workdir / "agent")
    completed = subprocess.run(
        command + [task["prompt"]], cwd=str(workspace),
        capture_output=True, text=True, timeout=900, check=False,
    )
    passed, message = grade(task["task_id"], workspace, completed.stdout)
    return [("agent", passed, message)]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=("fake", "agent"), default="fake")
    parser.add_argument("--task", action="append", help="run only this task id (repeatable)")
    parser.add_argument("--agent-command", help="CLI to invoke in --mode agent, e.g. 'claude -p'")
    args = parser.parse_args()

    if args.mode == "agent" and not args.agent_command:
        raise SystemExit("--mode agent requires --agent-command")

    tasks = load_tasks(args.task)
    failures = 0
    with tempfile.TemporaryDirectory(prefix="murmur-scaffold-eval-") as tmp:
        workdir = Path(tmp)
        for task in tasks:
            if args.mode == "fake":
                results = run_fake(task, workdir / task["task_id"])
            else:
                results = run_agent(task, workdir / task["task_id"], args.agent_command.split())
            for arm, ok, message in results:
                failures += 0 if ok else 1
                print(f"{'PASS' if ok else 'FAIL'}  {task['task_id']:<28} [{arm}]  {message}")

    print(f"\n{len(tasks)} task(s), {failures} failure(s)")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
