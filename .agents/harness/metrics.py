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
CONFIG_PATH = Path(__file__).with_name("config.json")
# `--store <path>` accepts every shape an operator naturally has at hand. First
# match wins, so `<store>/v2/tasks` is always preferred over the retired v1
# sibling `<store>/tasks`; a bare directory is accepted only when it already IS
# a task root, otherwise an unrelated path would silently mint phantom tasks.
STORE_CANDIDATES: Tuple[Tuple[str, ...], ...] = (
    ("agent-harness", "v2", "tasks"),
    ("v2", "tasks"),
    ("tasks",),
)
# `attempts[].telemetry.usage` has two vendor dialects; only `input_tokens` and
# `output_tokens` are shared. First present key wins — never sum two aliases.
USAGE_FIELDS: Tuple[Tuple[str, Tuple[str, ...]], ...] = (
    ("input", ("input_tokens",)),
    ("output", ("output_tokens",)),
    ("cached", ("cached_input_tokens", "cache_read_input_tokens")),
    ("cache_write", ("cache_write_input_tokens", "cache_creation_input_tokens")),
    ("reasoning", ("reasoning_output_tokens",)),
)
TOKEN_LABELS: Tuple[str, ...] = tuple(label for label, _ in USAGE_FIELDS)
# Only these are billable. `reasoning` is a SUBSET of `output_tokens` — measured
# strictly less in all 199 corpus records that report it — so adding it inflates
# the total; cache counters are not priced like fresh input.
BILLABLE_LABELS: Tuple[str, ...] = ("input", "output")
PRICE_FIELDS: Tuple[Tuple[str, str], ...] = (
    ("input", "input_per_mtok"),
    ("output", "output_per_mtok"),
)
ACCEPTED_VERDICT = "PASSED"


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


def _task_roots(
    common: Path, stores: Sequence[Path]
) -> List[Tuple[Path, Optional[Path]]]:
    roots: List[Tuple[Path, Optional[Path]]] = [
        (common, common / "agent-harness" / "v2" / "tasks")
    ]
    for store in stores:
        origin = Path(store).expanduser().resolve()
        candidates = [origin.joinpath(*parts) for parts in STORE_CANDIDATES]
        if origin.name == "tasks":
            candidates.append(origin)
        resolved = next(
            (
                candidate
                for candidate in candidates
                if _is_real(candidate, stat.S_IFDIR)
            ),
            None,
        )
        roots.append((origin, resolved))
    return roots


def _discover_root(root: Path, root_index: int) -> Tuple[List[Dict[str, Any]], int]:
    records: List[Dict[str, Any]] = []
    unsafe = 0
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
                "root_index": root_index,
                "root": root,
                "task_dir": task,
                "status": status,
                "last_event_at": last_at,
                "last_epoch": last_epoch,
                "reads": reads,
                "events": events,
            }
        )
    return records, unsafe


def _discover(
    common: Path, stores: Sequence[Path] = ()
) -> Tuple[List[Dict[str, Any]], int, List[Dict[str, Any]]]:
    records: List[Dict[str, Any]] = []
    unsafe = 0
    summaries: List[Dict[str, Any]] = []
    for index, (origin, root) in enumerate(_task_roots(common, stores)):
        found: List[Dict[str, Any]] = []
        if root is not None:
            found, skipped = _discover_root(root, index)
            unsafe += skipped
        records.extend(found)
        summaries.append(
            {
                "path": str(origin),
                "root": None if root is None else str(root),
                "tasks": len(found),
            }
        )
    return records, unsafe, summaries


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


def _load_json_file(path: Path) -> Optional[Any]:
    if not _is_real(path, stat.S_IFREG):
        return None
    try:
        with path.open("r", encoding="utf-8", errors="strict") as handle:
            return json.load(handle)
    except (OSError, UnicodeError, json.JSONDecodeError, ValueError):
        return None


