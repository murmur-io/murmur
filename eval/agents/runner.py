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

# The scaffold arms (`--scaffold`)

Until 2026-08-01 `--mode agent` measured a BARE MODEL, not the envelope. `materialize` copied only
`fixtures/<task>/initial/` into a temp directory and the CLI ran there, so the agent never saw
`CLAUDE.md`, `AGENTS.md` or any `.claude/rules/*.md`. The rule under test was absent from BOTH
arms, which made the suite's own thesis — "editing `angular-zoneless.md` is engineering, not
vibes" — untestable by construction.

`--scaffold` fixes that by making the envelope the independent variable:

  none    the bare fixture, exactly as before. This is the CONTROL arm.
  rules   the same fixture PLUS the scaffold files the task declares in `scaffold_files`, copied
          from the repo root at their real repo-relative paths, plus a generated `CLAUDE.md` /
          `AGENTS.md` that declares them binding (the repo's real `CLAUDE.md` reaches its rules the
          same way, through `@.claude/rules/*.md` imports — the generated loader is an ABLATION of
          that mechanism, not new advice: it names files, never answers).

A declared file that is missing on disk is a hard error. A silently-absent scaffold file would
make the treatment arm secretly identical to the control arm — a green measurement that means
nothing — so `scaffold_files` fails loudly instead. `--selftest` asserts the two arms really do
differ, byte for byte, on every task that declares a file.
"""

from __future__ import annotations

import argparse
import json
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path, PurePosixPath
from typing import Any, Dict, List, Optional, Sequence, Tuple

ROOT = Path(__file__).resolve().parent
TASKS = ROOT / "tasks"
GRADER = ROOT / "graders" / "smoke.py"
# `eval/agents/` -> `eval/` -> the worktree root that owns CLAUDE.md and .claude/rules/.
REPO_ROOT = ROOT.parent.parent

SCAFFOLD_ARMS = ("none", "rules")

# Entry points an agent CLI reads on its own from the working directory. Generated ONLY in the
# `rules` arm, and only when the task declares at least one scaffold file. Claude Code follows the
# `@path` import; Codex reads AGENTS.md verbatim and opens the listed paths with its own tools.
LOADER_FILES = ("CLAUDE.md", "AGENTS.md")
LOADER_HEADER = (
    "# Binding project instructions\n"
    "\n"
    "The rule file(s) below are BINDING for this repository. They are not style preferences.\n"
    "Read them and follow them before you act.\n"
    "\n"
)


def load_tasks(only: Optional[List[str]]) -> List[Dict[str, Any]]:
    tasks = [json.loads(p.read_text(encoding="utf-8")) for p in sorted(TASKS.glob("*.json"))]
    if only:
        wanted = set(only)
        tasks = [t for t in tasks if t["task_id"] in wanted]
        missing = wanted - {t["task_id"] for t in tasks}
        if missing:
            raise SystemExit(f"unknown task(s): {', '.join(sorted(missing))}")
    return tasks


def scaffold_sources(task: Dict[str, Any]) -> List[Tuple[str, Path]]:
    """Resolve `scaffold_files` against the repo root, failing loudly on anything unusable."""
    declared = task.get("scaffold_files")
    if declared is None:
        return []
    task_id = task.get("task_id", "<unknown>")
    if not isinstance(declared, list):
        raise SystemExit(f"{task_id}: scaffold_files must be a list of repo-relative paths")
    resolved: List[Tuple[str, Path]] = []
    for entry in declared:
        if not isinstance(entry, str) or not entry.strip():
            raise SystemExit(f"{task_id}: scaffold_files entries must be non-empty strings")
        relative = PurePosixPath(entry)
        if relative.is_absolute() or ".." in relative.parts:
            raise SystemExit(
                f"{task_id}: scaffold_files entry must be repo-relative without '..': {entry}"
            )
        source = REPO_ROOT / relative
        if not source.is_file():
            # The worst failure mode of this whole design: a treatment arm that is secretly the
            # control arm. Never degrade to a warning.
            raise SystemExit(
                f"{task_id}: declared scaffold file is missing: {source}\n"
                "  the 'rules' arm would be byte-identical to the 'none' arm — refusing to run"
            )
        resolved.append((str(relative), source))
    return resolved


def inject_scaffold(task: Dict[str, Any], workspace: Path) -> List[str]:
    """Copy the task's declared scaffold files into `workspace` at their repo-relative paths."""
    injected: List[str] = []
    for relative, source in scaffold_sources(task):
        target = workspace / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
        injected.append(relative)
    if not injected:
        return injected
    declared = list(injected)  # the loader lists the DECLARED files only, never another loader
    for name in LOADER_FILES:
        if name in declared or (workspace / name).exists():
            continue  # a task that declares the real CLAUDE.md keeps the real bytes
        body = "".join(
            (f"@{relative}\n" if name == "CLAUDE.md" else f"- {relative}\n") for relative in declared
        )
        (workspace / name).write_text(LOADER_HEADER + body, encoding="utf-8")
        injected.append(name)
    return injected


