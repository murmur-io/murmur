#!/usr/bin/env python3
"""Small read-only rollup over append-only Harness event ledgers."""

from __future__ import annotations

import datetime as dt
import json
import math
import os
from pathlib import Path
import re
import stat
from typing import Any, Callable, Dict, List, Mapping, Optional, Sequence, Tuple


Observation = Tuple[str, Any]
RETRY_LABEL = re.compile(r"-try-(\d+)$")


def _utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).replace(microsecond=0).isoformat().replace(
        "+00:00", "Z"
    )


def _is_real(path: Path, kind: int) -> bool:
    try:
        return stat.S_IFMT(os.lstat(path).st_mode) == kind
    except OSError:
        return False


def _timestamp(value: Any) -> Optional[float]:
    if not isinstance(value, str):
        return None
    try:
        parsed = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    return parsed.timestamp() if parsed.tzinfo is not None else None


def _ledger_paths(task: Path) -> Tuple[List[Path], int]:
    candidates = [task / "events.jsonl"]
    unsafe = 0
    attempts = task / "attempts"
    if _is_real(attempts, stat.S_IFDIR):
        try:
            children = sorted(attempts.iterdir(), key=lambda item: item.name)
        except OSError:
            children = []
            unsafe += 1
        attempt_dirs = [
            child for child in children if _is_real(child, stat.S_IFDIR)
        ]
        candidates.extend(child / "events.jsonl" for child in attempt_dirs)
        unsafe += sum(1 for child in children if not _is_real(child, stat.S_IFDIR))
        for attempt in attempt_dirs:
            review_runs = attempt / "review-runs"
            if _is_real(review_runs, stat.S_IFDIR):
                try:
                    review_children = sorted(
                        review_runs.iterdir(), key=lambda item: item.name
                    )
                except OSError:
                    review_children = []
                    unsafe += 1
                review_dirs = [
                    child
                    for child in review_children
                    if _is_real(child, stat.S_IFDIR)
                ]
                candidates.extend(
                    child / "events.jsonl" for child in review_dirs
                )
                unsafe += sum(
                    1
                    for child in review_children
                    if not _is_real(child, stat.S_IFDIR)
                )
            elif review_runs.exists() or review_runs.is_symlink():
                unsafe += 1
    elif attempts.exists() or attempts.is_symlink():
        unsafe += 1
    ledgers: List[Path] = []
    for path in candidates:
        if _is_real(path, stat.S_IFREG):
            ledgers.append(path)
        elif path.exists() or path.is_symlink():
            unsafe += 1
    return ledgers, unsafe


def _read(path: Path) -> Dict[str, Any]:
    result: Dict[str, Any] = {
        "readable": False,
        "lines": 0,
        "valid": 0,
        "malformed": 0,
        "non_object": 0,
        "events": [],
    }
    try:
        with path.open("r", encoding="utf-8", errors="strict") as handle:
            result["readable"] = True
            for line_number, line in enumerate(handle, start=1):
                result["lines"] += 1
                if not line.strip():
                    continue
                try:
                    document = json.loads(line)
                except (json.JSONDecodeError, UnicodeDecodeError):
                    result["malformed"] += 1
                    continue
                if not isinstance(document, dict):
                    result["non_object"] += 1
                    continue
                result["valid"] += 1
                result["events"].append(
                    {**document, "_ledger": str(path), "_line": line_number}
                )
    except (OSError, UnicodeError):
        result["readable"] = False
    return result


def _discover(common: Path) -> Tuple[List[Dict[str, Any]], int]:
    records: List[Dict[str, Any]] = []
    unsafe = 0
    root = common / "agent-harness" / "v2" / "tasks"
    if not _is_real(root, stat.S_IFDIR):
        if root.exists() or root.is_symlink():
            unsafe += 1
        return records, unsafe
    try:
        entries = sorted(root.iterdir(), key=lambda item: item.name)
    except OSError:
        return records, unsafe + 1
    for task in entries:
        if not _is_real(task, stat.S_IFDIR):
            unsafe += int(task.is_symlink())
            continue
        paths, skipped = _ledger_paths(task)
        unsafe += skipped
        reads = [_read(path) for path in paths]
        events = [event for read in reads for event in read["events"]]
        status: Optional[str] = None
        last_at: Optional[str] = None
        last_epoch: Optional[float] = None
        for event in events:
            epoch = _timestamp(event.get("at"))
            if epoch is not None and (last_epoch is None or epoch >= last_epoch):
                last_epoch, last_at = epoch, event["at"]
            if event.get("event") == "state":
                nested = event.get("state")
                candidate = (
                    nested.get("status")
                    if isinstance(nested, Mapping)
                    else event.get("status")
                )
                if isinstance(candidate, str) and candidate:
                    status = candidate
        records.append(
            {
                "task_id": task.name,
                "status": status,
                "last_event_at": last_at,
                "last_epoch": last_epoch,
                "reads": reads,
                "events": events,
            }
        )
    return records, unsafe