def _safe_name(value: Any) -> Optional[str]:
    if not isinstance(value, str) or not value or value in {".", ".."}:
        return None
    if "/" in value or "\\" in value or "\x00" in value:
        return None
    return value


def _resolve_record(task_dir: Path, event: Mapping[str, Any]) -> Optional[Path]:
    """Resolve store-relatively first so a copied or archived store still joins.

    `record_path` is an absolute path baked in at write time; following it
    blindly makes `--store` useless for any store that ever moved.
    """
    attempt = _safe_name(event.get("attempt_id"))
    kind = _safe_name(event.get("review_kind"))
    if attempt is not None and kind is not None:
        candidate = task_dir / "attempts" / attempt / "reviews" / f"{kind}.json"
        if _is_real(candidate, stat.S_IFREG):
            return candidate
    baked = event.get("record_path")
    if isinstance(baked, str) and baked:
        fallback = Path(baked)
        if _is_real(fallback, stat.S_IFREG):
            return fallback
    return None


def _attempt_green(task_dir: Path, attempt_id: Any) -> Optional[bool]:
    """True when every `checks/*.json` of the attempt recorded a PASS outcome.

    The outcome is nested under `evidence`; the flat `outcome` belongs to the
    `check` EVENT, not to this file.
    """
    attempt = _safe_name(attempt_id)
    if attempt is None:
        return None
    checks = task_dir / "attempts" / attempt / "checks"
    if not _is_real(checks, stat.S_IFDIR):
        return None
    try:
        entries = sorted(checks.iterdir(), key=lambda item: item.name)
    except OSError:
        return None
    outcomes: List[str] = []
    for entry in entries:
        if entry.suffix != ".json":
            continue
        document = _load_json_file(entry)
        if not isinstance(document, Mapping):
            return None
        evidence = document.get("evidence")
        outcome = evidence.get("outcome") if isinstance(evidence, Mapping) else None
        if not isinstance(outcome, str) or not outcome:
            return None
        outcomes.append(outcome)
    if not outcomes:
        return None
    return all(outcome == "PASS" for outcome in outcomes)


def _usage_tokens(record: Mapping[str, Any]) -> Dict[str, Optional[int]]:
    totals: Dict[str, Optional[int]] = {label: None for label in TOKEN_LABELS}
    attempts = record.get("attempts")
    if not isinstance(attempts, Sequence) or isinstance(attempts, (str, bytes)):
        return totals
    for attempt in attempts:
        if not isinstance(attempt, Mapping):
            continue
        telemetry = attempt.get("telemetry")
        usage = telemetry.get("usage") if isinstance(telemetry, Mapping) else None
        if not isinstance(usage, Mapping):
            continue
        for label, names in USAGE_FIELDS:
            for name in names:
                value = usage.get(name)
                if _valid_integer(value):
                    totals[label] = (totals[label] or 0) + int(value)
                    break
    return totals


