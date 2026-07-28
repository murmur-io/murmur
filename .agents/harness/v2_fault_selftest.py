#!/usr/bin/env python3
"""Deterministic fault injection for Harness v2's immutable evidence boundary.

The suite uses real temporary Git repositories, but no network or external
model.  It intentionally exercises private trust-kernel seams because these
are crash/ABA/tamper regressions, not product behavior tests.
"""

from __future__ import annotations

import copy
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
from typing import Any, Callable, Dict, Mapping, Optional, Sequence

import cli as harness_cli
import task_runner as legacy
import verifier


ROOT = Path(__file__).resolve().parents[2]
A_BYTES = b"state-A\n\x00\xffexact bytes\n"
B_BYTES = b"state-B\n\x00\x7fdifferent bytes\n"
UNTRACKED_BYTES = bytes(range(256)) + b"\x00\xffuntracked\n"


class Tests:
    def __init__(self) -> None:
        self.count = 0
        self.failures: list[str] = []

    def equal(self, label: str, actual: Any, expected: Any) -> None:
        self.count += 1
        if actual == expected:
            print(f"  [PASS] {label}")
            return
        self.failures.append(
            f"{label}: expected {expected!r}, found {actual!r}"
        )
        print(f"  [FAIL] {label}: {actual!r}")

    def true(self, label: str, value: Any) -> None:
        self.equal(label, bool(value), True)

    def raises(
        self,
        label: str,
        invoke: Callable[[], Any],
        contains: str,
    ) -> None:
        self.count += 1
        try:
            invoke()
        except Exception as exc:  # noqa: BLE001 - intentional tamper injection
            if contains in str(exc):
                print(f"  [PASS] {label}")
                return
            self.failures.append(
                f"{label}: error did not contain {contains!r}: {exc}"
            )
            print(f"  [FAIL] {label}: {exc}")
            return
        self.failures.append(f"{label}: expected an exception")
        print(f"  [FAIL] {label}: no exception")


def _git(repo: Path, *args: str) -> str:
    completed = subprocess.run(
        ["git", *args],
        cwd=str(repo),
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            f"git {' '.join(args)} failed: {completed.stderr.strip()}"
        )
    return completed.stdout.strip()


def _fixture(root: Path) -> Dict[str, Any]:
    primary = root / "primary"
    primary.mkdir()
    _git(primary, "init", "-q", "-b", "murmur")
    _git(primary, "config", "user.name", "QueaT")
    _git(primary, "config", "user.email", "kgm004a@gmail.com")
    (primary / "owned.bin").write_bytes(b"base\n")
    _git(primary, "add", "owned.bin")
    _git(primary, "commit", "-q", "-m", "base")
    base = _git(primary, "rev-parse", "HEAD")

    worktree = root / "task" / "meetnotes"
    worktree.parent.mkdir()
    branch = "agent/v2/fault-selftest"
    _git(
        primary,
        "worktree",
        "add",
        "-q",
        "-b",
        branch,
        str(worktree),
        base,
    )
    (worktree / "owned.bin").write_bytes(A_BYTES)
    (worktree / "untracked.bin").write_bytes(UNTRACKED_BYTES)
    executable = worktree / "exact.sh"
    executable.write_bytes(b"#!/bin/sh\nprintf exact\n")
    executable.chmod(0o755)

    common = Path(
        _git(
            primary,
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        )
    ).resolve()
    task_dir = common / "agent-harness" / "v2" / "tasks" / "fault-selftest"
    attempt_dir = task_dir / "attempts" / ("a" * 64)
    (attempt_dir / "checks").mkdir(parents=True)
    contract: Dict[str, Any] = {
        "schema_version": 2,
        "task_id": "fault-selftest",
        "description": "exercise immutable snapshot fault boundaries",
        "kind": "harness",
        "base_sha": base,
        "contract_sha256": "",
        "repo_realpath": str(primary.resolve()),
        "git_common_dir": str(common),
        "worktree_path": str(worktree.resolve()),
        "branch": branch,
        "owned_paths": ["owned.bin", "untracked.bin", "exact.sh"],
        "claims": [],
        "reviewer": "fake",
        "expected_change": True,
        "degraded_provenance": [],
        "created_at": legacy.utc_now(),
    }
    contract["contract_sha256"] = verifier.document_hash(
        contract, "contract_sha256"
    )
    legacy.atomic_write_json(task_dir / "task.json", contract)
    paths, diff, tree_sha = verifier.snapshot_scoped_diff(
        worktree, contract, task_dir
    )
    plan = {
        "schema_version": 2,
        "task_id": contract["task_id"],
        "contract_sha256": contract["contract_sha256"],
        "base_sha": base,
        "diff_sha256": legacy.sha256_bytes(diff),
        "tree_sha": tree_sha,
        "protocol_sha256": "2" * 64,
        "changed_paths": paths,
        "claims": [],
        "actual_risk_flags": [],
        "checks": [],
        "reviews": [{"kind": "combined", "vendor": "fake"}],
        "server_required": False,
        "created_at": contract["created_at"],
        "plan_sha256": "1" * 64,
    }
    snapshot = harness_cli._ensure_verification_snapshot(
        contract, task_dir, plan, attempt_dir
    )
    return {
        "primary": primary,
        "worktree": worktree,
        "task_dir": task_dir,
        "attempt_dir": attempt_dir,
        "contract": contract,
        "plan": plan,
        "diff": diff,
        "snapshot": snapshot,
    }


