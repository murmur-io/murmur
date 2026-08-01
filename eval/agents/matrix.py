#!/usr/bin/env python3
"""Comparative driver for the scaffold eval: {agents} x {scaffold arms} x {tasks} x {repeats}.

`runner.py` answers "did THIS agent pass THIS task once?". That is an anecdote. This answers the
two questions the suite exists for:

  1. Does the scaffold help?   Compare the `none` column against the `rules` column for one agent.
  2. Which agent is better?    Compare two agents within the same column.

Both are only readable as a comparison, so both arms and both agents run against the SAME fixtures,
the SAME graders and the SAME repetition count, and every cell reports `k/N` rather than a bare
PASS/FAIL — models are non-deterministic and a single run cannot distinguish a rule from luck.

    python3 eval/agents/matrix.py \
        --agent 'claude=claude -p --permission-mode dontAsk' \
        --agent 'codex=codex exec --skip-git-repo-check --ephemeral -s workspace-write' \
        --repeat 3 --json eval/agents/results/matrix.json

Invocation flags come from `.agents/harness/runtime.py::invoke_model`, which is the authority on
how each vendor is driven headlessly — but this runner deliberately keeps its own invocation
simple. The harness reviewer profile is TOOL-FREE by design; an eval agent must be able to read and
edit files, so only the non-interactivity carries over: `-p/--print` and `--permission-mode dontAsk`
for Claude, `exec --ephemeral --skip-git-repo-check` (plus a writable sandbox) for Codex. The
runner additionally closes stdin, so a CLI that tries to ask a question dies instead of hanging.

`--dry-run` prints the planned cross-product and exits without spending a single model call. Use it
before every real matrix; live calls are the expensive part of this suite.
"""

from __future__ import annotations

import argparse
import shlex
import sys
import tempfile
import time
from pathlib import Path
from typing import Any, Dict, List, Sequence, Tuple

sys.path.insert(0, str(Path(__file__).resolve().parent))

import runner  # noqa: E402  (sibling module, resolved via the path insert above)


def parse_agent(spec: str) -> Tuple[str, List[str]]:
    """Parse `label=command` into a label and an argv list."""
    label, separator, command = spec.partition("=")
    if not separator or not label.strip() or not command.strip():
        raise SystemExit(f"--agent expects 'label=command', got: {spec!r}")
    argv = shlex.split(command)
    if not argv:
        raise SystemExit(f"--agent command is empty: {spec!r}")
    return label.strip(), argv


def render_table(
    tasks: Sequence[Dict[str, Any]],
    columns: Sequence[Tuple[str, str]],
    cells: Dict[Tuple[str, str, str], List[Dict[str, Any]]],
    repeat: int,
) -> str:
    headers = ["task"] + [f"{label}/{scaffold}" for label, scaffold in columns]
    rows: List[List[str]] = []
    for task in tasks:
        row = [task["task_id"]]
        for label, scaffold in columns:
            records = cells.get((label, scaffold, task["task_id"]), [])
            row.append(f"{sum(1 for r in records if r['passed'])}/{len(records) or repeat}")
        rows.append(row)

    total = ["TOTAL"]
    for label, scaffold in columns:
        records = [r for key, values in cells.items() if key[0] == label and key[1] == scaffold
                   for r in values]
        total.append(f"{sum(1 for r in records if r['passed'])}/{len(records)}")
    rows.append(total)

    widths = [max(len(headers[i]), *(len(row[i]) for row in rows)) for i in range(len(headers))]
    lines = ["  ".join(headers[i].ljust(widths[i]) for i in range(len(headers))),
             "  ".join("-" * widths[i] for i in range(len(headers)))]
    for index, row in enumerate(rows):
        if index == len(rows) - 1:
            lines.append("  ".join("-" * widths[i] for i in range(len(headers))))
        lines.append("  ".join(row[i].ljust(widths[i]) for i in range(len(headers))))
    return "\n".join(lines)