def _review_rows(
    records: Sequence[Mapping[str, Any]]
) -> Tuple[List[Dict[str, Any]], int]:
    """Deduped `review-checkpoint` events joined to their record and checks.

    The corpus contains checkpoints that fire twice against the same rewritten
    record file; counting events instead of records double-counts their tokens
    and model minutes.
    """
    deduped: Dict[Tuple[Any, ...], Dict[str, Any]] = {}
    duplicates = 0
    green_cache: Dict[Tuple[Any, ...], Optional[bool]] = {}
    for record in records:
        task_key = (record["root_index"], record["task_id"])
        task_dir = record["task_dir"]
        for event in record["events"]:
            if event.get("event") != "review-checkpoint":
                continue
            attempt_id = event.get("attempt_id")
            reviewer = event.get("review_kind")
            key = (task_key, str(attempt_id), str(reviewer))
            if key in deduped:
                duplicates += 1
            green_key = (task_key, str(attempt_id))
            if green_key not in green_cache:
                green_cache[green_key] = _attempt_green(task_dir, attempt_id)
            row: Dict[str, Any] = {
                "task_key": task_key,
                "task_id": record["task_id"],
                "attempt_id": attempt_id,
                "reviewer": reviewer if isinstance(reviewer, str) and reviewer else "UNKNOWN",
                "resolved": "missing",
                "verdict": None,
                "vendor": None,
                "duration_ms": None,
                "proof_gaps": None,
                "findings": {},
                "green_checks": green_cache[green_key],
            }
            row.update({f"tokens_{label}": None for label in TOKEN_LABELS})
            path = _resolve_record(task_dir, event)
            document = None if path is None else _load_json_file(path)
            if path is not None and not isinstance(document, Mapping):
                row["resolved"] = "invalid"
            elif isinstance(document, Mapping):
                row["resolved"] = "available"
                row["record_path"] = str(path)
                kind = document.get("kind")
                if row["reviewer"] == "UNKNOWN" and isinstance(kind, str) and kind:
                    row["reviewer"] = kind
                vendor = document.get("vendor")
                row["vendor"] = vendor if isinstance(vendor, str) and vendor else None
                duration = document.get("duration_ms")
                row["duration_ms"] = duration if _valid_number(duration) else None
                result = document.get("result")
                if isinstance(result, Mapping):
                    verdict = result.get("verdict")
                    row["verdict"] = (
                        verdict if isinstance(verdict, str) and verdict else None
                    )
                    findings = result.get("findings")
                    severities: Dict[str, int] = {}
                    if isinstance(findings, Sequence) and not isinstance(
                        findings, (str, bytes)
                    ):
                        for finding in findings:
                            if not isinstance(finding, Mapping):
                                continue
                            severity = finding.get("severity")
                            name = (
                                severity
                                if isinstance(severity, str) and severity
                                else "UNKNOWN"
                            )
                            severities[name] = severities.get(name, 0) + 1
                    row["findings"] = severities
                    gaps = result.get("proof_gaps")
                    if isinstance(gaps, Sequence) and not isinstance(gaps, (str, bytes)):
                        row["proof_gaps"] = len(gaps)
                for label, total in _usage_tokens(document).items():
                    row[f"tokens_{label}"] = total
            deduped[key] = row
    ordered = sorted(deduped, key=lambda key: (key[0][0], key[0][1], key[1], key[2]))
    return [deduped[key] for key in ordered], duplicates


def _billable(row: Mapping[str, Any]) -> Optional[int]:
    present = [
        int(row[f"tokens_{label}"])
        for label in BILLABLE_LABELS
        if _valid_integer(row.get(f"tokens_{label}"))
    ]
    return sum(present) if present else None


def _row_usd(
    row: Mapping[str, Any], pricing: Mapping[str, Mapping[str, float]]
) -> Optional[float]:
    rate = pricing.get(row.get("vendor"))
    if not isinstance(rate, Mapping):
        return None
    parts = [
        float(row[f"tokens_{label}"]) / 1000000.0 * float(rate[key])
        for label, key in PRICE_FIELDS
        if _valid_integer(row.get(f"tokens_{label}")) and _valid_number(rate.get(key))
    ]
    return math.fsum(parts) if parts else None


def _counted(observations: Sequence[Observation]) -> Dict[str, int]:
    counts: Dict[str, int] = {}
    for state, value in observations:
        if state == "available":
            counts[str(value)] = counts.get(str(value), 0) + 1
    return dict(sorted(counts.items()))