def _wait_for(path: Path, process: subprocess.Popen[str]) -> None:
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if path.is_file():
            return
        if process.poll() is not None:
            raise RuntimeError(
                "slow snapshot reader exited before its synchronization point"
            )
        time.sleep(0.01)
    raise RuntimeError("slow snapshot reader did not reach its synchronization point")


def snapshot_aba_and_reconstruction_cases(test: Tests) -> None:
    with tempfile.TemporaryDirectory(prefix="murmur-v2-fault-snapshot-") as raw:
        values = _fixture(Path(raw))
        worktree: Path = values["worktree"]
        snapshot: Path = values["snapshot"]
        task_dir: Path = values["task_dir"]
        attempt_dir: Path = values["attempt_dir"]
        contract: Mapping[str, Any] = values["contract"]
        plan: Mapping[str, Any] = values["plan"]

        ready = Path(raw) / "slow-check.ready"
        release = Path(raw) / "slow-check.release"
        child_code = (
            "import hashlib,json,pathlib,sys,time;"
            "target=pathlib.Path('owned.bin');"
            "first=target.read_bytes();"
            "pathlib.Path(sys.argv[1]).write_text('ready');"
            "release=pathlib.Path(sys.argv[2]);"
            "deadline=time.monotonic()+5;"
            "\nwhile not release.exists() and time.monotonic()<deadline:"
            "\n time.sleep(0.01)"
            "\nsecond=target.read_bytes();"
            "\nprint(json.dumps({'first':hashlib.sha256(first).hexdigest(),"
            "'second':hashlib.sha256(second).hexdigest()}))"
        )
        process = subprocess.Popen(
            [sys.executable, "-c", child_code, str(ready), str(release)],
            cwd=str(snapshot),
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            _wait_for(ready, process)
            (worktree / "owned.bin").write_bytes(B_BYTES)
            _, b_diff, b_tree = verifier.snapshot_scoped_diff(
                worktree, contract, task_dir
            )
            test.true(
                "SNAPSHOT source B really differs while the slow check is running",
                legacy.sha256_bytes(b_diff) != plan["diff_sha256"]
                and b_tree != plan["tree_sha"],
            )
            test.equal(
                "SNAPSHOT slow check still reads immutable A during source B",
                (snapshot / "owned.bin").read_bytes(),
                A_BYTES,
            )
            (worktree / "owned.bin").write_bytes(A_BYTES)
            _, restored_diff, restored_tree = verifier.snapshot_scoped_diff(
                worktree, contract, task_dir
            )
            test.true(
                "SNAPSHOT source A-B-A returns to the exact planned identity",
                legacy.sha256_bytes(restored_diff) == plan["diff_sha256"]
                and restored_tree == plan["tree_sha"],
            )
            release.write_text("continue", encoding="utf-8")
            stdout, stderr = process.communicate(timeout=5)
        finally:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=2)
        test.equal(
            "SNAPSHOT slow check survives source A-B-A without byte drift",
            json.loads(stdout),
            {
                "first": hashlib.sha256(A_BYTES).hexdigest(),
                "second": hashlib.sha256(A_BYTES).hexdigest(),
            },
        )
        test.equal("SNAPSHOT slow check has no hidden error", stderr, "")
        snapshot_paths, snapshot_diff, snapshot_tree = (
            verifier.snapshot_scoped_diff(snapshot, contract, task_dir)
        )
        test.true(
            "SNAPSHOT evidence identity remains bound to A",
            snapshot_paths == plan["changed_paths"]
            and legacy.sha256_bytes(snapshot_diff) == plan["diff_sha256"]
            and snapshot_tree == plan["tree_sha"],
        )

        original_bytes = {
            relative: (snapshot / relative).read_bytes()
            for relative in ("owned.bin", "untracked.bin", "exact.sh")
        }
        reference = harness_cli._verification_snapshot_ref(
            contract["task_id"], attempt_dir.name
        )
        anchored_commit = _git(
            values["primary"], "rev-parse", "--verify", reference
        )
        harness_cli._discard_claimed_verification_snapshot(
            contract, attempt_dir, snapshot
        )
        test.true(
            "SNAPSHOT cleanup removes only the materialized clone",
            not snapshot.exists(),
        )
        test.equal(
            "SNAPSHOT cleanup preserves the immutable reconstruction anchor",
            _git(values["primary"], "rev-parse", "--verify", reference),
            anchored_commit,
        )
        snapshot.mkdir()
        (snapshot / "partial-after-crash").write_bytes(b"partial")
        rebuilt = harness_cli._ensure_verification_snapshot(
            contract, task_dir, plan, attempt_dir
        )
        test.true(
            "SNAPSHOT resume replaces a partial clone from the durable anchor",
            not (rebuilt / "partial-after-crash").exists(),
        )
        test.equal(
            "SNAPSHOT reconstruction preserves every tracked/untracked byte",
            {
                relative: (rebuilt / relative).read_bytes()
                for relative in ("owned.bin", "untracked.bin", "exact.sh")
            },
            original_bytes,
        )
        test.true(
            "SNAPSHOT reconstruction preserves executable mode",
            bool((rebuilt / "exact.sh").stat().st_mode & 0o111),
        )
        rebuilt_paths, rebuilt_diff, rebuilt_tree = verifier.snapshot_scoped_diff(
            rebuilt, contract, task_dir
        )
        test.true(
            "SNAPSHOT reconstruction preserves exact diff and tree evidence",
            rebuilt_paths == plan["changed_paths"]
            and rebuilt_diff == values["diff"]
            and rebuilt_tree == plan["tree_sha"],
        )


