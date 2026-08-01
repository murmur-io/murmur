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


def _write_json(path: Path, document: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(document, sort_keys=True), encoding="utf-8")


def _review_record(
    kind: str,
    vendor: str,
    verdict: str,
    duration_ms: int,
    usage: Mapping[str, Any],
    severities: Any = (),
    proof_gaps: Any = (),
    cost_usd: Any = None,
) -> Mapping[str, Any]:
    return {
        "schema_version": 2,
        "kind": kind,
        "label": f"review-{kind}-try-1",
        "vendor": vendor,
        "model": "unspecified",
        "created_at": "2026-07-05T10:00:00Z",
        "duration_ms": duration_ms,
        "attempts": [
            {
                "attempt": 1,
                "created_at": "2026-07-05T10:00:00Z",
                "duration_ms": duration_ms,
                "label": f"review-{kind}-try-1",
                "ok": True,
                "transient": False,
                "vendor": vendor,
                "telemetry": {"cost_usd": cost_usd, "usage": dict(usage)},
            }
        ],
        "result": {
            "verdict": verdict,
            "summary": f"{kind} {verdict}",
            "requirements_covered": [],
            "findings": [
                {
                    "severity": severity,
                    "file": "src-tauri/src/lib.rs",
                    "evidence": "fixture",
                    "required_fix": "fixture",
                }
                for severity in severities
            ],
            "proof_gaps": list(proof_gaps),
            "probe_requests": [],
        },
    }


def _check_record(check_id: str, outcome: str) -> Mapping[str, Any]:
    return {
        "schema_version": 2,
        "id": check_id,
        "command": f"run {check_id}",
        "created_at": "2026-07-05T10:00:00Z",
        "evidence": {
            "outcome": outcome,
            "passed": outcome == "PASS",
            "exit_code": 0 if outcome == "PASS" else 1,
            "duration_ms": 1000,
        },
    }


def _review_checkpoint(
    at: str, attempt_id: str, kind: str, record_path: Path, verdict: str
) -> Mapping[str, Any]:
    return {
        "at": at,
        "event": "review-checkpoint",
        "attempt_id": attempt_id,
        "review_kind": kind,
        "record_path": str(record_path),
        "verdict": verdict,
    }


def _evidence_checkpoint(
    at: str, attempt_id: str, evidence_path: Path, verdict: str
) -> Mapping[str, Any]:
    return {
        "at": at,
        "event": "evidence-checkpoint",
        "attempt_id": attempt_id,
        "evidence_path": str(evidence_path),
        "evidence_sha256": "0" * 64,
        "verdict": verdict,
    }