def _reviewer_row(rows: Sequence[Mapping[str, Any]], priced: bool) -> Dict[str, Any]:
    groups = [[row] for row in rows]
    resolution: List[Observation] = [
        (str(row["resolved"]), True if row["resolved"] == "available" else None)
        for row in rows
    ]
    verdicts = _observe(
        groups, "verdict", lambda value: isinstance(value, str) and bool(value)
    )
    by_verdict = _counted(verdicts)
    decided = sum(state == "available" for state, _ in verdicts)
    passes = by_verdict.get("PASS", 0)
    findings: Dict[str, int] = {}
    for row in rows:
        for severity, count in row["findings"].items():
            findings[severity] = findings.get(severity, 0) + count
    tokens: Dict[str, Any] = {
        label: _numeric(_observe(groups, f"tokens_{label}", _valid_integer))
        for label in TOKEN_LABELS
    }
    billable = [value for value in (_billable(row) for row in rows) if value is not None]
    tokens["total"] = sum(billable) if billable else None
    gaps = _numeric(_observe(groups, "proof_gaps", _valid_integer))["total"]
    summary: Dict[str, Any] = {
        "reviews": len(rows),
        "records": _coverage(resolution),
        "pass": passes,
        "pass_rate": round(100.0 * passes / decided, 1) if decided else None,
        "by_verdict": by_verdict,
        "findings": dict(sorted(findings.items())),
        "proof_gaps": 0 if gaps is None else gaps,
        "durations_ms": _numeric(_observe(groups, "duration_ms", _valid_number)),
        "tokens": tokens,
        "by_vendor": _counted(
            _observe(groups, "vendor", lambda value: isinstance(value, str) and bool(value))
        ),
        "green_check_reviews": _true_count(
            _observe(groups, "green_checks", lambda value: isinstance(value, bool))
        ),
    }
    if priced:
        summary["usd"] = _numeric(_observe(groups, "usd", _valid_number))
    return summary


def _review_outcomes(
    rows: Sequence[Mapping[str, Any]],
    duplicates: int,
    pricing: Optional[Mapping[str, Mapping[str, float]]],
) -> Dict[str, Any]:
    by_reviewer: Dict[str, List[Mapping[str, Any]]] = {}
    for row in rows:
        by_reviewer.setdefault(str(row["reviewer"]), []).append(row)
    report = _reviewer_row(rows, pricing is not None)
    report["duplicate_checkpoints"] = duplicates
    report["by_reviewer"] = {
        reviewer: _reviewer_row(by_reviewer[reviewer], pricing is not None)
        for reviewer in sorted(by_reviewer)
    }
    if pricing is not None:
        by_vendor: Dict[str, List[float]] = {}
        for row in rows:
            cost = row.get("usd")
            if _valid_number(cost) and isinstance(row.get("vendor"), str):
                by_vendor.setdefault(str(row["vendor"]), []).append(float(cost))
        usd = report["usd"]
        usd["by_vendor"] = {
            vendor: _number(math.fsum(by_vendor[vendor])) for vendor in sorted(by_vendor)
        }
        usd["by_reviewer"] = {
            reviewer: report["by_reviewer"][reviewer]["usd"]["total"]
            for reviewer in sorted(by_reviewer)
        }
        report["usd"] = usd
    return report