def _coverage(observations: Sequence[Observation]) -> Dict[str, Any]:
    available = sum(state == "available" for state, _ in observations)
    missing = sum(state == "missing" for state, _ in observations)
    invalid = len(observations) - available - missing
    return {
        "available": available,
        "missing": missing,
        "invalid": invalid,
        "total": len(observations),
        "percent": round(100.0 * available / len(observations), 1)
        if observations
        else None,
    }


def _valid_number(value: Any) -> bool:
    return (
        isinstance(value, (int, float))
        and not isinstance(value, bool)
        and math.isfinite(float(value))
        and float(value) >= 0
    )


def _valid_integer(value: Any) -> bool:
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


def _observe(
    groups: Sequence[Sequence[Mapping[str, Any]]],
    field: str,
    valid: Callable[[Any], bool],
) -> List[Observation]:
    observations: List[Observation] = []
    for group in groups:
        values = [event[field] for event in group if event.get(field) is not None]
        if not values:
            observations.append(("missing", None))
        elif any(not valid(value) for value in values):
            observations.append(("invalid", None))
        elif any(value != values[0] for value in values[1:]):
            observations.append(("invalid", None))
        else:
            observations.append(("available", values[0]))
    return observations


def _number(value: float) -> Any:
    return int(value) if float(value).is_integer() else round(float(value), 6)


def _numeric(observations: Sequence[Observation]) -> Dict[str, Any]:
    values = sorted(
        float(value) for state, value in observations if state == "available"
    )

    def percentile(fraction: float) -> Optional[Any]:
        if not values:
            return None
        return _number(values[max(0, math.ceil(fraction * len(values)) - 1)])

    return {
        "total": _number(math.fsum(values)) if values else None,
        "p50": percentile(0.50),
        "p90": percentile(0.90),
        "coverage": _coverage(observations),
    }


def _model_key(task_key: str, event: Mapping[str, Any], ordinal: int) -> Tuple[str, ...]:
    for field in ("result_path", "invocation_path", "execution_id"):
        value = event.get(field)
        if isinstance(value, str) and value:
            return task_key, str(event["_ledger"]), field, value
    label = event.get("label")
    if isinstance(label, str) and label:
        return task_key, str(event["_ledger"]), "label", label, str(
            event.get("session_id") or ""
        )
    return task_key, str(event["_ledger"]), "line", str(ordinal)


def _model_groups(records: Sequence[Mapping[str, Any]]) -> List[List[Mapping[str, Any]]]:
    groups: Dict[Tuple[str, ...], List[Mapping[str, Any]]] = {}
    for record in records:
        task_key = str(record["task_id"])
        for ordinal, event in enumerate(record["events"]):
            if event.get("event") not in {
                "model-process-exit",
                "model-invocation",
            }:
                continue
            groups.setdefault(_model_key(task_key, event, ordinal), []).append(event)
    return list(groups.values())


def _true_count(observations: Sequence[Observation]) -> Dict[str, Any]:
    return {
        "count": sum(
            state == "available" and value is True
            for state, value in observations
        ),
        "coverage": _coverage(observations),
    }


def _retries(groups: Sequence[Sequence[Mapping[str, Any]]]) -> Dict[str, Any]:
    labels = _observe(
        groups, "label", lambda value: isinstance(value, str) and bool(value)
    )
    return {
        "count": sum(
            state == "available"
            and (match := RETRY_LABEL.search(str(label))) is not None
            and int(match.group(1)) >= 2
            for state, label in labels
        ),
        "label_coverage": _coverage(labels),
    }


