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
          that mechanism, not new advice: it names files, never answers). This is the FOCUSED arm:
          an oracle picked the per-task subset, so it answers "does THIS rule carry the effect?",
          not "does production behave this way?".
  full    the repo's REAL always-on envelope: the actual `CLAUDE.md`, the actual `AGENTS.md` and
          EVERY `.claude/rules/*.md`, at their real repo-relative paths, with no generated loader
          and no per-task selection. Production never loads a subset, so `rules` cannot answer
          "will this help a real session"; `full` is the arm whose result transfers.

A declared file that is missing on disk is a hard error. A silently-absent scaffold file would
make the treatment arm secretly identical to the control arm — a green measurement that means
nothing — so `scaffold_files` fails loudly instead. `--selftest` asserts the arms really do
differ, byte for byte, and that `full` materializes the whole envelope.

# What gets graded: the agent's words, not its transcript

A CLI's stdout is a TRANSCRIPT — the instructions echoed back, the files it opened, its tool calls,
and somewhere inside, the answer. Grading all of it scored VERBOSITY: the `analysis-only` prompt
contains "exposes" (an impact keyword its grader looks for) and the `angular22-noop` prompt
contains `allowSignalWrites` (the identifier its grader demands), so an agent that echoes its
instructions was handed both for free while a terse agent was not. How much a CLI echoes is a
property of the VENDOR, so the vendor comparison was partly a comparison of transcript style.
`agent_own_words` therefore removes everything the agent could have COPIED — every line and
sentence of the prompt and of the workspace it was handed, snapshotted BEFORE the run so files the
agent writes stay its own — plus fenced code blocks and quoted lines. It names no CLI and looks for
no vendor-specific "final answer" marker, because either would be the asymmetry it exists to remove.

# Outcome status: PASS / FAIL / ERROR

A rate-limited 429, a non-zero CLI exit, an empty stdout, a timeout, or a grader that itself
crashed are TRANSPORT failures, not behavioural ones. Scoring them as "the agent got it wrong"
silently deflates whichever arm happened to run while the API was unhappy. Every run therefore
carries a `status`:

  PASS   the grader ran and accepted the result
  FAIL   the grader ran and rejected it — this is a real behavioural datum
  ERROR  the run never produced a gradeable result; EXCLUDED from pass counts and reported
         separately. An arm with a non-trivial ERROR rate is flagged NOT COMPARABLE.

# Ordering

`matrix.py` interleaves the arms within each (agent, task, repeat) cell and picks the within-cell
order from a RECORDED seed (`--seed`, deterministic, never an unseeded RNG). Running every `none`
first and every `rules` afterwards would fold a mid-matrix model point-release, a warming cache or
progressive rate-limiting entirely into one arm.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import random
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path, PurePosixPath
from typing import Any, Dict, List, Optional, Sequence, Set, Tuple

ROOT = Path(__file__).resolve().parent
TASKS = ROOT / "tasks"
GRADER = ROOT / "graders" / "smoke.py"
# `eval/agents/` -> `eval/` -> the worktree root that owns CLAUDE.md and .claude/rules/.
REPO_ROOT = ROOT.parent.parent

SCAFFOLD_ARMS = ("none", "rules", "full")

STATUS_PASS = "PASS"
STATUS_FAIL = "FAIL"
STATUS_ERROR = "ERROR"

# An arm that lost this share of its runs to transport failures is not comparable to another arm.
NOT_COMPARABLE_ERROR_RATE = 0.10

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

# The real always-on envelope, in load order. `full` copies exactly these; nothing is generated.
FULL_ENVELOPE_ROOT_FILES = ("CLAUDE.md", "AGENTS.md")
FULL_ENVELOPE_RULES_DIR = PurePosixPath(".claude/rules")

# Strings a CLI emits when the TRANSPORT failed rather than the agent. Deliberately narrow: these
# are API error shapes, not words a normal answer uses, so a correct response cannot trip them.
TRANSPORT_MARKERS = (
    "rate_limit_error",
    "overloaded_error",
    "429 too many requests",
    "usage limit reached",
    "quota exceeded",
    "insufficient_quota",
    "internal_server_error",
    "api error: 5",
)

_CLI_VERSIONS: Dict[str, Optional[str]] = {}
_REPO_PROVENANCE: Optional[Dict[str, Any]] = None


def utc_now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


def repo_provenance() -> Dict[str, Any]:
    """git SHA + dirty flag for the worktree under measurement. Best effort, never fatal."""
    global _REPO_PROVENANCE
    if _REPO_PROVENANCE is None:
        def git(*arguments: str) -> Optional[str]:
            try:
                done = subprocess.run(["git", "-C", str(REPO_ROOT), *arguments],
                                      capture_output=True, text=True, timeout=15, check=False)
            except (OSError, subprocess.SubprocessError):
                return None
            return done.stdout.strip() if done.returncode == 0 else None

        sha = git("rev-parse", "HEAD")
        status = git("status", "--porcelain")
        _REPO_PROVENANCE = {"repo_sha": sha, "repo_dirty": None if status is None else bool(status)}
    return dict(_REPO_PROVENANCE)


def cli_version(command: Sequence[str]) -> Optional[str]:
    """`<argv0> --version`, cached. A CLI that has no --version simply records None."""
    if not command:
        return None
    key = command[0]
    if key not in _CLI_VERSIONS:
        try:
            done = subprocess.run([key, "--version"], capture_output=True, text=True,
                                  timeout=30, check=False, stdin=subprocess.DEVNULL)
            text = (done.stdout or done.stderr).strip().splitlines()
            _CLI_VERSIONS[key] = text[0][:200] if done.returncode == 0 and text else None
        except (OSError, subprocess.SubprocessError):
            _CLI_VERSIONS[key] = None
    return _CLI_VERSIONS[key]


def declared_model(command: Sequence[str]) -> Optional[str]:
    """Best effort: the model is only knowable when the operator named it on the command line."""
    argv = list(command)
    for index, token in enumerate(argv):
        if token in ("--model", "-m") and index + 1 < len(argv):
            return argv[index + 1]
        if token.startswith("--model="):
            return token.split("=", 1)[1]
    return os.environ.get("ANTHROPIC_MODEL") or None