def cached_artifact_tamper_case(test: Tests) -> None:
    with tempfile.TemporaryDirectory(prefix="murmur-v2-fault-cache-") as raw:
        values = _fixture(Path(raw))
        contract: Mapping[str, Any] = values["contract"]
        plan: Mapping[str, Any] = values["plan"]
        task_dir: Path = values["task_dir"]
        attempt_dir: Path = values["attempt_dir"]
        snapshot: Path = values["snapshot"]
        declared = {
            "id": "synthetic-cache",
            "command": "runner-owned synthetic check",
            "timeout_seconds": 5,
        }
        counter = 0
        original_run_check = legacy.run_check

        def synthetic_run_check(
            _worktree: Path,
            task: Path,
            check: Mapping[str, Any],
            phase: str,
            *,
            bound_environment: Optional[Mapping[str, str]] = None,
        ) -> Dict[str, Any]:
            nonlocal counter
            counter += 1
            log_path = task / "logs" / f"fault-cache-{counter}.log"
            stdout_path = log_path.with_suffix(".stdout.log")
            stderr_path = log_path.with_suffix(".stderr.log")
            sandbox_path = task / "runtime" / f"fault-cache-{counter}.sb"
            guardian_path = task / "runtime" / f"fault-cache-{counter}.json"
            for path, content in (
                (log_path, f"run {counter}\n".encode()),
                (stdout_path, b"green\n"),
                (stderr_path, b""),
                (sandbox_path, b"(version 1)\n"),
                (guardian_path, b'{"clean":true}\n'),
            ):
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(content)
            return {
                "id": check["id"],
                "command": check["command"],
                "phase": phase,
                "exit_code": 0,
                "timed_out": False,
                "duration_ms": 1,
                "resource_wait_ms": 0,
                "execution_id": f"synthetic-{counter}",
                "log_path": str(log_path),
                "log_sha256": legacy.sha256_file(log_path),
                "stdout_path": str(stdout_path),
                "stdout_sha256": legacy.sha256_file(stdout_path),
                "stderr_path": str(stderr_path),
                "stderr_sha256": legacy.sha256_file(stderr_path),
                "sandbox_profile_path": str(sandbox_path),
                "sandbox_profile_sha256": legacy.sha256_file(sandbox_path),
                "guardian_path": str(guardian_path),
                "guardian_sha256": legacy.sha256_file(guardian_path),
                "leader_exited_with_live_group": False,
                "passed": True,
                "outcome": "PASS",
            }

        legacy.run_check = synthetic_run_check
        try:
            first, first_did_run = harness_cli._run_or_resume_check(
                contract,
                task_dir,
                plan,
                attempt_dir,
                declared,
                snapshot,
                checkpoint_number=1,
            )
            verifier.validate_check_checkpoint(
                first,
                declared,
                plan,
                task_dir,
            )
            test.true(
                "CACHE pre-change non-npm checkpoint resumes without bound environment",
                "bound_environment" not in first["evidence"],
            )
            first_log = Path(first["evidence"]["log_path"])
            first_log.write_bytes(first_log.read_bytes() + b"tampered\n")
            second, second_did_run = harness_cli._run_or_resume_check(
                contract,
                task_dir,
                plan,
                attempt_dir,
                declared,
                snapshot,
                checkpoint_number=2,
            )
        finally:
            legacy.run_check = original_run_check
        test.true(
            "CACHE initial exact-diff checkpoint executes once",
            first_did_run and counter == 2,
        )
        test.true(
            "CACHE corrupt artifact is rejected and rerun, never reused",
            second_did_run
            and second["evidence"]["passed"]
            and second["evidence"]["log_path"] != str(first_log),
        )
        test.equal(
            "CACHE rerun publishes evidence for the second execution",
            Path(second["evidence"]["log_path"]).read_bytes(),
            b"run 2\n",
        )