def materialize(
    task: Dict[str, Any], overlay: Optional[str], into: Path, scaffold: str = "none"
) -> Path:
    """Lay down the task's `initial/` tree, then apply an overlay and the scaffold arm on top."""
    workspace = into / task["task_id"]
    shutil.copytree(ROOT / task["source"]["initial"], workspace)
    if overlay:
        overlay_root = ROOT / overlay
        for source in overlay_root.rglob("*"):
            if source.is_file():
                target = workspace / source.relative_to(overlay_root)
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source, target)
    if scaffold == "rules":
        inject_scaffold(task, workspace)
    elif scaffold != "none":
        raise SystemExit(f"unknown scaffold arm: {scaffold}")
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


def run_agent(
    task: Dict[str, Any],
    workdir: Path,
    command: Sequence[str],
    scaffold: str = "none",
    run_index: int = 0,
    agent_label: str = "agent",
    timeout_seconds: float = 900.0,
) -> Dict[str, Any]:
    """Hand `initial/` (+ the scaffold arm) and the prompt to a real CLI, then grade the result."""
    workspace = materialize(task, None, workdir / f"{scaffold}-{run_index}", scaffold=scaffold)
    argv = list(command) + [task["prompt"]]
    started = time.monotonic()
    timed_out = False
    try:
        completed = subprocess.run(
            argv, cwd=str(workspace),
            capture_output=True, text=True, timeout=timeout_seconds, check=False,
            # The agent must not be able to prompt for input: a headless eval that blocks on a
            # TTY read is a hung matrix, not a measurement.
            stdin=subprocess.DEVNULL,
        )
        stdout, exit_code = completed.stdout, completed.returncode
    except subprocess.TimeoutExpired as expired:
        timed_out = True
        stdout = expired.stdout.decode("utf-8", "replace") if isinstance(expired.stdout, bytes) else (expired.stdout or "")
        exit_code = None
    except FileNotFoundError as missing:
        raise SystemExit(f"agent command not found: {argv[0]} ({missing})") from missing
    seconds = round(time.monotonic() - started, 3)
    if timed_out:
        passed, message = False, f"agent timed out after {timeout_seconds:.0f}s"
    else:
        passed, message = grade(task["task_id"], workspace, stdout)
    return {
        "task_id": task["task_id"],
        "agent_label": agent_label,
        "scaffold": scaffold,
        "run_index": run_index,
        "arm": "agent",
        "passed": passed,
        "message": message,
        "seconds": seconds,
        "exit_code": exit_code,
        "timed_out": timed_out,
        "command": " ".join(command),
    }