def _outcome_fixture(tasks: Path) -> None:
    """Two reviewed tasks (one PASSED, one NEEDS_FIX) plus one unreviewed PASSED task."""
    passed = tasks / "task-p"
    passed_attempt = passed / "attempts" / "att-p"
    combined_p = passed_attempt / "reviews" / "combined.json"
    _write_json(
        combined_p,
        _review_record(
            "combined",
            "codex",
            "PASS",
            60000,
            {
                "input_tokens": 1000,
                "output_tokens": 100,
                "cached_input_tokens": 200,
                "cache_write_input_tokens": 400,
                "reasoning_output_tokens": 10,
            },
            ["MINOR"],
            ["gap-p"],
        ),
    )
    lock_p = passed_attempt / "reviews" / "lock-security.json"
    _write_json(
        lock_p,
        _review_record(
            "lock-security",
            "codex",
            "PASS",
            30000,
            {"input_tokens": 500, "output_tokens": 50},
        ),
    )
    _write_json(passed_attempt / "checks" / "rust-lib.json", _check_record("rust-lib", "PASS"))
    passed_evidence = passed_attempt / "evidence.json"
    _write_json(passed_evidence, {"schema_version": 2, "verdict": "PASSED"})
    _append(
        passed / "events.jsonl",
        _state("2026-07-05T10:00:00Z", "OPEN"),
        _review_checkpoint("2026-07-05T10:01:00Z", "att-p", "combined", combined_p, "PASS"),
        _review_checkpoint(
            "2026-07-05T10:02:00Z", "att-p", "lock-security", lock_p, "PASS"
        ),
        _evidence_checkpoint("2026-07-05T10:03:00Z", "att-p", passed_evidence, "PASSED"),
        _state("2026-07-05T10:04:00Z", "COMMITTED"),
    )

    needs_fix = tasks / "task-n"
    needs_fix_attempt = needs_fix / "attempts" / "att-n"
    combined_n = needs_fix_attempt / "reviews" / "combined.json"
    _write_json(
        combined_n,
        _review_record(
            "combined",
            "claude",
            "FAIL",
            90000,
            # Copied verbatim from a real claude review record in the corpus.
            # The Anthropic dialect puts essentially the whole prompt in the
            # cache pair and leaves `input_tokens` at a literal 2, so a fixture
            # that models it as a plausible-looking 2000 cannot tell the two
            # dialects apart and the normalization assertion goes vacuous.
            {
                "input_tokens": 2,
                "output_tokens": 21730,
                "cache_read_input_tokens": 3871,
                "cache_creation_input_tokens": 57500,
            },
            ["MAJOR", "BLOCKER"],
            ["gap-a", "gap-b"],
            cost_usd=1.45,
        ),
    )
    _write_json(needs_fix_attempt / "checks" / "ng-lint.json", _check_record("ng-lint", "FAIL"))
    absent = needs_fix_attempt / "reviews" / "egress-security.json"
    needs_fix_evidence = needs_fix_attempt / "evidence.json"
    _write_json(needs_fix_evidence, {"schema_version": 2, "verdict": "NEEDS_FIX"})
    _append(
        needs_fix / "events.jsonl",
        _state("2026-07-06T10:00:00Z", "OPEN"),
        _review_checkpoint("2026-07-06T10:01:00Z", "att-n", "combined", combined_n, "FAIL"),
        # Same attempt, same reviewer, rewritten record: one review, two events.
        _review_checkpoint("2026-07-06T10:02:00Z", "att-n", "combined", combined_n, "FAIL"),
        # A checkpoint whose record was pruned: absent must stay absent, never zero.
        _review_checkpoint(
            "2026-07-06T10:03:00Z", "att-n", "egress-security", absent, "PASS"
        ),
        _evidence_checkpoint(
            "2026-07-06T10:04:00Z", "att-n", needs_fix_evidence, "NEEDS_FIX"
        ),
        _state("2026-07-06T10:05:00Z", "NEEDS_FIX"),
    )

    quiet = tasks / "task-q"
    quiet_evidence = quiet / "attempts" / "att-q" / "evidence.json"
    _write_json(quiet_evidence, {"schema_version": 2, "verdict": "PASSED"})
    _append(
        quiet / "events.jsonl",
        _state("2026-07-07T10:00:00Z", "OPEN"),
        _evidence_checkpoint("2026-07-07T10:01:00Z", "att-q", quiet_evidence, "PASSED"),
        _state("2026-07-07T10:02:00Z", "COMMITTED"),
    )