def _models(groups: Sequence[Sequence[Mapping[str, Any]]]) -> Dict[str, Any]:
    roles = _observe(
        groups, "role", lambda value: isinstance(value, str) and bool(value)
    )
    by_role: Dict[str, int] = {}
    for state, role in roles:
        if state == "available":
            by_role[str(role)] = by_role.get(str(role), 0) + 1
    costs = _observe(groups, "total_cost_usd", _valid_number)
    turns = _observe(groups, "num_turns", _valid_integer)
    http = _observe(
        groups,
        "http_status",
        lambda value: isinstance(value, int)
        and not isinstance(value, bool)
        and 100 <= value <= 599,
    )
    return {
        "invocations": len(groups),
        "by_role": dict(sorted(by_role.items())),
        "role_coverage": _coverage(roles),
        "total_cost_usd": _numeric(costs)["total"],
        "cost_usd_coverage": _coverage(costs),
        "total_num_turns": _numeric(turns)["total"],
        "num_turns_coverage": _coverage(turns),
        "durations_ms": _numeric(_observe(groups, "duration_ms", _valid_number)),
        "timeouts": _true_count(
            _observe(groups, "timed_out", lambda value: isinstance(value, bool))
        ),
        "retry_invocations": _retries(groups),
        "retryable_http_failures": {
            "count": sum(
                state == "available" and (value == 429 or value >= 500)
                for state, value in http
            ),
            "coverage": _coverage(http),
        },
    }


def _reviews(groups: Sequence[Sequence[Mapping[str, Any]]]) -> Dict[str, Any]:
    selected = [
        group
        for group in groups
        if _observe(
            [group], "role", lambda value: isinstance(value, str) and bool(value)
        )[0]
        == ("available", "reviewer")
    ]
    return {
        "attempts": len(selected),
        "durations_ms": _numeric(
            _observe(selected, "duration_ms", _valid_number)
        ),
        "timeouts": _true_count(
            _observe(selected, "timed_out", lambda value: isinstance(value, bool))
        ),
        "retry_invocations": _retries(selected),
    }


def _checks(records: Sequence[Mapping[str, Any]]) -> Dict[str, Any]:
    events = [
        event
        for record in records
        for event in record["events"]
        if event.get("event") == "check"
    ]
    groups = [[event] for event in events]
    outcomes = _observe(
        groups, "outcome", lambda value: isinstance(value, str) and bool(value)
    )
    by_outcome: Dict[str, int] = {}
    for state, outcome in outcomes:
        if state == "available":
            by_outcome[str(outcome)] = by_outcome.get(str(outcome), 0) + 1
    return {
        "runs": len(events),
        "by_outcome": dict(sorted(by_outcome.items())),
        "outcome_coverage": _coverage(outcomes),
        "durations_ms": _numeric(
            _observe(groups, "duration_ms", _valid_number)
        ),
        "timeouts": _true_count(
            _observe(groups, "timed_out", lambda value: isinstance(value, bool))
        ),
    }


