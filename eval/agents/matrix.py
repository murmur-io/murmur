#!/usr/bin/env python3
"""Comparative driver for the scaffold eval: {agents} x {scaffold arms} x {tasks} x {repeats}.

`runner.py` answers "did THIS agent pass THIS task once?". That is an anecdote. This answers the
two questions the suite exists for:

  1. Does the scaffold help?   Compare the `none` column against `rules` and `full` for one agent.
  2. Which agent is better?    Compare two agents within the same column.

Both are only readable as a comparison, so every arm and every agent runs against the SAME
fixtures, the SAME graders and the SAME repetition count, and every cell reports `k/N` rather than
a bare PASS/FAIL — models are non-deterministic and a single run cannot distinguish a rule from
luck.

    python3 eval/agents/matrix.py \\
        --agent 'claude=claude -p --permission-mode dontAsk' \\
        --agent 'codex=codex exec --skip-git-repo-check --ephemeral -s workspace-write' \\
        --repeat 3 --json eval/agents/results/matrix.json

Invocation flags come from `.agents/harness/runtime.py::invoke_model`, which is the authority on
how each vendor is driven headlessly — but this runner deliberately keeps its own invocation
simple. The harness reviewer profile is TOOL-FREE by design; an eval agent must be able to read and
edit files, so only the non-interactivity carries over: `-p/--print` and `--permission-mode dontAsk`
for Claude, `exec --ephemeral --skip-git-repo-check` (plus a writable sandbox) for Codex. The
runner additionally closes stdin, so a CLI that tries to ask a question dies instead of hanging.

`--dry-run` prints the planned cross-product and exits without spending a single model call. Use it
before every real matrix; live calls are the expensive part of this suite.

# Three things this driver does that a naive cross-product does not

**Interleaving.** Until 2026-08-01 the loop was `for arm: for task: run`, so every `none` run
happened before every `rules` run. A model point-release mid-matrix, a warming cache or
progressive rate-limiting would land entirely in one arm and be read as a scaffold effect. The
arms of one (agent, task, repeat) cell now run back to back, and which of them goes first is drawn
from `--seed` — deterministic, reproducible, RECORDED in every JSON record.

**ERROR is not FAIL.** A 429, a non-zero exit, an empty stdout or a timeout never produced a
gradeable result, so it is reported as ERROR, excluded from the pass counts, and shown separately.
An arm that lost a non-trivial share of its runs that way is flagged NOT COMPARABLE, because its
denominator is no longer the same experiment as the other arm's.

**Incremental JSON.** Every record is flushed to `--json` as it completes. A Ctrl-C at run 40 of 60
keeps the 40 live calls already paid for.
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


def cell_label(records: Sequence[Dict[str, Any]], repeat: int) -> str:
    """`k/N` over SCORED runs, with an explicit `eN` when transport failures cost us runs."""
    if not records:
        return f"0/{repeat}"
    summary = runner.summarize(records)
    text = f"{summary['pass']}/{summary['scored']}"
    if summary["error"]:
        text += f" e{summary['error']}"
    return text


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
            row.append(cell_label(cells.get((label, scaffold, task["task_id"]), []), repeat))
        rows.append(row)

    total = ["TOTAL"]
    for label, scaffold in columns:
        records = [r for key, values in cells.items() if key[0] == label and key[1] == scaffold
                   for r in values]
        total.append(cell_label(records, repeat * len(tasks)))
    rows.append(total)

    widths = [max(len(headers[i]), *(len(row[i]) for row in rows)) for i in range(len(headers))]
    lines = ["  ".join(headers[i].ljust(widths[i]) for i in range(len(headers))),
             "  ".join("-" * widths[i] for i in range(len(headers)))]
    for index, row in enumerate(rows):
        if index == len(rows) - 1:
            lines.append("  ".join("-" * widths[i] for i in range(len(headers))))
        lines.append("  ".join(row[i].ljust(widths[i]) for i in range(len(headers))))
    lines.append("")
    lines.append("k/N counts PASSES over SCORED runs; eN = runs lost to transport (ERROR), which "
                 "are excluded from both.")
    return "\n".join(lines)


def column_records(
    cells: Dict[Tuple[str, str, str], List[Dict[str, Any]]], label: str, scaffold: str
) -> List[Dict[str, Any]]:
    return [r for key, values in cells.items() if key[0] == label and key[1] == scaffold
            for r in values]


def render_transport(
    labels: Sequence[str],
    scaffolds: Sequence[str],
    cells: Dict[Tuple[str, str, str], List[Dict[str, Any]]],
) -> List[str]:
    """Transport failures get their own report. They are not evidence about the scaffold."""
    lines: List[str] = []
    for label in labels:
        for scaffold in scaffolds:
            records = column_records(cells, label, scaffold)
            summary = runner.summarize(records)
            if not summary["error"]:
                continue
            reasons: Dict[str, int] = {}
            for record in records:
                if record.get("status") == runner.STATUS_ERROR:
                    reason = str(record.get("error_reason") or "unknown")
                    reasons[reason] = reasons.get(reason, 0) + 1
            flag = "  NOT COMPARABLE" if runner.not_comparable(summary) else ""
            lines.append(f"  {label}/{scaffold}: {summary['error']}/{summary['total']} "
                         f"run(s) lost to transport{flag}")
            for reason, count in sorted(reasons.items(), key=lambda item: -item[1]):
                lines.append(f"      {count}x {reason}")
    return lines


def render_deltas(
    labels: Sequence[str],
    scaffolds: Sequence[str],
    cells: Dict[Tuple[str, str, str], List[Dict[str, Any]]],
) -> List[str]:
    """The measurement this suite exists for: treatment-arm pass RATE minus control-arm pass rate.

    Rates, not raw counts, because ERROR runs make the denominators differ between arms.
    """
    if "none" not in scaffolds:
        return ["  (no control arm in this run — a delta needs --scaffold none)"]
    lines = []
    for label in labels:
        control = runner.summarize(column_records(cells, label, "none"))
        for scaffold in scaffolds:
            if scaffold == "none":
                continue
            treated = runner.summarize(column_records(cells, label, scaffold))
            if not control["scored"] or not treated["scored"]:
                lines.append(f"  {label}: {scaffold} vs none — NOT COMPARABLE, an arm scored no runs")
                continue
            control_rate = control["pass"] / control["scored"]
            treated_rate = treated["pass"] / treated["scored"]
            note = ""
            if runner.not_comparable(control) or runner.not_comparable(treated):
                note = "   [NOT COMPARABLE: transport losses exceed " \
                       f"{runner.NOT_COMPARABLE_ERROR_RATE:.0%} of an arm's runs]"
            lines.append(
                f"  {label}: {scaffold} {treated['pass']}/{treated['scored']} "
                f"({treated_rate:.0%}) vs none {control['pass']}/{control['scored']} "
                f"({control_rate:.0%}) -> {treated_rate - control_rate:+.0%} points{note}"
            )
    return lines


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--agent", action="append", required=True, metavar="LABEL=COMMAND",
                        help="repeatable, e.g. --agent 'claude=claude -p --permission-mode dontAsk'")
    parser.add_argument("--scaffold", action="append", choices=runner.SCAFFOLD_ARMS,
                        help="repeatable; default runs every arm (none, rules, full)")
    parser.add_argument("--task", action="append", help="run only this task id (repeatable)")
    parser.add_argument("--repeat", type=int, default=1, help="runs per cell (default 1)")
    parser.add_argument("--json", dest="json_path", type=Path,
                        help="write one JSON record per run, flushed as each run completes")
    parser.add_argument("--timeout", type=float, default=900.0, help="per-run agent wall timeout")
    parser.add_argument("--seed", type=int, default=0,
                        help="seeds the within-cell arm order; recorded in every JSON record")
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
    by_id = {task["task_id"]: task for task in tasks}
    argv_by_label = dict(agents)

    # Resolve every declared scaffold file BEFORE spending a model call: a missing file must fail
    # the whole matrix, not silently turn one treatment cell into a second control cell.
    for task in tasks:
        declared = runner.scaffold_sources(task)
        if "rules" in scaffolds and not declared:
            print(f"note: {task['task_id']} declares no scaffold files — "
                  "its 'rules' cells are identical to its 'none' cells", file=sys.stderr)
    if "full" in scaffolds:
        runner.full_envelope_sources()  # aborts here rather than 40 live calls later

    plan = runner.plan_matrix(labels, scaffolds, [task["task_id"] for task in tasks],
                              args.repeat, args.seed)
    columns = [(label, scaffold) for label in labels for scaffold in scaffolds]
    print(f"matrix: {len(labels)} agent(s) x {len(scaffolds)} arm(s) x {len(tasks)} task(s) "
          f"x {args.repeat} repeat(s) = {len(plan)} run(s), arm order seeded {args.seed}\n")
    if args.dry_run:
        for label, argv in agents:
            print(f"  {label}: {' '.join(argv)}")
        for entry in plan:
            print(f"  plan  {entry['order_index']:>3}  {entry['agent_label']}/"
                  f"{entry['scaffold']:<5}  {entry['task_id']}"
                  + (f"  #{entry['run_index'] + 1}" if args.repeat > 1 else ""))
        return 0

    cells: Dict[Tuple[str, str, str], List[Dict[str, Any]]] = {}
    sink = runner.RecordSink(args.json_path)
    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="murmur-scaffold-matrix-") as tmp:
        workdir = Path(tmp)
        runner.guard_workspace_root(workdir)
        for entry in plan:
            label, scaffold, task_id = entry["agent_label"], entry["scaffold"], entry["task_id"]
            record = runner.run_agent(
                by_id[task_id], workdir / label / scaffold / task_id, argv_by_label[label],
                scaffold=scaffold, run_index=entry["run_index"], agent_label=label,
                timeout_seconds=args.timeout, seed=args.seed,
                order_index=entry["order_index"],
            )
            cells.setdefault((label, scaffold, task_id), []).append(record)
            sink.append(record)
            arm = f"{label}/{scaffold}"
            if args.repeat > 1:
                arm = f"{arm} {entry['run_index'] + 1}/{args.repeat}"
            print(f"{record['status']:<5}  {task_id:<28} [{arm}]  {record['message']}")

    print()
    print(render_table(tasks, columns, cells, args.repeat))
    transport = render_transport(labels, scaffolds, cells)
    if transport:
        print("\ntransport failures (ERROR — excluded from every pass count above):")
        print("\n".join(transport))
    print("\nscaffold effect (higher is better):")
    print("\n".join(render_deltas(labels, scaffolds, cells)))
    print(f"\n{len(sink.records)} run(s) in {time.monotonic() - started:.1f}s"
          + (f", records written to {args.json_path}" if args.json_path else ""))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