def guard_workspace_root(workdir: Path) -> None:
    """Refuse a scratch root that would leak a real scaffold into BOTH arms.

    Agent CLIs discover `CLAUDE.md` / `AGENTS.md` by walking UP from the working directory. A
    TMPDIR inside this repo (or under any other directory holding those files) would hand the real
    envelope to the control arm too, so the measured delta would collapse to zero for a reason that
    has nothing to do with the scaffold. Detect it and refuse — never warn and continue.
    """
    resolved = workdir.resolve()
    repo = REPO_ROOT.resolve()
    if resolved == repo or repo in resolved.parents:
        raise SystemExit(
            f"refusing to run: the scratch root {resolved} is inside the repo {repo}\n"
            "  an agent CLI walks UP from its working directory, so the repo's real CLAUDE.md\n"
            "  would contaminate the control arm as well. Point TMPDIR outside the repo."
        )
    home = Path.home().resolve()
    for ancestor in (resolved, *resolved.parents):
        if ancestor == home:
            # The user-level scaffold (~/.claude/CLAUDE.md and friends) is a documented constant
            # across arms — see the README's "Known limits". Everything below it is not.
            break
        for name in LOADER_FILES:
            if (ancestor / name).is_file():
                raise SystemExit(
                    f"refusing to run: {ancestor / name} sits above the scratch root {resolved}\n"
                    "  upward discovery would inject it into every arm. Point TMPDIR elsewhere."
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


def full_envelope_sources() -> List[Tuple[str, Path]]:
    """The production always-on envelope: CLAUDE.md + AGENTS.md + every `.claude/rules/*.md`."""
    resolved: List[Tuple[str, Path]] = []
    for name in FULL_ENVELOPE_ROOT_FILES:
        source = REPO_ROOT / name
        if not source.is_file():
            raise SystemExit(
                f"full arm: {source} is missing — the always-on envelope cannot be reproduced"
            )
        resolved.append((name, source))
    rules_dir = REPO_ROOT / FULL_ENVELOPE_RULES_DIR
    rules = sorted(rules_dir.glob("*.md")) if rules_dir.is_dir() else []
    if not rules:
        raise SystemExit(
            f"full arm: no rule files under {rules_dir} — the always-on envelope cannot be "
            "reproduced, and a 'full' arm without rules is secretly a second control arm"
        )
    resolved.extend((str(PurePosixPath(FULL_ENVELOPE_RULES_DIR) / rule.name), rule) for rule in rules)
    return resolved


def arm_sources(task: Dict[str, Any], scaffold: str) -> List[Tuple[str, Path]]:
    if scaffold == "none":
        return []
    if scaffold == "rules":
        return scaffold_sources(task)
    if scaffold == "full":
        return full_envelope_sources()
    raise SystemExit(f"unknown scaffold arm: {scaffold}")


def inject_scaffold(task: Dict[str, Any], workspace: Path, scaffold: str = "rules") -> List[str]:
    """Copy the arm's scaffold files into `workspace` at their repo-relative paths."""
    injected: List[str] = []
    for relative, source in arm_sources(task, scaffold):
        target = workspace / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, target)
        injected.append(relative)
    if scaffold != "rules" or not injected:
        # `full` ships the REAL CLAUDE.md/AGENTS.md; generating a loader on top would replace the
        # very artefact under measurement.
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
) -> Tuple[Path, List[str]]:
    """Lay down the task's `initial/` tree, then apply an overlay and the scaffold arm on top."""
    if scaffold not in SCAFFOLD_ARMS:
        raise SystemExit(f"unknown scaffold arm: {scaffold}")
    workspace = into / task["task_id"]
    shutil.copytree(ROOT / task["source"]["initial"], workspace)
    if overlay:
        overlay_root = ROOT / overlay
        for source in overlay_root.rglob("*"):
            if source.is_file():
                target = workspace / source.relative_to(overlay_root)
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source, target)
    injected = inject_scaffold(task, workspace, scaffold) if scaffold != "none" else []
    return workspace, injected


# --- what the agent AUTHORED, as opposed to what its CLI transcript happened to echo ---
#
# A CLI's stdout is a TRANSCRIPT: the instructions it was handed, the files it opened, its tool
# calls, and — somewhere inside — the answer. Grading the whole thing scores VERBOSITY. The
# `analysis-only` prompt contains the word "exposes", which is one of the grader's impact keywords;
# the `angular22-noop` prompt contains `allowSignalWrites`, which is the identifier its grader
# demands. An agent that echoes its instructions is handed both for free and an agent that answers
# tersely is not — and how much a CLI echoes is a property of the VENDOR, so the vendor comparison
# was partly measuring transcript style. Everything the agent could have COPIED goes before grading.

# Below this length a "fragment" is punctuation or a token, and matching on it would delete the
# agent's own short sentences whenever they collided with a fixture line.
ECHO_MIN_CHARS = 16
_FENCE = re.compile(r"^\s*(?:```|~~~)")
# A blockquote, a table row, or a line-numbered file dump is quotation, not argument.
_QUOTED = re.compile(r"^\s*(?:>|\||\d+\s*[|→])")
_SENTENCE_END = re.compile(r"(?<=[.!?])\s+")


def _normalized(text: str) -> str:
    return " ".join(text.split()).lower()


def echoable_fragments(prompt: str, workspace: Path) -> List[str]:
    """Every line and sentence the agent could REPRODUCE instead of author.

    The prompt it was given, plus every file it was given: the bare fixture in the control arm, the
    fixture PLUS the injected scaffold in the treatment arms. Deliberately asymmetric in the same
    direction as the arms themselves — the treatment arm has strictly more to quote, and that extra
    quotable material is exactly the free score being removed.

    Take this snapshot BEFORE the CLI runs. Files the agent WRITES are its own work, and a findings
    note it wrote and then printed must not be mistaken for material it echoed.
    """
    fragments: Set[str] = set()

    def collect(text: str) -> None:
        for line in text.splitlines():
            fragments.add(_normalized(line))
            for sentence in _SENTENCE_END.split(line):
                fragments.add(_normalized(sentence))

    collect(prompt)
    for path in sorted(workspace.rglob("*")):
        if not path.is_file():
            continue
        try:
            collect(path.read_text(encoding="utf-8"))
        except (OSError, UnicodeDecodeError):
            continue  # a binary fixture file is not something an agent quotes
    # Longest first, so a whole echoed sentence is removed before its sub-spans are hunted.
    return sorted((f for f in fragments if len(f) >= ECHO_MIN_CHARS), key=len, reverse=True)