def prompt_policy_tamper_cases(test: Tests) -> None:
    with tempfile.TemporaryDirectory(prefix="murmur-v2-fault-prompt-") as raw:
        values = _fixture(Path(raw))
        contract: Mapping[str, Any] = values["contract"]
        plan: Mapping[str, Any] = values["plan"]
        snapshot: Path = values["snapshot"]
        task_dir: Path = values["task_dir"]
        run_dir = values["attempt_dir"] / "review-runs" / "combined"
        review = {"kind": "combined", "vendor": "fake"}
        record = verifier.invoke_readonly_review(
            contract=contract,
            plan=plan,
            worktree=snapshot,
            task_dir=task_dir,
            attempt_dir=run_dir,
            diff=values["diff"],
            checks=[],
            review=review,
            probe_evidence_sha256=verifier.probe_evidence_hash([]),
            allow_test_adapter=True,
            sleep=lambda _delay: None,
        )
        prompt = verifier.combined_review_prompt(
            contract,
            plan,
            values["diff"],
            [],
            "combined",
            snapshot,
            task_dir,
        )
        expected_prompt_sha = legacy.sha256_bytes(prompt.encode("utf-8"))
        verifier.validate_review_checkpoint(
            record,
            review,
            plan,
            task_dir,
            expected_prompt_sha256=expected_prompt_sha,
            allow_test_adapter=True,
        )
        test.true(
            "PROMPT untouched runner/model artifacts validate",
            record["prompt_sha256"] == expected_prompt_sha,
        )

        relabelled = copy.deepcopy(record)
        relabelled["kind"] = "lock-security"
        test.raises(
            "PROMPT review kind cannot be relabelled across planned roles",
            lambda: verifier.validate_review_checkpoint(
                relabelled,
                {"kind": "lock-security", "vendor": "fake"},
                plan,
                task_dir,
                expected_prompt_sha256=expected_prompt_sha,
                allow_test_adapter=True,
            ),
            "label does not match review kind",
        )

        forged = copy.deepcopy(record)
        forged["prompt_sha256"] = "f" * 64
        test.raises(
            "PROMPT substituted checkpoint hash is rejected",
            lambda: verifier.validate_review_checkpoint(
                forged,
                review,
                plan,
                task_dir,
                expected_prompt_sha256=expected_prompt_sha,
                allow_test_adapter=True,
            ),
            "prompt hash changed",
        )

        policy = legacy.read_prompt("combined-reviewer")
        drifted_prompt = verifier.combined_review_prompt(
            contract,
            plan,
            values["diff"],
            [],
            "combined",
            snapshot,
            task_dir,
            policy_text=policy + "\nFAULT POLICY DRIFT\n",
        )
        drifted_prompt_sha = legacy.sha256_bytes(
            drifted_prompt.encode("utf-8")
        )
        test.true(
            "POLICY drift produces a distinct review input binding",
            drifted_prompt_sha != expected_prompt_sha,
        )
        test.raises(
            "POLICY drift rejects the cached review rather than laundering it",
            lambda: verifier.validate_review_checkpoint(
                record,
                review,
                plan,
                task_dir,
                expected_prompt_sha256=drifted_prompt_sha,
                allow_test_adapter=True,
            ),
            "prompt hash changed",
        )