def render_deltas(
    labels: Sequence[str],
    scaffolds: Sequence[str],
    cells: Dict[Tuple[str, str, str], List[Dict[str, Any]]],
) -> List[str]:
    """The measurement this suite exists for: rules-arm passes minus control-arm passes."""
    if "none" not in scaffolds or "rules" not in scaffolds:
        return []
    lines = []
    for label in labels:
        control = [r for key, values in cells.items() if key[0] == label and key[1] == "none"
                   for r in values]
        treated = [r for key, values in cells.items() if key[0] == label and key[1] == "rules"
                   for r in values]
        control_pass = sum(1 for r in control if r["passed"])
        treated_pass = sum(1 for r in treated if r["passed"])
        lines.append(
            f"  {label}: rules {treated_pass}/{len(treated)} vs none {control_pass}/{len(control)} "
            f"(delta {treated_pass - control_pass:+d} passes)"
        )
    return lines


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--agent", action="append", required=True, metavar="LABEL=COMMAND",
                        help="repeatable, e.g. --agent 'claude=claude -p --permission-mode dontAsk'")
    parser.add_argument("--scaffold", action="append", choices=runner.SCAFFOLD_ARMS,
                        help="repeatable; default runs both arms")
    parser.add_argument("--task", action="append", help="run only this task id (repeatable)")
    parser.add_argument("--repeat", type=int, default=1, help="runs per cell (default 1)")
    parser.add_argument("--json", dest="json_path", type=Path, help="write one JSON record per run")
    parser.add_argument("--timeout", type=float, default=900.0, help="per-run agent wall timeout")
    parser.add_argument("--dry-run", action="store_true",
                        help="print the planned cross-product and exit without any model call")
    args = parser.parse_args()

    if args.repeat < 1:
        raise SystemExit("--repeat must be >= 1")
    agents = [parse_agent(spec) for spec in args.agent]
    labels = [label for label, _ in agents]
    if len(set(labels)) != len(labels):
        raise SystemExit("--agent labels must be unique")
    scaffolds = args.scaffold or list(runner.SCAFFOLD_ARMS)
    scaffolds = [arm for arm in runner.SCAFFOLD_ARMS if arm in scaffolds]
    tasks = runner.load_tasks(args.task)

    # Resolve every declared scaffold file BEFORE spending a model call: a missing file must fail
    # the whole matrix, not silently turn one treatment cell into a second control cell.
    for task in tasks:
        declared = runner.scaffold_sources(task)
        if "rules" in scaffolds and not declared:
            print(f"note: {task['task_id']} declares no scaffold files — "
                  "its 'rules' cells are identical to its 'none' cells", file=sys.stderr)

    columns = [(label, scaffold) for label in labels for scaffold in scaffolds]
    planned = len(columns) * len(tasks) * args.repeat
    print(f"matrix: {len(labels)} agent(s) x {len(scaffolds)} arm(s) x {len(tasks)} task(s) "
          f"x {args.repeat} repeat(s) = {planned} run(s)\n")
    if args.dry_run:
        for label, argv in agents:
            print(f"  {label}: {' '.join(argv)}")
        for label, scaffold in columns:
            for task in tasks:
                print(f"  plan  {label}/{scaffold:<5}  {task['task_id']}")
        return 0

    cells: Dict[Tuple[str, str, str], List[Dict[str, Any]]] = {}
    records: List[Dict[str, Any]] = []
    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="murmur-scaffold-matrix-") as tmp:
        workdir = Path(tmp)
        for label, argv in agents:
            for scaffold in scaffolds:
                for task in tasks:
                    bucket = cells.setdefault((label, scaffold, task["task_id"]), [])
                    for index in range(args.repeat):
                        record = runner.run_agent(
                            task, workdir / label / task["task_id"], argv,
                            scaffold=scaffold, run_index=index, agent_label=label,
                            timeout_seconds=args.timeout,
                        )
                        bucket.append(record)
                        records.append(record)
                        arm = f"{label}/{scaffold}"
                        if args.repeat > 1:
                            arm = f"{arm} {index + 1}/{args.repeat}"
                        verdict = "PASS" if record["passed"] else "FAIL"
                        print(f"{verdict}  {task['task_id']:<28} [{arm}]  {record['message']}")

    if args.json_path:
        runner.write_json(args.json_path, records)

    print()
    print(render_table(tasks, columns, cells, args.repeat))
    deltas = render_deltas(labels, scaffolds, cells)
    if deltas:
        print("\nscaffold effect (higher is better):")
        print("\n".join(deltas))
    print(f"\n{len(records)} run(s) in {time.monotonic() - started:.1f}s"
          + (f", records written to {args.json_path}" if args.json_path else ""))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