def _task_outcomes(
    records: Sequence[Mapping[str, Any]],
    rows: Sequence[Mapping[str, Any]],
    pricing: Optional[Mapping[str, Mapping[str, float]]],
) -> Dict[str, Any]:
    per_attempt: Dict[Tuple[Any, ...], str] = {}
    per_task: Dict[Tuple[Any, ...], str] = {}
    for record in records:
        task_key = (record["root_index"], record["task_id"])
        for event in record["events"]:
            if event.get("event") != "evidence-checkpoint":
                continue
            verdict = event.get("verdict")
            if not isinstance(verdict, str) or not verdict:
                continue
            attempt = event.get("attempt_id")
            attempt_key = (
                str(attempt)
                if isinstance(attempt, str) and attempt
                else f"_line:{event.get('_ledger')}:{event.get('_line')}"
            )
            per_attempt[(task_key, attempt_key)] = verdict
            per_task[task_key] = verdict
    tokens_by_verdict: Dict[str, int] = {}
    usd_by_verdict: Dict[str, List[float]] = {}
    attributed = 0
    unattributed = 0
    billable_rows = 0
    attributed_usd: List[float] = []
    unattributed_usd: List[float] = []
    for row in rows:
        verdict = per_attempt.get((row["task_key"], str(row["attempt_id"])))
        tokens = _billable(row)
        if tokens is not None:
            billable_rows += 1
            if verdict is None:
                unattributed += tokens
            else:
                tokens_by_verdict[verdict] = tokens_by_verdict.get(verdict, 0) + tokens
                attributed += tokens
        cost = row.get("usd")
        if pricing is not None and _valid_number(cost):
            if verdict is None:
                unattributed_usd.append(float(cost))
            else:
                usd_by_verdict.setdefault(verdict, []).append(float(cost))
                attributed_usd.append(float(cost))
    accepted = sum(verdict == ACCEPTED_VERDICT for verdict in per_task.values())
    total_tokens = attributed + unattributed
    report: Dict[str, Any] = {
        "outcomes": len(per_attempt),
        "by_verdict": _counted([("available", value) for value in per_attempt.values()]),
        "tasks": len(per_task),
        "by_final_verdict": _counted(
            [("available", value) for value in per_task.values()]
        ),
        "accepted_tasks": accepted,
        "tokens": {
            # No priced review is an absent total, never a zero total.
            "total": total_tokens if billable_rows else None,
            "attributed": attributed,
            "unattributed": unattributed,
            "coverage": _coverage(
                [
                    ("available" if _billable(row) is not None else "missing", None)
                    for row in rows
                ]
            ),
        },
        "tokens_by_verdict": dict(sorted(tokens_by_verdict.items())),
        "tokens_per_accepted_task": _number(total_tokens / accepted)
        if accepted and billable_rows
        else None,
    }
    if pricing is not None:
        total_usd = math.fsum(attributed_usd + unattributed_usd)
        report["usd"] = {
            "total": _number(total_usd) if (attributed_usd or unattributed_usd) else None,
            "by_verdict": {
                verdict: _number(math.fsum(usd_by_verdict[verdict]))
                for verdict in sorted(usd_by_verdict)
            },
        }
        report["usd_per_accepted_task"] = (
            _number(total_usd / accepted)
            if accepted and (attributed_usd or unattributed_usd)
            else None
        )
    return report


def _pricing(config_path: Path) -> Optional[Dict[str, Dict[str, float]]]:
    """Vendor -> per-Mtok rates, or None. An absent block means an absent column.

    Read directly, never through `runtime.load_config`: metrics is a lazy,
    stdlib-only read-only extension and must not pull in protocol code.
    """
    document = _load_json_file(config_path)
    if not isinstance(document, Mapping):
        return None
    block = document.get("pricing")
    if not isinstance(block, Mapping):
        return None
    priced: Dict[str, Dict[str, float]] = {}
    for vendor, rate in block.items():
        if not isinstance(vendor, str) or not vendor or not isinstance(rate, Mapping):
            continue
        entry = {
            key: float(rate[key])
            for _, key in PRICE_FIELDS
            if _valid_number(rate.get(key))
        }
        if entry:
            priced[vendor] = dict(sorted(entry.items()))
    return dict(sorted(priced.items())) or None


