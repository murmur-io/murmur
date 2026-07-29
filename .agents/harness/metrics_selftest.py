#!/usr/bin/env python3
"""Deterministic selftest for Harness telemetry aggregation."""

from __future__ import annotations

import json
from pathlib import Path
import tempfile
from typing import Any, Mapping

import metrics


class Tests:
    def __init__(self) -> None:
        self.assertions = 0
        self.failures: list[str] = []

    def equal(self, name: str, actual: Any, expected: Any) -> None:
        self.assertions += 1
        if actual != expected:
            self.failures.append(f"{name}: expected {expected!r}, got {actual!r}")

    def true(self, name: str, condition: bool) -> None:
        self.assertions += 1
        if not condition:
            self.failures.append(name)


def _append(path: Path, *documents: Any, raw: str = "") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as handle:
        for document in documents:
            handle.write(json.dumps(document, sort_keys=True) + "\n")
        if raw:
            handle.write(raw)


def _state(at: str, status: str) -> Mapping[str, Any]:
    return {
        "at": at,
        "event": "state",
        "state": {
            "schema_version": 2,
            "task_id": "fixture",
            "status": status,
            "updated_at": at,
        },
    }


def cases(test: Tests) -> None:
    with tempfile.TemporaryDirectory(prefix="murmur-metrics-") as raw:
        common = Path(raw) / ".git"
        tasks = common / "agent-harness" / "v2" / "tasks"

        task_a = tasks / "task-a" / "events.jsonl"
        _append(
            task_a,
            _state("2026-07-01T10:00:00Z", "OPEN"),
            {
                "at": "2026-07-01T10:01:00Z",
                "event": "model-invocation",
                "label": "review-combined-try-1",
                "role": "reviewer",
                "vendor": "claude",
                "result_path": "/task-a/review.json",
                "duration_ms": 1000,
                "timed_out": False,
            },
            {
                "at": "2026-07-01T10:02:00Z",
                "event": "check",
                "id": "rust-lib",
                "duration_ms": 300,
                "timed_out": False,
                "outcome": "PASS",
            },
            _state("2026-07-01T10:03:00Z", "CLOSED"),
        )

        task_b = tasks / "task-b" / "events.jsonl"
        process = {
            "at": "2026-07-02T10:01:00Z",
            "event": "model-process-exit",
            "label": "round-01-spec",
            "role": "reviewer",
            "vendor": "codex",
            "result_path": "/task-b/spec.json",
            "execution_id": "exec-b",
            "duration_ms": 2000,
            "total_cost_usd": 1.5,
            "num_turns": 3,
            "http_status": 429,
            "timed_out": False,
        }
        invocation = {
            **process,
            "event": "model-invocation",
            "session_id": "session-b",
        }
        _append(
            task_b,
            _state("2026-07-02T10:00:00Z", "VERIFYING"),
            process,
            invocation,
            {
                "at": "2026-07-02T10:02:00Z",
                "event": "check",
                "id": "tauri-boot",
                "duration_ms": 500,
                "timed_out": True,
                "outcome": "BLOCKED",
            },
            _state("2026-07-02T10:03:00Z", "BLOCKED"),
            [1, 2, 3],
            raw="{not-json}\n",
        )

        task_c_top = tasks / "task-c" / "events.jsonl"
        _append(
            task_c_top,
            _state("2026-07-03T10:00:00Z", "OPEN"),
            {
                "at": "2026-07-03T10:01:00Z",
                "event": "check",
                "id": "ng-build",
                "duration_ms": 700,
                "timed_out": False,
                "outcome": "PASS",
            },
            _state("2026-07-03T10:05:00Z", "PAUSED_RETRYABLE"),
            _state("2026-07-03T10:06:00Z", "PASSED"),
        )
        task_c_review = (
            tasks
            / "task-c"
            / "attempts"
            / "attempt-1"
            / "review-runs"
            / "combined"
            / "events.jsonl"
        )
        for number, duration, cost, turns, timed_out in (
            (1, 3000, 2.0, 4, True),
            (2, 4000, 3.0, 5, False),
        ):
            model_exit = {
                "at": f"2026-07-03T10:0{number + 1}:00Z",
                "event": "model-process-exit",
                "label": f"review-combined-try-{number}",
                "role": "reviewer",
                "vendor": "claude",
                "result_path": f"/task-c/review-{number}.json",
                "execution_id": f"exec-c-{number}",
                "duration_ms": duration,
                "total_cost_usd": cost,
                "num_turns": turns,
                "timed_out": timed_out,
            }
            _append(
                task_c_review,
                model_exit,
                {**model_exit, "event": "model-invocation"},
            )

        report = metrics.collect_metrics(common, limit=20)
        test.equal("TASK all tasks counted", report["tasks"]["count"], 3)
        test.equal(
            "TASK statuses come only from authoritative state events",
            report["tasks"]["by_status"],
            {"BLOCKED": 1, "CLOSED": 1, "PASSED": 1},
        )
        test.equal(
            "MODEL process-exit plus invocation is deduplicated",
            report["models"]["invocations"],
            4,
        )
        test.equal(
            "MODEL absent cost is not guessed",
            report["models"]["cost_usd_coverage"],
            {
                "available": 3,
                "missing": 1,
                "invalid": 0,
                "total": 4,
                "percent": 75.0,
            },
        )
        test.equal(
            "MODEL observed costs sum only covered invocations",
            report["models"]["total_cost_usd"],
            6.5,
        )
        test.equal(
            "MODEL observed turns sum only covered invocations",
            report["models"]["total_num_turns"],
            12,
        )
        test.equal(
            "MODEL nearest-rank p50 is stable",
            report["models"]["durations_ms"]["p50"],
            2000,
        )
        test.equal(
            "MODEL nearest-rank p90 is stable",
            report["models"]["durations_ms"]["p90"],
            4000,
        )
        test.equal(
            "REVIEW attempts are role-derived",
            report["reviews"]["attempts"],
            4,
        )
        test.equal(
            "REVIEW retry labels count only extra attempts",
            report["reviews"]["retry_invocations"]["count"],
            1,
        )
        test.equal(
            "REVIEW timeouts retain typed evidence",
            report["reviews"]["timeouts"]["count"],
            1,
        )
        test.equal(
            "LEDGER production review-runs depth is discovered",
            report["events"]["ledgers"]["discovered"],
            4,
        )
        test.equal(
            "CHECK runs include all tasks",
            report["checks"]["runs"],
            3,
        )
        test.equal(
            "CHECK p50 is stable",
            report["checks"]["durations_ms"]["p50"],
            500,
        )
        test.equal(
            "CHECK timeout is counted",
            report["checks"]["timeouts"]["count"],
            1,
        )
        test.equal(
            "RETRY HTTP status is reported without inference",
            report["models"]["retryable_http_failures"]["count"],
            1,
        )
        test.equal(
            "RETRYABLE state transition is counted",
            report["events"]["retryable_state_transitions"],
            1,
        )
        test.equal(
            "LEDGER malformed JSON remains visible",
            report["events"]["lines"]["malformed"],
            1,
        )
        test.equal(
            "LEDGER non-object JSON remains visible",
            report["events"]["lines"]["non_object"],
            1,
        )
        test.equal(
            "LIMIT selects newest tasks",
            [
                item["task_id"]
                for item in metrics.collect_metrics(common, limit=2)["tasks"]["records"]
            ],
            ["task-c", "task-b"],
        )
        text = metrics.render_text(report)
        test.true(
            "TEXT output makes coverage explicit",
            "coverage 3/4 (75.0%; missing 1, invalid 0)" in text,
        )

        empty = metrics.collect_metrics(Path(raw) / "empty.git", limit=5)
        test.equal("EMPTY totals stay unavailable", empty["models"]["total_cost_usd"], None)
        test.equal(
            "EMPTY coverage does not invent a percentage",
            empty["models"]["cost_usd_coverage"]["percent"],
            None,
        )
        try:
            metrics.collect_metrics(common, limit=0)
        except ValueError as exc:
            test.true("LIMIT zero fails clearly", "positive" in str(exc))
        else:
            test.true("LIMIT zero fails clearly", False)


def main() -> int:
    test = Tests()
    cases(test)
    if test.failures:
        for failure in test.failures:
            print(f"metrics-selftest: FAIL: {failure}")
        return 1
    print(f"metrics-selftest: PASS ({test.assertions} assertions)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