def write_json(path: Path, records: List[Dict[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(records, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def tree_bytes(root: Path) -> Dict[str, bytes]:
    return {
        str(path.relative_to(root)): path.read_bytes()
        for path in sorted(root.rglob("*"))
        if path.is_file()
    }


def selftest() -> int:
    """Prove the scaffold arms differ. This is what stops the feature from silently rotting.

    Deterministic, no model calls: materialize every task in both arms and assert each declared
    scaffold file is ABSENT under `none` and PRESENT with byte-identical content under `rules`.
    """
    failures = 0
    tasks = load_tasks(None)
    with tempfile.TemporaryDirectory(prefix="murmur-scaffold-selftest-") as tmp:
        workdir = Path(tmp)
        for task in tasks:
            task_id = task["task_id"]
            declared = scaffold_sources(task)
            control = tree_bytes(materialize(task, None, workdir / task_id / "none", scaffold="none"))
            treated = tree_bytes(materialize(task, None, workdir / task_id / "rules", scaffold="rules"))
            problems: List[str] = []
            for relative, source in declared:
                if relative in control:
                    problems.append(f"{relative} leaked into the control arm")
                if relative not in treated:
                    problems.append(f"{relative} missing from the rules arm")
                elif treated[relative] != source.read_bytes():
                    problems.append(f"{relative} differs from {source}")
            if declared:
                expected_listing = [relative for relative, _ in declared]
                for name in LOADER_FILES:
                    if name in control:
                        problems.append(f"{name} leaked into the control arm")
                    if name not in treated:
                        problems.append(f"{name} loader missing from the rules arm")
                        continue
                    body = treated[name].decode("utf-8")
                    listed = [line.lstrip("@- ").strip() for line in body.splitlines()
                              if line.startswith(("@", "- "))]
                    if listed != expected_listing:
                        # The loader must name the declared files and nothing else — a loader that
                        # lists another loader is noise the agent has to resolve.
                        problems.append(f"{name} lists {listed}, expected {expected_listing}")
                if treated == control:
                    problems.append("rules arm is byte-identical to the control arm")
                for relative, payload in control.items():
                    if treated.get(relative) != payload:
                        problems.append(f"scaffold mutated the fixture file {relative}")
            elif treated != control:
                problems.append("no scaffold declared, yet the arms differ")
            declared_note = ", ".join(relative for relative, _ in declared) or "none declared"
            if problems:
                failures += 1
                print(f"FAIL  {task_id:<28} [selftest]  {'; '.join(problems)}")
            else:
                print(f"PASS  {task_id:<28} [selftest]  {declared_note}")

        # A declared-but-absent scaffold file MUST abort, never degrade to a silent no-op.
        bogus = {"task_id": "selftest-missing-file", "scaffold_files": [".claude/rules/does-not-exist.md"]}
        try:
            scaffold_sources(bogus)
        except SystemExit:
            print(f"PASS  {'missing-file-guard':<28} [selftest]  a missing scaffold file aborts the run")
        else:
            failures += 1
            print(f"FAIL  {'missing-file-guard':<28} [selftest]  a missing scaffold file was tolerated")

    print(f"\n{len(tasks)} task(s), {failures} failure(s)")
    return 1 if failures else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=("fake", "agent"), default="fake")
    parser.add_argument("--task", action="append", help="run only this task id (repeatable)")
    parser.add_argument("--agent-command", help="CLI to invoke in --mode agent, e.g. 'claude -p'")
    parser.add_argument("--agent-label", help="label recorded in --json (default: the command's argv[0])")
    parser.add_argument("--scaffold", choices=SCAFFOLD_ARMS, default="none",
                        help="'none' = bare fixture (control); 'rules' = fixture + declared scaffold files")
    parser.add_argument("--repeat", type=int, default=1, help="runs per task in --mode agent (default 1)")
    parser.add_argument("--json", dest="json_path", type=Path, help="write one JSON record per run")
    parser.add_argument("--timeout", type=float, default=900.0, help="per-run agent wall timeout in seconds")
    parser.add_argument("--selftest", action="store_true",
                        help="assert the scaffold arms differ (deterministic, no model calls)")
    args = parser.parse_args()

    if args.selftest:
        return selftest()

    if args.mode == "agent" and not args.agent_command:
        raise SystemExit("--mode agent requires --agent-command")
    if args.repeat < 1:
        raise SystemExit("--repeat must be >= 1")

    tasks = load_tasks(args.task)
    command = shlex.split(args.agent_command) if args.agent_command else []
    label = args.agent_label or (command[0] if command else "fake")
    records: List[Dict[str, Any]] = []
    failures = 0
    with tempfile.TemporaryDirectory(prefix="murmur-scaffold-eval-") as tmp:
        workdir = Path(tmp)
        for task in tasks:
            if args.mode == "fake":
                for arm, ok, message in run_fake(task, workdir / task["task_id"]):
                    failures += 0 if ok else 1
                    print(f"{'PASS' if ok else 'FAIL'}  {task['task_id']:<28} [{arm}]  {message}")
                    records.append({
                        "task_id": task["task_id"], "agent_label": label, "scaffold": "none",
                        "run_index": 0, "arm": arm, "passed": ok, "message": message,
                        "seconds": 0.0, "exit_code": 0, "timed_out": False, "command": "",
                    })
                continue

            if args.scaffold == "rules" and not scaffold_sources(task):
                print(f"note: {task['task_id']} declares no scaffold files — "
                      "its 'rules' arm is identical to its 'none' arm", file=sys.stderr)
            passes = 0
            for index in range(args.repeat):
                record = run_agent(
                    task, workdir / task["task_id"], command,
                    scaffold=args.scaffold, run_index=index, agent_label=label,
                    timeout_seconds=args.timeout,
                )
                records.append(record)
                passes += 1 if record["passed"] else 0
                failures += 0 if record["passed"] else 1
                arm = f"agent/{args.scaffold}"
                if args.repeat > 1:
                    arm = f"{arm} {index + 1}/{args.repeat}"
                verdict = "PASS" if record["passed"] else "FAIL"
                print(f"{verdict}  {task['task_id']:<28} [{arm}]  {record['message']}")
            if args.repeat > 1:
                print(f"      {task['task_id']:<28} [agent/{args.scaffold}]  {passes}/{args.repeat} passed")

    if args.json_path:
        write_json(args.json_path, records)

    print(f"\n{len(tasks)} task(s), {failures} failure(s)")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