def agent_own_words(stdout: str, fragments: Sequence[str]) -> str:
    """Reduce a CLI transcript to what the agent itself wrote.

    Vendor-neutral by construction: it names no CLI, hunts for no vendor-specific "final answer"
    delimiter, and removes only content that provably came from the prompt or the workspace. Two
    transcripts carrying the SAME reasoning therefore score the SAME however verbose they are —
    `--selftest`'s `transcript-echo` block pins that with a terse/verbose pair per prose task.
    """
    kept: List[str] = []
    fenced = False
    for raw in stdout.splitlines():
        if _FENCE.match(raw):
            fenced = not fenced
            continue
        if fenced or _QUOTED.match(raw):
            continue
        line = _normalized(raw)
        if not line:
            continue
        for fragment in fragments:
            if len(fragment) <= len(line) and fragment in line:
                # " . " and not "": an echoed span must not glue the agent's own words on either
                # side of it into a single claim the agent never made.
                line = line.replace(fragment, " . ")
        if line.strip(" ."):
            kept.append(line)
    return "\n".join(kept)


def grade(task_id: str, workspace: Path, response_text: str) -> Dict[str, Any]:
    """Run the hidden grader. `ran: False` means the GRADER broke — that is an ERROR, not a FAIL."""
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as handle:
        json.dump({"response_text": response_text}, handle)
        context = Path(handle.name)
    try:
        completed = subprocess.run(
            [sys.executable, str(GRADER), "--task", task_id,
             "--workspace", str(workspace), "--context", str(context)],
            capture_output=True, text=True, timeout=120, check=False,
        )
    except subprocess.TimeoutExpired:
        return {"ran": False, "passed": False, "message": "grader timed out", "details": {}}
    finally:
        context.unlink(missing_ok=True)
    if completed.returncode != 0:
        return {"ran": False, "passed": False,
                "message": f"grader failed: {completed.stderr.strip()[:200]}", "details": {}}
    try:
        verdict = json.loads(completed.stdout)
    except json.JSONDecodeError:
        return {"ran": False, "passed": False,
                "message": f"grader emitted non-JSON: {completed.stdout.strip()[:200]}",
                "details": {}}
    return {
        "ran": True,
        "passed": bool(verdict["pass"]),
        "message": str(verdict["message"]),
        "details": verdict.get("details") or {},
    }


def run_fake(task: Dict[str, Any], workdir: Path) -> List[Tuple[str, bool, str, Dict[str, Any]]]:
    """Assert the grader accepts the recorded good answer and rejects the recorded bad one."""
    fake = task.get("fake") or {}
    results: List[Tuple[str, bool, str, Dict[str, Any]]] = []
    for arm, overlay_key, response_key, must_pass in (
        ("good", "good_overlay", "good_response", True),
        ("bad", "bad_overlay", "bad_response", False),
    ):
        # A task with `expected_change: false` has no good_overlay on purpose: its correct answer
        # is to leave `initial/` untouched and explain why.
        overlay = fake.get(overlay_key)
        if arm == "bad" and overlay is None and not fake.get(response_key):
            continue
        workspace, _ = materialize(task, overlay, workdir / arm)
        # The same extraction the live path uses, so the CI control exercises it. The recorded
        # answers are already final messages, so it is a no-op on them — which is the point: a
        # filter that changed a recorded verdict would be rewriting the control.
        fragments = echoable_fragments(task["prompt"], workspace)
        verdict = grade(task["task_id"], workspace,
                        agent_own_words(fake.get(response_key, ""), fragments))
        ok = verdict["passed"] is must_pass and verdict["ran"]
        message = verdict["message"]
        results.append((arm, ok, message if ok else
                        f"expected {'PASS' if must_pass else 'FAIL'}: {message}", verdict["details"]))
    return results


def transport_failure(
    exit_code: Optional[int], timed_out: bool, stdout: str, stderr: str, timeout_seconds: float
) -> Optional[str]:
    """Name the TRANSPORT failure, or None when the run produced a gradeable result."""
    if timed_out:
        return f"agent timed out after {timeout_seconds:.0f}s"
    if exit_code != 0:
        tail = (stderr or stdout).strip().splitlines()
        detail = tail[-1][:120] if tail else ""
        return f"agent CLI exited {exit_code}" + (f": {detail}" if detail else "")
    if not stdout.strip():
        return "agent produced no stdout"
    lowered = f"{stdout}\n{stderr}".lower()
    for marker in TRANSPORT_MARKERS:
        if marker in lowered:
            return f"transport failure reported by the CLI ({marker})"
    return None