def outcome_cases(test: Tests) -> None:
    with tempfile.TemporaryDirectory(prefix="murmur-metrics-") as raw:
        base = Path(raw)
        common = base / ".git"
        _outcome_fixture(common / "agent-harness" / "v2" / "tasks")
        # An explicit unpriced config keeps these assertions independent of
        # whether the shipped harness config ever gains a "pricing" block.
        unpriced = base / "unpriced-config.json"
        unpriced.write_text(json.dumps({"schema_version": 2}), encoding="utf-8")
        report = metrics.collect_metrics(common, limit=20, config_path=unpriced)
        reviews = report["review_outcomes"]
        outcomes = report["task_outcomes"]

        test.equal(
            "REVIEW-OUTCOME duplicate checkpoints collapse to one review",
            (reviews["reviews"], reviews["duplicate_checkpoints"]),
            (4, 1),
        )
        test.equal(
            "REVIEW-OUTCOME a pruned record is missing, never zero",
            reviews["records"],
            {"available": 3, "missing": 1, "invalid": 0, "total": 4, "percent": 75.0},
        )
        test.equal(
            "REVIEW-OUTCOME PASS-rate is computed over resolved records only",
            (reviews["pass"], reviews["pass_rate"], reviews["by_verdict"]),
            (2, 66.7, {"FAIL": 1, "PASS": 2}),
        )
        test.equal(
            "REVIEW-OUTCOME findings are counted by severity",
            reviews["findings"],
            {"BLOCKER": 1, "MAJOR": 1, "MINOR": 1},
        )
        test.equal("REVIEW-OUTCOME proof gaps are counted", reviews["proof_gaps"], 3)
        test.equal(
            "REVIEW-OUTCOME model time sums record durations",
            (
                reviews["durations_ms"]["total"],
                reviews["durations_ms"]["p50"],
                reviews["durations_ms"]["p90"],
            ),
            (180000, 60000, 90000),
        )
        test.equal(
            "REVIEW-OUTCOME both vendor usage dialects are normalized",
            (
                reviews["tokens"]["input"]["total"],
                reviews["tokens"]["output"]["total"],
                reviews["tokens"]["cached"]["total"],
                reviews["tokens"]["cache_write"]["total"],
            ),
            (1502, 21880, 4071, 57900),
        )
        test.equal(
            "REVIEW-OUTCOME the usage dialect is attributed per record",
            reviews["by_dialect"],
            {"anthropic": 1, "openai": 2},
        )
        # codex: 1000+100 and 500+50 — `cached_input_tokens` is a SUBSET of
        # `input_tokens` there, so adding it would double-count.
        # claude: 2+3871+57500+21730 — those cache counters are DISJOINT from
        # `input_tokens`, so billing `input + output` alone would score this
        # 96k-token review at 21732 tokens, 26% of what it consumed.
        test.equal(
            "REVIEW-OUTCOME billable tokens follow each record's own dialect",
            reviews["tokens"]["total"],
            1100 + 550 + 83103,
        )
        test.equal(
            "REVIEW-OUTCOME the Anthropic dialect bills its cache counters",
            reviews["by_reviewer"]["combined"]["tokens"]["total"] - 1100,
            83103,
        )
        test.equal(
            "REVIEW-OUTCOME reasoning tokens are reported but never billed",
            (reviews["tokens"]["reasoning"]["total"], reviews["tokens"]["total"]),
            (10, 84753),
        )
        test.equal(
            "REVIEW-OUTCOME the vendor's own measured cost is surfaced",
            (
                reviews["observed_usd"]["total"],
                reviews["observed_usd"]["coverage"]["available"],
                reviews["observed_usd"]["coverage"]["missing"],
            ),
            (1.45, 1, 3),
        )
        test.equal(
            "REVIEW-OUTCOME absent cache counters stay uncovered",
            reviews["tokens"]["cached"]["coverage"],
            {"available": 2, "missing": 2, "invalid": 0, "total": 4, "percent": 50.0},
        )
        test.equal(
            "REVIEW-OUTCOME vendors are attributed from the record",
            reviews["by_vendor"],
            {"claude": 1, "codex": 2},
        )
        test.equal(
            "REVIEW-OUTCOME green-check reviews read the nested check outcome",
            reviews["green_check_reviews"],
            {
                "count": 2,
                "coverage": {
                    "available": 4,
                    "missing": 0,
                    "invalid": 0,
                    "total": 4,
                    "percent": 100.0,
                },
            },
        )
        test.equal(
            "REVIEW-OUTCOME reviewer rows are ordered deterministically",
            list(reviews["by_reviewer"]),
            ["combined", "egress-security", "lock-security"],
        )
        combined = reviews["by_reviewer"]["combined"]
        test.equal(
            "REVIEW-OUTCOME per-reviewer PASS-rate is per reviewer",
            (combined["reviews"], combined["pass"], combined["pass_rate"]),
            (2, 1, 50.0),
        )
        test.equal(
            "REVIEW-OUTCOME per-reviewer findings and gaps are per reviewer",
            (combined["findings"], combined["proof_gaps"]),
            ({"BLOCKER": 1, "MAJOR": 1, "MINOR": 1}, 3),
        )
        test.equal(
            "REVIEW-OUTCOME per-reviewer tokens and minutes are per reviewer",
            (combined["tokens"]["total"], combined["durations_ms"]["total"]),
            (84203, 150000),
        )
        test.equal(
            "REVIEW-OUTCOME a reviewer with no resolved record invents no rate",
            (
                reviews["by_reviewer"]["egress-security"]["pass_rate"],
                reviews["by_reviewer"]["egress-security"]["tokens"]["total"],
            ),
            (None, None),
        )
        test.equal(
            "TASK-OUTCOME verification verdicts are counted per attempt",
            (outcomes["outcomes"], outcomes["by_verdict"]),
            (3, {"NEEDS_FIX": 1, "PASSED": 2}),
        )
        test.equal(
            "TASK-OUTCOME final task verdict is the last checkpoint",
            (outcomes["tasks"], outcomes["by_final_verdict"], outcomes["accepted_tasks"]),
            (3, {"NEEDS_FIX": 1, "PASSED": 2}, 2),
        )
        test.equal(
            "TASK-OUTCOME review tokens are attributed to the outcome that consumed them",
            outcomes["tokens_by_verdict"],
            {"NEEDS_FIX": 83103, "PASSED": 1650},
        )
        test.equal(
            "TASK-OUTCOME cost per accepted task divides only by accepted tasks",
            (outcomes["tokens"]["total"], outcomes["tokens_per_accepted_task"]),
            (84753, 42376.5),
        )
        test.equal(
            "TASK-OUTCOME observed cost per accepted task needs no rate card",
            (
                outcomes["observed_usd"]["total"],
                outcomes["observed_usd_per_accepted_task"],
            ),
            (1.45, 0.725),
        )
        test.equal(
            "TASK-OUTCOME an unpriceable review is uncovered, not free",
            outcomes["tokens"]["coverage"],
            {"available": 3, "missing": 1, "invalid": 0, "total": 4, "percent": 75.0},
        )
        test.true(
            "PRICING absent block means absent column",
            "usd" not in reviews and "usd_per_accepted_task" not in outcomes,
        )
        text = metrics.render_text(report)
        test.true(
            "TEXT review outcomes name the PASS-rate",
            "review outcomes: 4 reviews, 2 PASS (66.7%)" in text,
        )
        test.true(
            "TEXT per-reviewer row is rendered",
            "combined: 2 reviews, 1 PASS (50.0%)" in text,
        )
        test.true(
            "TEXT cost per accepted task is rendered",
            "tokens per accepted task: 42376.5" in text,
        )
        test.true(
            "TEXT omits the rate-card column when unpriced",
            "rate-card USD" not in text,
        )
        test.true(
            "TEXT reports the measured cost even with no rate card",
            "observed USD per accepted task 0.725" in text,
        )

        first = metrics.collect_metrics(common, limit=20, config_path=unpriced)
        second = metrics.collect_metrics(common, limit=20, config_path=unpriced)
        first.pop("generated_at")
        second.pop("generated_at")
        test.equal("REVIEW-OUTCOME the whole report is deterministic", first, second)
        test.equal(
            "PRICING the shipped harness config is the default source",
            "usd" in metrics.collect_metrics(common, limit=20)["review_outcomes"],
            metrics._pricing(metrics.CONFIG_PATH) is not None,
        )

        priced = base / "priced-config.json"
        priced.write_text(
            json.dumps(
                {
                    "schema_version": 2,
                    "pricing": {
                        "codex": {"input_per_mtok": 2.0, "output_per_mtok": 10.0},
                        "claude": {"input_per_mtok": 5.0, "output_per_mtok": 25.0},
                    },
                },
                sort_keys=True,
            ),
            encoding="utf-8",
        )
        with_usd = metrics.collect_metrics(common, limit=20, config_path=priced)
        test.equal(
            "PRICING per-vendor rates price exactly the tokens each dialect bills",
            with_usd["review_outcomes"]["usd"]["by_vendor"],
            # claude: (2 + 3871 + 57500)/1e6*5 + 21730/1e6*25
            # codex:  (1000 + 500)/1e6*2 + (100 + 50)/1e6*10
            {"claude": 0.850115, "codex": 0.0045},
        )
        test.equal(
            "PRICING total USD and USD per accepted task follow the same join",
            (
                with_usd["review_outcomes"]["usd"]["total"],
                with_usd["task_outcomes"]["usd_per_accepted_task"],
            ),
            (0.854615, 0.427308),
        )
        test.true(
            "PRICING a malformed block is ignored rather than guessed",
            metrics.collect_metrics(
                common, limit=20, config_path=base / "no-such-config.json"
            )["review_outcomes"].get("usd")
            is None,
        )

        store = base / "elsewhere" / "agent-harness"
        _outcome_fixture(store / "v2" / "tasks")
        for label, candidate in (
            ("git common dir", store.parent),
            ("store root", store),
            ("v2 dir", store / "v2"),
            ("tasks dir", store / "v2" / "tasks"),
        ):
            merged = metrics.collect_metrics(
                common, limit=20, stores=[candidate], config_path=unpriced
            )
            test.equal(
                f"STORE {label} resolves to the same task root",
                (
                    merged["selection"]["discovered_tasks"],
                    merged["review_outcomes"]["reviews"],
                ),
                (6, 8),
            )
        unresolved = metrics.collect_metrics(
            common, limit=20, stores=[base], config_path=unpriced
        )
        test.equal(
            "STORE an unresolvable store is reported, not silently empty",
            unresolved["selection"]["stores"][1]["root"],
            None,
        )

        # Absolute counts double while every rate stays identical, so nothing in
        # the output looks anomalous. `--store <this repo's own store>` run from
        # the driver clone is the documented command's own failure mode.
        def totals(report: Mapping[str, Any]) -> Any:
            return (
                report["selection"]["discovered_tasks"],
                report["review_outcomes"]["reviews"],
                report["review_outcomes"]["tokens"]["total"],
                report["task_outcomes"]["accepted_tasks"],
                report["review_outcomes"]["durations_ms"]["total"],
            )

        once = metrics.collect_metrics(
            common, limit=20, stores=[store], config_path=unpriced
        )
        twice = metrics.collect_metrics(
            common, limit=20, stores=[store, store / "v2"], config_path=unpriced
        )
        test.equal(
            "STORE the same task root passed twice is counted once",
            totals(twice),
            totals(once),
        )
        test.true(
            "STORE a duplicate store is reported rather than silently elided",
            twice["selection"]["stores"][2]["duplicate"] is True
            and twice["selection"]["stores"][2]["tasks"] == 0
            and "DUPLICATE (already counted)" in metrics.render_text(twice),
        )
        plain = metrics.collect_metrics(common, limit=20, config_path=unpriced)
        self_referential = metrics.collect_metrics(
            common, limit=20, stores=[common], config_path=unpriced
        )
        test.equal(
            "STORE the default store passed as --store is not counted twice",
            totals(self_referential),
            totals(plain),
        )
        test.equal(
            "STORE same task id in two stores does not collide",
            [
                (item["task_id"], item["store"])
                for item in metrics.collect_metrics(
                    common, limit=2, stores=[store], config_path=unpriced
                )["tasks"]["records"]
            ],
            [
                ("task-q", str((store / "v2" / "tasks").resolve())),
                ("task-q", str((common / "agent-harness" / "v2" / "tasks").resolve())),
            ],
        )


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
    outcome_cases(test)
    if test.failures:
        for failure in test.failures:
            print(f"metrics-selftest: FAIL: {failure}")
        return 1
    print(f"metrics-selftest: PASS ({test.assertions} assertions)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