def collect_metrics(common_dir: Path, *, limit: int = 20) -> Dict[str, Any]:
    if limit < 1:
        raise ValueError("metrics limit must be positive")
    common = common_dir.resolve()
    records, unsafe = _discover(common)
    records.sort(
        key=lambda record: (
            record["last_epoch"] is not None,
            record["last_epoch"] or 0,
            record["task_id"],
        ),
        reverse=True,
    )
    selected = records[:limit]
    status_counts: Dict[str, int] = {}
    for record in selected:
        status = record["status"] or "UNKNOWN"
        status_counts[status] = status_counts.get(status, 0) + 1
    status_observations = [
        ("available", record["status"])
        if record["status"] is not None
        else ("missing", None)
        for record in selected
    ]
    model_groups = _model_groups(selected)
    reads = [read for record in selected for read in record["reads"]]
    retryable_states = sum(
        event.get("event") == "state"
        and (
            event.get("state", {}).get("status")
            if isinstance(event.get("state"), Mapping)
            else event.get("status")
        )
        == "PAUSED_RETRYABLE"
        for record in selected
        for event in record["events"]
    )
    return {
        "schema_version": 1,
        "generated_at": _utc_now(),
        "git_common_dir": str(common),
        "selection": {
            "limit": limit,
            "discovered_tasks": len(records),
            "selected_tasks": len(selected),
            "order": "latest valid event timestamp descending; task id tie-break",
        },
        "tasks": {
            "count": len(selected),
            "by_status": dict(sorted(status_counts.items())),
            "status_coverage": _coverage(status_observations),
            "records": [
                {
                    "task_id": record["task_id"],
                    "status": record["status"] or "UNKNOWN",
                    "last_event_at": record["last_event_at"],
                    "event_ledgers": len(record["reads"]),
                }
                for record in selected
            ],
        },
        "models": _models(model_groups),
        "checks": _checks(selected),
        "reviews": _reviews(model_groups),
        "events": {
            "ledgers": {
                "discovered": len(reads),
                "readable": sum(read["readable"] for read in reads),
                "unreadable": sum(not read["readable"] for read in reads),
            },
            "lines": {
                "total": sum(read["lines"] for read in reads),
                "valid_objects": sum(read["valid"] for read in reads),
                "malformed": sum(read["malformed"] for read in reads),
                "non_object": sum(read["non_object"] for read in reads),
            },
            "retryable_state_transitions": retryable_states,
        },
        "unsafe_entries_skipped": unsafe,
    }


def _coverage_text(value: Mapping[str, Any]) -> str:
    percent = "n/a" if value["percent"] is None else f"{value['percent']:.1f}%"
    return (
        f"{value['available']}/{value['total']} ({percent}; "
        f"missing {value['missing']}, invalid {value['invalid']})"
    )


def _duration_text(value: Mapping[str, Any]) -> str:
    summary = (
        "n/a"
        if value["p50"] is None
        else f"p50 {value['p50']} ms, p90 {value['p90']} ms, total {value['total']} ms"
    )
    return f"{summary}; coverage {_coverage_text(value['coverage'])}"


def render_text(report: Mapping[str, Any]) -> str:
    selection = report["selection"]
    tasks = report["tasks"]
    models = report["models"]
    checks = report["checks"]
    reviews = report["reviews"]
    events = report["events"]
    cost = (
        "n/a"
        if models["total_cost_usd"] is None
        else f"${models['total_cost_usd']:.6f}"
    )
    turns = (
        "n/a"
        if models["total_num_turns"] is None
        else str(models["total_num_turns"])
    )
    return "\n".join(
        [
            (
                f"Harness metrics: {selection['selected_tasks']}/"
                f"{selection['discovered_tasks']} most recent tasks "
                f"(limit {selection['limit']})"
            ),
            (
                f"tasks: status {json.dumps(tasks['by_status'], sort_keys=True)}; "
                f"coverage {_coverage_text(tasks['status_coverage'])}"
            ),
            (
                f"models: {models['invocations']} invocations; observed cost {cost} "
                f"(coverage {_coverage_text(models['cost_usd_coverage'])}); "
                f"observed turns {turns} "
                f"(coverage {_coverage_text(models['num_turns_coverage'])})"
            ),
            f"model durations: {_duration_text(models['durations_ms'])}",
            (
                f"reviews: {reviews['attempts']} attempts, "
                f"{reviews['retry_invocations']['count']} retries, "
                f"{reviews['timeouts']['count']} timeouts; "
                f"durations {_duration_text(reviews['durations_ms'])}"
            ),
            (
                f"checks: {checks['runs']} runs, {checks['timeouts']['count']} timeouts; "
                f"outcomes {json.dumps(checks['by_outcome'], sort_keys=True)}; "
                f"durations {_duration_text(checks['durations_ms'])}"
            ),
            (
                f"retry signals: {models['retry_invocations']['count']} model retries, "
                f"{models['retryable_http_failures']['count']} observed HTTP 429/5xx "
                f"exits, {events['retryable_state_transitions']} PAUSED_RETRYABLE"
            ),
            (
                f"ledgers: {events['ledgers']['readable']}/"
                f"{events['ledgers']['discovered']} readable; "
                f"{events['lines']['valid_objects']}/{events['lines']['total']} valid; "
                f"{events['lines']['malformed']} malformed; "
                f"{report['unsafe_entries_skipped']} unsafe entries skipped"
            ),
        ]
    )