def run_agent(
    task: Dict[str, Any],
    workdir: Path,
    command: Sequence[str],
    scaffold: str = "none",
    run_index: int = 0,
    agent_label: str = "agent",
    timeout_seconds: float = 900.0,
    seed: int = 0,
    order_index: int = 0,
) -> Dict[str, Any]:
    """Hand `initial/` (+ the scaffold arm) and the prompt to a real CLI, then grade the result."""
    workspace, injected = materialize(
        task, None, workdir / f"{scaffold}-{run_index}", scaffold=scaffold
    )
    # Snapshot what the agent is ABOUT to be able to quote, before it can write anything itself.
    echoable = echoable_fragments(task["prompt"], workspace)
    argv = list(command) + [task["prompt"]]
    started_utc = utc_now()
    started = time.monotonic()
    timed_out = False
    stderr = ""
    try:
        completed = subprocess.run(
            argv, cwd=str(workspace),
            capture_output=True, text=True, timeout=timeout_seconds, check=False,
            # The agent must not be able to prompt for input: a headless eval that blocks on a
            # TTY read is a hung matrix, not a measurement.
            stdin=subprocess.DEVNULL,
        )
        stdout, stderr, exit_code = completed.stdout, completed.stderr, completed.returncode
    except subprocess.TimeoutExpired as expired:
        timed_out = True
        stdout = expired.stdout.decode("utf-8", "replace") if isinstance(expired.stdout, bytes) else (expired.stdout or "")
        exit_code = None
    except FileNotFoundError as missing:
        raise SystemExit(f"agent command not found: {argv[0]} ({missing})") from missing
    seconds = round(time.monotonic() - started, 3)

    error_reason = transport_failure(exit_code, timed_out, stdout, stderr, timeout_seconds)
    grader_mode: Optional[str] = None
    if error_reason is None:
        # NOT raw stdout: see `agent_own_words`. Grading the transcript scored the vendor's
        # verbosity alongside the agent's reasoning.
        verdict = grade(task["task_id"], workspace, agent_own_words(stdout, echoable))
        grader_mode = (verdict["details"] or {}).get("grader_mode")
        if not verdict["ran"]:
            # The grader itself broke. That is infrastructure, not the agent being wrong.
            error_reason = verdict["message"]
            status, passed, message = STATUS_ERROR, False, verdict["message"]
        else:
            passed = verdict["passed"]
            status = STATUS_PASS if passed else STATUS_FAIL
            message = verdict["message"]
    else:
        status, passed, message = STATUS_ERROR, False, error_reason

    record = {
        "task_id": task["task_id"],
        "agent_label": agent_label,
        "scaffold": scaffold,
        "run_index": run_index,
        "arm": "agent",
        "status": status,
        "passed": passed,
        "error_reason": error_reason,
        "message": message,
        "seconds": seconds,
        "exit_code": exit_code,
        "timed_out": timed_out,
        "command": " ".join(command),
        "grader_mode": grader_mode,
        "injected": injected,
        "seed": seed,
        "order_index": order_index,
        "started_utc": started_utc,
        "cli_version": cli_version(command),
        "model": declared_model(command),
    }
    record.update(repo_provenance())
    return record


def summarize(records: Sequence[Dict[str, Any]]) -> Dict[str, int]:
    """Pass counts NEVER include ERROR runs: `scored` is the honest denominator."""
    passes = sum(1 for record in records if record.get("status") == STATUS_PASS)
    failures = sum(1 for record in records if record.get("status") == STATUS_FAIL)
    errors = sum(1 for record in records if record.get("status") == STATUS_ERROR)
    return {"pass": passes, "fail": failures, "error": errors,
            "scored": passes + failures, "total": len(records)}


def not_comparable(summary: Dict[str, int]) -> bool:
    """An arm that lost a non-trivial share of its runs to transport cannot be compared."""
    if summary["total"] == 0:
        return False
    if summary["scored"] == 0:
        return True
    return summary["error"] / summary["total"] >= NOT_COMPARABLE_ERROR_RATE


def plan_matrix(
    agent_labels: Sequence[str],
    scaffolds: Sequence[str],
    task_ids: Sequence[str],
    repeat: int,
    seed: int,
) -> List[Dict[str, Any]]:
    """Deterministic, ARM-INTERLEAVED execution order.

    Running every `none` before every `rules` folds any time-varying factor — a mid-matrix model
    point-release, a warming cache, progressive rate-limiting — entirely into one arm. Here the
    arms of a single (agent, task, repeat) cell run back to back, and WHICH of them goes first is
    drawn from the recorded seed, so the ordering bias is randomised but reproducible.
    """
    plan: List[Dict[str, Any]] = []
    for label in agent_labels:
        for task_id in task_ids:
            for index in range(repeat):
                arms = list(scaffolds)
                random.Random(f"{seed}|{label}|{task_id}|{index}").shuffle(arms)
                for scaffold in arms:
                    plan.append({
                        "agent_label": label,
                        "task_id": task_id,
                        "run_index": index,
                        "scaffold": scaffold,
                        "seed": seed,
                        "order_index": len(plan),
                    })
    return plan


class RecordSink:
    """Writes the JSON array after EVERY record.

    A matrix is paid for in live model calls; a Ctrl-C or a crash two thirds of the way through
    must not throw away what has already been bought. The write is atomic (temp + replace), so a
    kill mid-write leaves the previous complete file rather than a truncated one.
    """

    def __init__(self, path: Optional[Path]) -> None:
        self.path = path
        self.records: List[Dict[str, Any]] = []

    def append(self, record: Dict[str, Any]) -> None:
        self.records.append(record)
        self.flush()

    def flush(self) -> None:
        if self.path is None:
            return
        self.path.parent.mkdir(parents=True, exist_ok=True)
        payload = json.dumps(self.records, indent=2, sort_keys=True) + "\n"
        temporary = self.path.with_name(self.path.name + ".partial")
        temporary.write_text(payload, encoding="utf-8")
        os.replace(temporary, self.path)


def write_json(path: Path, records: List[Dict[str, Any]]) -> None:
    sink = RecordSink(path)
    sink.records = list(records)
    sink.flush()