def review_stream_binding_cases(test: Tests) -> None:
    with tempfile.TemporaryDirectory(prefix="murmur-v2-fault-stream-") as raw:
        task_dir = Path(raw)
        stream = task_dir / "stdout.log"
        payload = b"HEAD:" + (b"x" * 5_000) + b":TAIL"
        stream.write_bytes(payload)
        digest = legacy.sha256_file(stream)
        summary = verifier._bounded_stream_summary(  # noqa: SLF001
            task_dir,
            str(stream),
            digest,
            "fault stream",
            128,
        )
        test.true(
            "PROMPT stream excerpt is bounded with deterministic head and tail",
            summary["truncated"]
            and summary["bytes"] == len(payload)
            and summary["excerpt"].startswith("HEAD:")
            and summary["excerpt"].endswith(":TAIL")
            and "evidence bytes omitted" in summary["excerpt"],
        )
        test.raises(
            "PROMPT stream hash drift fails before review dispatch",
            lambda: verifier._bounded_stream_summary(  # noqa: SLF001
                task_dir,
                str(stream),
                "f" * 64,
                "fault stream",
                128,
            ),
            "hash changed",
        )


def protocol_manifest_cases(test: Tests) -> None:
    with tempfile.TemporaryDirectory(prefix="murmur-v2-fault-protocol-") as raw:
        fixture = Path(raw) / "fixture"
        manifest = verifier.protocol_relative_paths(ROOT)
        for relative in manifest:
            source = ROOT / relative
            target = fixture / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
        baseline = verifier.protocol_bundle(fixture)["protocol_sha256"]

        expected_python = {
            path.relative_to(ROOT).as_posix()
            for path in (ROOT / ".agents" / "harness").glob("*.py")
            if path.is_file() and not path.is_symlink()
        }
        test.true(
            "PROTOCOL manifest automatically binds every harness Python module",
            expected_python.issubset(set(manifest))
            and ".agents/harness/v2_fault_selftest.py" in manifest,
        )
        checked_inputs = {
            "AGENTS.md",
            "CLAUDE.md",
            ".claude/settings.json",
            ".codex/config.toml",
            ".codex/hooks.json",
            ".codex/rules/agentic-workflow.md",
            ".claude/rules/agentic-workflow.md",
        }
        test.true(
            "PROTOCOL manifest binds checked vendor settings and instructions",
            checked_inputs.issubset(set(manifest)),
        )

        representatives = (
            (".agents/harness/verifier.py", "runner code"),
            (
                ".agents/harness/prompts/combined-reviewer.md",
                "review policy",
            ),
            (".claude/settings.json", "Claude sandbox settings"),
            (".codex/config.toml", "Codex configuration"),
            (".codex/rules/agentic-workflow.md", "workflow instruction"),
            ("AGENTS.md", "root project instruction"),
        )
        for relative, label in representatives:
            path = fixture / relative
            original = path.read_bytes()
            path.write_bytes(original + b"\n# fault drift\n")
            drifted = verifier.protocol_bundle(fixture)["protocol_sha256"]
            test.true(
                f"PROTOCOL {label} drift changes the protocol hash",
                drifted != baseline,
            )
            path.write_bytes(original)
            test.equal(
                f"PROTOCOL restoring {label} restores the protocol hash",
                verifier.protocol_bundle(fixture)["protocol_sha256"],
                baseline,
            )

        future_module = fixture / ".agents" / "harness" / "future_module.py"
        future_module.write_text("VALUE = 'new executable module'\n", encoding="utf-8")
        test.true(
            "PROTOCOL an unlisted future Python module cannot escape the hash",
            verifier.protocol_bundle(fixture)["protocol_sha256"] != baseline
            and ".agents/harness/future_module.py"
            in verifier.protocol_relative_paths(fixture),
        )
        future_module.unlink()
        future_module.symlink_to(fixture / ".agents" / "harness" / "verifier.py")
        test.raises(
            "PROTOCOL a symlinked future Python module fails closed",
            lambda: verifier.protocol_relative_paths(fixture),
            "Python module is missing or unsafe",
        )


def main() -> int:
    test = Tests()
    snapshot_aba_and_reconstruction_cases(test)
    cached_artifact_tamper_case(test)
    prompt_policy_tamper_cases(test)
    review_stream_binding_cases(test)
    protocol_manifest_cases(test)
    if test.failures:
        print("v2 fault selftest: FAIL")
        for failure in test.failures:
            print(f"  - {failure}")
        return 1
    print(f"v2 fault selftest: PASS ({test.count} assertions)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