def collect_metrics(
    common_dir: Path,
    *,
    limit: int = 20,
    stores: Sequence[Path] = (),
    config_path: Optional[Path] = None,
) -> Dict[str, Any]:
    if limit < 1:
        raise ValueError("metrics limit must be positive")
    common = common_dir.resolve()
    records, unsafe, store_summaries = _discover(common, stores)
    records.sort(
        key=lambda record: (
            record["last_epoch"] is not None,
            record["last_epoch"] or 0,
            record["task_id"],
            record["root_index"],
        ),
        reverse=True,
    )
    selected = records[:limit]
    pricing = _pricing(CONFIG_PATH if config_path is None else config_path)
    review_rows, duplicate_checkpoints = _review_rows(selected)
    if pricing is not None:
        for row in review_rows:
            row["usd"] = _row_usd(row, pricing)
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
            "stores": store_summaries,
        },
        "tasks": {
            "count": len(selected),
            "by_status": dict(sorted(status_counts.items())),
            "status_coverage": _coverage(status_observations),
            "records": [
                {
                    "task_id": record["task_id"],
                    "store": str(record["root"]),
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
        "review_outcomes": _review_outcomes(
            review_rows, duplicate_checkpoints, pricing
        ),
        "task_outcomes": _task_outcomes(selected, review_rows, pricing),
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


def _rate_text(value: Optional[float]) -> str:
    return "n/a" if value is None else f"{value:.1f}%"


def _minutes_text(value: Mapping[str, Any]) -> str:
    total = value["total"]
    return "n/a" if total is None else f"{total / 60000.0:.1f}"


def _tokens_text(value: Optional[int]) -> str:
    return "n/a" if value is None else str(value)


def _reviewer_text(name: str, row: Mapping[str, Any]) -> str:
    line = (
        f"  {name}: {row['reviews']} reviews, {row['pass']} PASS "
        f"({_rate_text(row['pass_rate'])}); "
        f"findings {json.dumps(row['findings'], sort_keys=True)}; "
        f"{row['proof_gaps']} proof gaps; "
        f"{row['green_check_reviews']['count']} on all-green checks; "
        f"{_minutes_text(row['durations_ms'])} model min; "
        f"{_tokens_text(row['tokens']['total'])} tokens "
        f"(records {_coverage_text(row['records'])})"
    )
    if "usd" in row:
        line += f"; USD {row['usd']['total']}"
    return line


def _outcome_lines(report: Mapping[str, Any]) -> List[str]:
    reviews = report["review_outcomes"]
    outcomes = report["task_outcomes"]
    header = (
        f"review outcomes: {reviews['reviews']} reviews, {reviews['pass']} PASS "
        f"({_rate_text(reviews['pass_rate'])}); "
        f"verdicts {json.dumps(reviews['by_verdict'], sort_keys=True)}; "
        f"vendors {json.dumps(reviews['by_vendor'], sort_keys=True)}; "
        f"{reviews['duplicate_checkpoints']} duplicate checkpoints skipped; "
        f"records {_coverage_text(reviews['records'])}"
    )
    lines = [header]
    lines.extend(
        _reviewer_text(name, row) for name, row in reviews["by_reviewer"].items()
    )
    lines.append(
        f"task outcomes: {outcomes['outcomes']} verification outcomes over "
        f"{outcomes['tasks']} tasks; "
        f"attempt verdicts {json.dumps(outcomes['by_verdict'], sort_keys=True)}; "
        f"final task verdicts "
        f"{json.dumps(outcomes['by_final_verdict'], sort_keys=True)}"
    )
    accepted = (
        "n/a"
        if outcomes["tokens_per_accepted_task"] is None
        else str(outcomes["tokens_per_accepted_task"])
    )
    cost = (
        f"tokens per accepted task: {accepted} "
        f"({outcomes['accepted_tasks']} accepted tasks; "
        f"{_tokens_text(outcomes['tokens']['total'])} review tokens, "
        f"{outcomes['tokens']['unattributed']} unattributed; "
        f"tokens by verdict "
        f"{json.dumps(outcomes['tokens_by_verdict'], sort_keys=True)})"
    )
    if "usd_per_accepted_task" in outcomes:
        priced = outcomes["usd_per_accepted_task"]
        cost += (
            f"; USD per accepted task "
            f"{'n/a' if priced is None else priced} "
            f"(total USD {outcomes['usd']['total']})"
        )
    lines.append(cost)
    return lines


def _store_lines(selection: Mapping[str, Any]) -> List[str]:
    stores = selection["stores"]
    if len(stores) < 2:
        return []
    return [
        "stores: "
        + "; ".join(
            f"{store['path']} -> "
            f"{'UNRESOLVED' if store['root'] is None else store['root']} "
            f"({store['tasks']} tasks)"
            for store in stores
        )
    ]


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
            *_store_lines(selection),
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
            *_outcome_lines(report),
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