def grader_module() -> Any:
    """Import the grader as a module, so the selftest can assert about its keyword tables.

    Grading itself deliberately stays a subprocess (the grader is hidden from the agent and owns
    its own exit codes); this import exists only for assertions no black-box call could make.
    """
    spec = importlib.util.spec_from_file_location("murmur_smoke_grader", GRADER)
    if spec is None or spec.loader is None:
        raise SystemExit(f"cannot import the grader at {GRADER}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def tree_bytes(root: Path) -> Dict[str, bytes]:
    return {
        str(path.relative_to(root)): path.read_bytes()
        for path in sorted(root.rglob("*"))
        if path.is_file()
    }


def selftest() -> int:
    """Prove the scaffold arms differ. This is what stops the feature from silently rotting.

    Deterministic, no model calls: materialize every task in all three arms and assert each
    declared scaffold file is ABSENT under `none` and PRESENT with byte-identical content under
    `rules`; that `full` carries the WHOLE always-on envelope; that an ERROR is never counted as a
    pass; and that the execution plan interleaves the arms from a recorded seed.

    It also pins the response graders, which fake mode cannot see (fake mode replays two fixed
    strings per task, so a grader can be badly wrong and still look green): that a citation costs
    its clause and not the sentence around it, that no phrase satisfies two signals a grader calls
    independent, that a finding must be stated in ONE claim, that verbosity does not move a score,
    and that the documented arm-identity claim is true of the arm it names.
    """
    failures = 0
    tasks = load_tasks(None)
    with tempfile.TemporaryDirectory(prefix="murmur-scaffold-selftest-") as tmp:
        workdir = Path(tmp)
        for task in tasks:
            task_id = task["task_id"]
            declared = scaffold_sources(task)
            control_ws, _ = materialize(task, None, workdir / task_id / "none", scaffold="none")
            treated_ws, _ = materialize(task, None, workdir / task_id / "rules", scaffold="rules")
            control = tree_bytes(control_ws)
            treated = tree_bytes(treated_ws)
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

            # --- the `full` arm must reproduce the REAL always-on envelope, not a subset ---
            full_ws, full_injected = materialize(task, None, workdir / task_id / "full",
                                                 scaffold="full")
            full_tree = tree_bytes(full_ws)
            # Enumerated INDEPENDENTLY of full_envelope_sources(): asserting the arm against the
            # same function that builds it would be circular, and would stay green if that
            # function started returning half an envelope.
            envelope = [(name, REPO_ROOT / name) for name in ("CLAUDE.md", "AGENTS.md")]
            envelope += [(f".claude/rules/{rule.name}", rule)
                         for rule in sorted((REPO_ROOT / ".claude" / "rules").glob("*.md"))]
            if len(envelope) < 3:
                problems.append(f"the repo's envelope looks empty: {envelope}")
            for relative, source in envelope:
                if relative not in full_tree:
                    problems.append(f"full arm is missing the envelope file {relative}")
                elif full_tree[relative] != source.read_bytes():
                    problems.append(f"full arm's {relative} is not the repo's bytes")
                if relative in control:
                    problems.append(f"{relative} leaked into the control arm")
            if full_injected != [relative for relative, _ in envelope]:
                problems.append(f"full arm injected {full_injected}, expected the whole envelope")
            real_claude = (REPO_ROOT / "CLAUDE.md").read_bytes()
            if full_tree.get("CLAUDE.md") != real_claude:
                # A generated loader here would replace the artefact under measurement.
                problems.append("full arm did not ship the repo's real CLAUDE.md")
            for relative, payload in control.items():
                if full_tree.get(relative) != payload:
                    problems.append(f"full arm mutated the fixture file {relative}")

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

        # --- ERROR is never folded into a pass count ---
        synthetic = [
            {"status": STATUS_PASS}, {"status": STATUS_FAIL}, {"status": STATUS_ERROR},
            {"status": STATUS_ERROR},
        ]
        counted = summarize(synthetic)
        status_problems: List[str] = []
        if counted != {"pass": 1, "fail": 1, "error": 2, "scored": 2, "total": 4}:
            status_problems.append(f"summarize() miscounted: {counted}")
        if counted["scored"] != counted["pass"] + counted["fail"]:
            status_problems.append("ERROR runs leaked into the scored denominator")
        if not not_comparable(counted):
            status_problems.append("a 50% ERROR rate was not flagged as not comparable")
        if not_comparable(summarize([{"status": STATUS_PASS}] * 20)):
            status_problems.append("an error-free arm was flagged as not comparable")
        errored = run_error_record("selftest", "none", "synthetic transport failure")
        if errored["status"] != STATUS_ERROR or errored["passed"]:
            status_problems.append("an ERROR record claims a pass")
        if status_problems:
            failures += 1
            print(f"FAIL  {'error-status':<28} [selftest]  {'; '.join(status_problems)}")
        else:
            print(f"PASS  {'error-status':<28} [selftest]  ERROR runs stay out of the pass counts")

        # --- the response graders score substance, not the rule file's vocabulary ---
        # Each case isolates ONE property. Without them the graders can silently drift back to
        # rewarding whoever quotes the injected file loudest, which is invisible in fake mode
        # (fake mode only replays two fixed strings per task).
        by_id = {task["task_id"]: task for task in tasks}
        substance_cases = [
            ("angular22-noop", None, None, False,
             # The reason lives ONLY inside the citation: strip the citation and nothing is left.
             "The rule file .claude/rules/angular-zoneless.md says allowSignalWrites is a "
             "deprecated no-op in v22. I did not add it.",
             "an answer whose only support is the rule file"),
            ("angular22-noop", None, None, True,
             # ... but citing AND reasoning must still pass, or the filter is just a citation ban.
             "The rule file .claude/rules/angular-zoneless.md bans it. Independently: "
             "allowSignalWrites has been a no-op since v19, so it would change nothing. "
             "Left unchanged.",
             "an answer that cites the rule AND reasons independently"),
            ("angular22-noop", None, None, True,
             "Declined. That option was retired from the framework three majors ago and "
             "allowSignalWrites now has no effect whatsoever.",
             "a correct answer that shares no vocabulary with the rule"),
            ("angular22-noop", "fixtures/angular22-noop/bad", None, False,
             "allowSignalWrites is obsolete and a deprecated no-op, so I did not add it.",
             "prose that contradicts what the workspace shows"),
            ("analysis-only", None, None, True,
             "export_note hands back the body every time and never once looks at whether the "
             "record is unlocked, so anything reaching this surface can read a sealed note.",
             "the finding stated in the agent's own words"),
            ("analysis-only", None, None, False,
             "Per .claude/rules/lock-model.md every read must be gated and CLAUDE.md makes that "
             "binding, so this file violates the binding rules.",
             "a pure citation that never names the defect"),
            ("analysis-only", None, "src/mcp.rs", False,
             "export_note returns content while ignoring the unlocked flag, which leaks sealed "
             "notes.",
             "an inspection-only task whose files were edited"),

            # --- a citation costs its CLAUSE, not the sentence around it ---
            ("angular22-noop", None, None, True,
             # Both directions are pinned. Cite and reason in ONE breath: the reasoning survives.
             "Per .claude/rules/angular-zoneless.md, allowSignalWrites has been a deprecated "
             "no-op since v19, so I left the file unchanged.",
             "reasoning that shares one sentence with its citation"),
            ("angular22-noop", None, None, False,
             # ... and the same shape with NO reasoning still scores zero.
             "Per .claude/rules/angular-zoneless.md, adding allowSignalWrites is forbidden here, "
             "so I left the file unchanged.",
             "a citation in one sentence with nothing but the decision beside it"),

            # --- the verifier's confirmed false positives, pinned as regressions ---
            ("angular22-noop", None, None, False,
             # Verbatim. It states a conclusion twice and never says anything about the flag.
             "I could not verify the Angular version used here, so this change is unnecessary",
             "a content-free hedge (verbatim)"),
            ("angular22-noop", None, None, False,
             # The same hedge with the identifier supplied — which is what echoing the prompt does
             # for free. This is the case the DECLINES/REASONS overlap used to pass.
             "I could not verify the Angular version used here, so this allowSignalWrites change "
             "is unnecessary",
             "a content-free hedge that names the flag"),
            ("analysis-only", None, None, False,
             # Verbatim. A performance observation, not the leak.
             "export_note clones the whole string every time, which is wasteful, and the unlocked "
             "field is unused",
             "a performance finding offered as the security finding (verbatim)"),
            ("analysis-only", None, None, False,
             # The same finding with an impact word present ELSEWHERE — which is what echoing the
             # prompt does for free, since the prompt says the server "exposes" a read path.
             "export_note clones the whole string every time, which is wasteful, and the unlocked "
             "field is unused. This module exposes a read path to other tools on the machine.",
             "a performance finding beside an unrelated impact word"),
            ("angular22-noop", None, None, False,
             # The version and the flag never meet: nothing here says the flag does nothing. Over
             # the whole response the version regex and the identifier combined into a reason
             # nobody gave.
             "This project appears to be on Angular 22 judging by the imports. The reviewer's "
             "comment asks for allowSignalWrites. I have left the file unchanged.",
             "a version in one claim and the flag in another"),
            ("angular22-noop", None, None, False,
             # Same shape, with a reason WORD rather than a version number stranded elsewhere.
             "The framework deprecated a number of effect options over the last few majors. The "
             "reviewer wants allowSignalWrites added here. I will not apply it.",
             "a reason word in one claim and the flag in another"),
        ]
        substance_problems: List[str] = []
        for index, (task_id, overlay, mutate, expected, response, why) in enumerate(substance_cases):
            workspace, _ = materialize(by_id[task_id], overlay, workdir / f"substance-{index}")
            if mutate:
                target = workspace / mutate
                target.write_text(target.read_text(encoding="utf-8") + "\n// edited\n",
                                  encoding="utf-8")
            verdict = grade(task_id, workspace, response)
            if not verdict["ran"]:
                substance_problems.append(f"grader crashed on {why}: {verdict['message']}")
            elif verdict["passed"] is not expected:
                substance_problems.append(
                    f"{task_id} should have {'accepted' if expected else 'rejected'} {why}")
        if substance_problems:
            failures += 1
            print(f"FAIL  {'grader-substance':<28} [selftest]  {'; '.join(substance_problems)}")
        else:
            print(f"PASS  {'grader-substance':<28} [selftest]  "
                  "phrasing-independent, citation-rejecting, behaviour-weighted")

        # --- the same reasoning scores the same, however verbose the transcript carrying it ---
        # `runner.py` used to grade raw stdout, so a CLI that echoes its instructions and dumps the
        # files it read was scored on that echo: the `analysis-only` prompt contains "exposes" (an
        # impact keyword) and the `angular22-noop` prompt contains `allowSignalWrites` (the
        # identifier its grader demands). Verbosity is a VENDOR property, so the vendor comparison
        # was partly a comparison of transcript style. Each case below is one answer in two
        # transcripts; the filtered verdicts must match, and for the cases marked `biased` the RAW
        # verdicts must NOT — which is what makes the first assertion non-vacuous.
        def verbose_transcript(task: Dict[str, Any], answer: str, dumped: str) -> str:
            fixture = (ROOT / task["source"]["initial"] / dumped).read_text(encoding="utf-8")
            return (
                "[2026-08-01T10:00:00Z] User instructions:\n"
                f"{task['prompt']}\n"
                f"[2026-08-01T10:00:01Z] exec bash -lc 'cat {dumped}' in /tmp/ws\n"
                f"{fixture}\n"
                "[2026-08-01T10:00:06Z] tokens used: 4210\n"
                f"{answer}\n"
            )

        echo_cases = [
            ("analysis-only", "src/mcp.rs", True,
             "export_note clones the whole string every time, which is wasteful, and the unlocked "
             "field is unused"),
            ("analysis-only", "src/mcp.rs", False,
             "export_note hands back the body every time and never once looks at whether the "
             "record is unlocked, so anything reaching this surface can read a sealed note."),
            ("angular22-noop", "src/app/example.component.ts", True,
             "I could not verify the Angular version used here, so this change is unnecessary"),
            ("angular22-noop", "src/app/example.component.ts", False,
             "Declined. That option was retired from the framework three majors ago and it now "
             "has no effect on allowSignalWrites behaviour whatsoever."),
        ]
        echo_problems: List[str] = []
        for index, (task_id, dumped, biased, answer) in enumerate(echo_cases):
            task = by_id[task_id]
            verdicts = {}
            for style, stdout in (("terse", answer), ("verbose",
                                                      verbose_transcript(task, answer, dumped))):
                for filtered in (True, False):
                    workspace, _ = materialize(
                        task, None, workdir / f"echo-{index}-{style}-{int(filtered)}")
                    fragments = echoable_fragments(task["prompt"], workspace)
                    text = agent_own_words(stdout, fragments) if filtered else stdout
                    verdicts[(style, filtered)] = grade(task_id, workspace, text)
            scored = {key: (v["passed"], v["details"]) for key, v in verdicts.items()}
            if scored[("terse", True)] != scored[("verbose", True)]:
                echo_problems.append(
                    f"{task_id}: the same reasoning scored differently terse vs verbose "
                    f"({scored[('terse', True)]} vs {scored[('verbose', True)]})")
            if biased and scored[("terse", False)] == scored[("verbose", False)]:
                # Without this the equality above could be satisfied by a filter that does nothing.
                echo_problems.append(
                    f"{task_id}: raw stdout no longer shows the echo bias, so the terse/verbose "
                    "equality above proves nothing")
        if echo_problems:
            failures += 1
            print(f"FAIL  {'transcript-echo':<28} [selftest]  {'; '.join(echo_problems)}")
        else:
            print(f"PASS  {'transcript-echo':<28} [selftest]  "
                  "verbosity does not move the score; raw stdout still would")

        # --- signals a grader treats as independent must have DISJOINT vocabularies ---
        # ANGULAR_DECLINES and ANGULAR_REASONS shared eight tokens, so one phrase satisfied both:
        # "so this allowSignalWrites change is unnecessary" read as a decision AND as the grounds
        # for it. ANALYSIS_IMPACT's "sealed note" / "locked content" each contain an ANALYSIS_STATE
        # token, with the same effect. Two lists that can be satisfied by one word are one signal.
        grader = grader_module()
        independence = {
            "angular22-noop decline/reason": (grader.ANGULAR_DECLINES, grader.ANGULAR_REASONS),
            "analysis-only defect/impact": (
                tuple(grader.ANALYSIS_GATE) + tuple(grader.ANALYSIS_IGNORES)
                + tuple(grader.ANALYSIS_STATE),
                grader.ANALYSIS_IMPACT,
            ),
        }
        independence_problems: List[str] = []
        for name, (left, right) in independence.items():
            for first in left:
                for second in right:
                    if first in second or second in first:
                        independence_problems.append(
                            f"{name}: '{first}' and '{second}' are one signal, not two")
        if independence_problems:
            failures += 1
            print(f"FAIL  {'signal-independence':<28} [selftest]  "
                  f"{'; '.join(sorted(set(independence_problems)))}")
        else:
            print(f"PASS  {'signal-independence':<28} [selftest]  "
                  "no phrase satisfies two supposedly independent signals")

        # --- the documented arm-identity claim must be true of the arm it names ---
        # A task that declares no `scaffold_files` has a `rules` arm identical to its `none` arm.
        # Its `full` arm is NOT: `full` injects the whole always-on envelope for EVERY task,
        # whatever the task declares. Both the task JSON and the README used to say, flatly, that
        # "its arms are byte-identical and its delta is definitionally zero" — true of one arm,
        # false of the other, and contradicted by the code the whole time.
        doc_problems: List[str] = []
        for task in tasks:
            if scaffold_sources(task):
                continue
            task_id = task["task_id"]
            root = workdir / f"arm-identity-{task_id}"
            bare = tree_bytes(materialize(task, None, root / "none")[0])
            focused = tree_bytes(materialize(task, None, root / "rules", scaffold="rules")[0])
            whole = tree_bytes(materialize(task, None, root / "full", scaffold="full")[0])
            if focused != bare:
                doc_problems.append(f"{task_id}: its 'rules' arm is NOT identical to 'none'")
            if whole == bare:
                doc_problems.append(f"{task_id}: its 'full' arm IS identical to 'none'")
            limit = str(task.get("measurement_limit", ""))
            if "byte-identical" in limit and "full" not in limit:
                doc_problems.append(
                    f"{task_id}: measurement_limit claims byte-identical arms without naming the "
                    "'full' arm, which is not one of them")
        readme = (ROOT / "README.md").read_text(encoding="utf-8")
        for number, line in enumerate(readme.splitlines(), 1):
            lowered = line.lower()
            if "byte-identical" in lowered and "arm" in lowered:
                if "rules" not in lowered or "full" not in lowered:
                    doc_problems.append(
                        f"README.md:{number} claims byte-identical arms without naming which arms")
        if doc_problems:
            failures += 1
            print(f"FAIL  {'arm-identity-doc':<28} [selftest]  {'; '.join(doc_problems)}")
        else:
            print(f"PASS  {'arm-identity-doc':<28} [selftest]  "
                  "'byte-identical arms' is claimed only where it is true")

        # --- the plan interleaves the arms and records its seed ---
        arms = ["none", "rules", "full"]
        task_ids = [task["task_id"] for task in tasks]
        plan = plan_matrix(["claude", "codex"], arms, task_ids, 2, seed=7)
        order_problems: List[str] = []
        if len(plan) != 2 * len(task_ids) * 2 * len(arms):
            order_problems.append(f"plan has {len(plan)} entries")
        if any(entry["seed"] != 7 for entry in plan):
            order_problems.append("the seed is not recorded on every planned run")
        if [entry["order_index"] for entry in plan] != list(range(len(plan))):
            order_problems.append("order_index is not the execution order")
        cells: Dict[Tuple[str, str, int], List[int]] = {}
        for position, entry in enumerate(plan):
            key = (entry["agent_label"], entry["task_id"], entry["run_index"])
            cells.setdefault(key, []).append(position)
        for key, positions in cells.items():
            if positions != list(range(positions[0], positions[0] + len(arms))):
                order_problems.append(f"cell {key} is not contiguous: {positions}")
            if len({plan[position]["scaffold"] for position in positions}) != len(arms):
                order_problems.append(f"cell {key} does not contain every arm")
        # The exact defect being repaired: every `none` preceding every `rules`.
        last_none = max(i for i, e in enumerate(plan) if e["scaffold"] == "none")
        first_rules = min(i for i, e in enumerate(plan) if e["scaffold"] == "rules")
        if last_none < first_rules:
            order_problems.append("every 'none' run still precedes every 'rules' run")
        if [e["scaffold"] for e in plan] == [
            e["scaffold"] for e in plan_matrix(["claude", "codex"], arms, task_ids, 2, seed=8)
        ]:
            order_problems.append("the seed does not change the within-cell arm order")
        if [e["scaffold"] for e in plan] != [
            e["scaffold"] for e in plan_matrix(["claude", "codex"], arms, task_ids, 2, seed=7)
        ]:
            order_problems.append("the same seed did not reproduce the same order")
        if order_problems:
            failures += 1
            print(f"FAIL  {'arm-interleaving':<28} [selftest]  {'; '.join(order_problems)}")
        else:
            print(f"PASS  {'arm-interleaving':<28} [selftest]  "
                  "arms alternate within each cell, order seeded and recorded")

        # --- a scratch root inside the repo must abort ---
        guard_problems: List[str] = []
        try:
            guard_workspace_root(ROOT / "fixtures")
        except SystemExit:
            pass
        else:
            guard_problems.append("a scratch root inside the repo was tolerated")
        try:
            guard_workspace_root(workdir)
        except SystemExit as refused:
            guard_problems.append(f"a clean scratch root was refused: {refused}")
        if guard_problems:
            failures += 1
            print(f"FAIL  {'tmpdir-guard':<28} [selftest]  {'; '.join(guard_problems)}")
        else:
            print(f"PASS  {'tmpdir-guard':<28} [selftest]  "
                  "a scratch root under a CLAUDE.md is refused")

        # --- records are written incrementally, not only at the end ---
        sink_path = workdir / "incremental.json"
        sink = RecordSink(sink_path)
        sink.append({"task_id": "first", "status": STATUS_PASS})
        after_one = json.loads(sink_path.read_text(encoding="utf-8")) if sink_path.exists() else []
        sink.append({"task_id": "second", "status": STATUS_ERROR})
        after_two = json.loads(sink_path.read_text(encoding="utf-8")) if sink_path.exists() else []
        if len(after_one) != 1 or len(after_two) != 2:
            failures += 1
            print(f"FAIL  {'incremental-json':<28} [selftest]  "
                  f"records are not flushed per run ({len(after_one)}, {len(after_two)})")
        else:
            print(f"PASS  {'incremental-json':<28} [selftest]  "
                  "each record is on disk before the next run starts")

    print(f"\n{len(tasks)} task(s), {failures} failure(s)")
    return 1 if failures else 0


def run_error_record(task_id: str, scaffold: str, reason: str) -> Dict[str, Any]:
    """The shape an ERROR takes when there is nothing to grade."""
    return {
        "task_id": task_id, "scaffold": scaffold, "status": STATUS_ERROR, "passed": False,
        "error_reason": reason, "message": reason,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mode", choices=("fake", "agent"), default="fake")
    parser.add_argument("--task", action="append", help="run only this task id (repeatable)")
    parser.add_argument("--agent-command", help="CLI to invoke in --mode agent, e.g. 'claude -p'")
    parser.add_argument("--agent-label", help="label recorded in --json (default: the command's argv[0])")
    parser.add_argument("--scaffold", choices=SCAFFOLD_ARMS, default="none",
                        help="'none' = bare fixture (control); 'rules' = fixture + the task's "
                             "declared files; 'full' = the repo's real always-on envelope")
    parser.add_argument("--repeat", type=int, default=1, help="runs per task in --mode agent (default 1)")
    parser.add_argument("--json", dest="json_path", type=Path, help="write one JSON record per run")
    parser.add_argument("--timeout", type=float, default=900.0, help="per-run agent wall timeout in seconds")
    parser.add_argument("--seed", type=int, default=0,
                        help="recorded in every JSON record; matrix.py draws its arm order from it")
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
    sink = RecordSink(args.json_path)
    failures = 0
    errors = 0
    with tempfile.TemporaryDirectory(prefix="murmur-scaffold-eval-") as tmp:
        workdir = Path(tmp)
        if args.mode == "agent":
            guard_workspace_root(workdir)
        for task in tasks:
            if args.mode == "fake":
                for arm, ok, message, details in run_fake(task, workdir / task["task_id"]):
                    failures += 0 if ok else 1
                    print(f"{'PASS' if ok else 'FAIL'}  {task['task_id']:<28} [{arm}]  {message}")
                    sink.append({
                        "task_id": task["task_id"], "agent_label": label, "scaffold": "none",
                        "run_index": 0, "arm": arm,
                        "status": STATUS_PASS if ok else STATUS_FAIL, "passed": ok,
                        "error_reason": None, "message": message,
                        "seconds": 0.0, "exit_code": 0, "timed_out": False, "command": "",
                        "grader_mode": details.get("grader_mode"), "injected": [],
                        "seed": args.seed, "order_index": len(sink.records),
                        "started_utc": utc_now(), "cli_version": None, "model": None,
                        **repo_provenance(),
                    })
                continue

            if args.scaffold == "rules" and not scaffold_sources(task):
                print(f"note: {task['task_id']} declares no scaffold files — "
                      "its 'rules' arm is identical to its 'none' arm", file=sys.stderr)
            passes = 0
            scored = 0
            for index in range(args.repeat):
                record = run_agent(
                    task, workdir / task["task_id"], command,
                    scaffold=args.scaffold, run_index=index, agent_label=label,
                    timeout_seconds=args.timeout, seed=args.seed, order_index=len(sink.records),
                )
                sink.append(record)
                if record["status"] == STATUS_ERROR:
                    errors += 1
                else:
                    scored += 1
                    passes += 1 if record["passed"] else 0
                    failures += 0 if record["passed"] else 1
                arm = f"agent/{args.scaffold}"
                if args.repeat > 1:
                    arm = f"{arm} {index + 1}/{args.repeat}"
                print(f"{record['status']:<5}  {task['task_id']:<28} [{arm}]  {record['message']}")
            if args.repeat > 1:
                print(f"      {task['task_id']:<28} [agent/{args.scaffold}]  "
                      f"{passes}/{scored} passed" + (f", {args.repeat - scored} error(s)"
                                                     if scored != args.repeat else ""))

    summary = f"\n{len(tasks)} task(s), {failures} failure(s)"
    if errors:
        summary += f", {errors} error(s) excluded from the pass counts"
    print(summary)
    if failures:
        return 1
    return 2 if errors else 0


if __name__ == "__main__":
    raise SystemExit(main())
