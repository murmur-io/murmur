#!/usr/bin/env python3
"""Exclusive, task-owned Tauri boot smoke for the development harness.

The generic path proves that Angular and the directly supervised Rust app boot without
reusing or killing foreign listeners. The exact reminders Harness task additionally
proves its lock/egress/restart claims with a runner-authenticated, content-free receipt.
"""

from __future__ import annotations

import argparse
import base64
import ctypes
import ctypes.util
import hashlib
import json
import os
import secrets
import select
import shutil
import signal
import socket
import sqlite3
import stat
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
import urllib.request
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, BinaryIO, Iterable


DEV_DEK = "0123456789abcdef" * 4
ENHANCED_RUNTIME_TASK_ID = "reminders-smart-inbox-v2-merge-20260731"
RUNTIME_CHALLENGE_FILE = "reminder-runtime-challenge.json"
RESTART_INTENT_FILE = "reminder-runtime-restart-intent.json"
RESTART_GENERATION_FILE = "reminder-runtime-restart-generation.json"
RUNTIME_WITNESS_FILE = "reminder-runtime-witness.json"
RUNNER_PASS_FILE = "runner-pass.json"
MAX_RECEIPT_BYTES = 64 * 1024
NETWORK_PROFILE = b"""(version 1)
(allow default)
(deny network-outbound (require-not (remote ip "localhost:*")))
"""
EXPECTED_RUNTIME_CHECKS: dict[str, Any] = {
    "actualTauriIpc": True,
    "sqlcipherIntegrity": True,
    "plainSqliteRejected": True,
    "lockUnlockRelock": True,
    "inFlightLocalInference": True,
    "maskedReads": True,
    "restartPurge": True,
    "noEgress": True,
    "providerCases": 4,
    "localGenerateCount": 5,
    "stubCase": True,
}


class RuntimeProofError(RuntimeError):
    """A proof assertion failed; cleanup still has to run before emitting FAIL."""


def utc_now() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")


def valid_lowercase_hex(value: Any, length: int) -> bool:
    return (
        isinstance(value, str)
        and len(value) == length
        and all(character in "0123456789abcdef" for character in value)
    )


def repo_root() -> Path:
    value = subprocess.check_output(
        ["git", "rev-parse", "--show-toplevel"], text=True, stderr=subprocess.DEVNULL
    ).strip()
    return Path(value).resolve()


def common_dir(root: Path) -> Path:
    value = subprocess.check_output(
        ["git", "rev-parse", "--path-format=absolute", "--git-common-dir"],
        cwd=str(root),
        text=True,
    ).strip()
    path = Path(value)
    if not path.is_absolute():
        path = root / path
    return path.resolve()


def git_head(root: Path) -> str:
    value = subprocess.check_output(
        ["git", "rev-parse", "HEAD"], cwd=str(root), text=True
    ).strip()
    if not valid_lowercase_hex(value, 40):
        raise RuntimeProofError("Git HEAD is not a full lowercase object id")
    return value


def git_branch(root: Path) -> str:
    return subprocess.check_output(
        ["git", "branch", "--show-current"], cwd=str(root), text=True
    ).strip()


def git_index_tree(root: Path) -> str:
    value = subprocess.check_output(
        ["git", "write-tree"], cwd=str(root), text=True
    ).strip()
    if not valid_lowercase_hex(value, 40):
        raise RuntimeProofError("Git index tree is not a full lowercase object id")
    return value


def tracked_worktree_matches_index(root: Path) -> bool:
    result = subprocess.run(
        ["git", "diff", "--quiet", "--no-ext-diff"],
        cwd=str(root),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    if result.returncode not in (0, 1):
        raise RuntimeProofError("Git could not compare the runtime worktree with its index")
    return result.returncode == 0


def runtime_dir(root: Path) -> Path:
    """Keep harness-owned boot artifacts inside the check's writable runtime."""

    harness_runtime = os.environ.get("MURMUR_HARNESS_RUNTIME_DIR")
    if harness_runtime:
        path = Path(harness_runtime)
        if not path.is_absolute():
            raise ValueError("MURMUR_HARNESS_RUNTIME_DIR must be absolute")
        return path.resolve()
    return common_dir(root) / "agent-harness" / "runtime"


def path_is_within(path: Path, parent: Path) -> bool:
    try:
        path.relative_to(parent)
        return True
    except ValueError:
        return False


def listener_pids(port: int) -> set[int]:
    result = subprocess.run(
        ["lsof", "-nP", "-a", "-t", f"-iTCP:{port}", "-sTCP:LISTEN"],
        text=True,
        capture_output=True,
        timeout=3,
        check=False,
    )
    if result.returncode not in (0, 1):
        raise RuntimeError(f"lsof could not inspect listener ownership on port {port}")
    pids: set[int] = set()
    for value in result.stdout.splitlines():
        value = value.strip()
        if value:
            try:
                pids.add(int(value))
            except ValueError as exc:
                raise RuntimeError(f"lsof returned an invalid PID for port {port}") from exc
    return pids


def listener(port: int) -> str | None:
    pids = listener_pids(port)
    if not pids:
        return None
    return f"listener PID(s) {','.join(str(pid) for pid in sorted(pids))} on 127.0.0.1:{port}"


def angular_ready() -> bool:
    for url in ("http://127.0.0.1:1420", "http://localhost:1420"):
        try:
            with urllib.request.urlopen(url, timeout=1) as response:
                if 200 <= response.status < 500:
                    return True
        except Exception:
            continue
    return False


def runtime_mode(root: Path) -> str:
    candidates = [
        root / "src-tauri" / "target" / "debug" / "Murmur",
        root / "target" / "debug" / "Murmur",
    ]
    target_dir = os.environ.get("CARGO_TARGET_DIR")
    if target_dir and path_is_within(Path(__file__).resolve(), root.resolve()):
        target_path = Path(target_dir)
        if not target_path.is_absolute():
            target_path = root / target_path
        candidates.insert(0, target_path.resolve() / "debug" / "Murmur")
    return "warm" if any(path.is_file() for path in candidates) else "cold"


def cargo_target_dir(root: Path, env: dict[str, str]) -> Path:
    raw = env.get("CARGO_TARGET_DIR")
    target = Path(raw) if raw else root / "target"
    if not target.is_absolute():
        target = root / target
    return target.resolve()


def fingerprint_file(
    path: Path,
    *,
    label: str,
    require_single_link: bool = True,
) -> dict[str, Any]:
    """Hash one exact, non-symlink inode and reject replacement during the read."""

    metadata = path.lstat()
    if (
        not stat.S_ISREG(metadata.st_mode)
        or (require_single_link and metadata.st_nlink != 1)
    ):
        raise RuntimeProofError(f"{label} is not a safe regular file")
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        if (
            not stat.S_ISREG(opened.st_mode)
            or opened.st_dev != metadata.st_dev
            or opened.st_ino != metadata.st_ino
            or opened.st_size != metadata.st_size
            or (require_single_link and opened.st_nlink != 1)
        ):
            raise RuntimeProofError(f"{label} changed while being opened")
        digest = hashlib.sha256()
        while True:
            chunk = os.read(descriptor, 1024 * 1024)
            if not chunk:
                break
            digest.update(chunk)
        finished = os.fstat(descriptor)
        if (
            finished.st_dev != opened.st_dev
            or finished.st_ino != opened.st_ino
            or finished.st_size != opened.st_size
            or finished.st_mtime_ns != opened.st_mtime_ns
        ):
            raise RuntimeProofError(f"{label} changed while being hashed")
    finally:
        os.close(descriptor)
    return {
        "path": str(path.resolve()),
        "device": opened.st_dev,
        "inode": opened.st_ino,
        "size": opened.st_size,
        "sha256": digest.hexdigest(),
    }


def same_fingerprint(actual: dict[str, Any], expected: dict[str, Any]) -> bool:
    return all(
        actual.get(key) == expected.get(key)
        for key in ("path", "device", "inode", "size", "sha256")
    )


def write_bytes_create_only(path: Path, encoded: bytes, mode: int = 0o600) -> None:
    temp = path.parent / f".{path.name}.{os.getpid()}.{secrets.token_hex(8)}.tmp"
    flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(temp, flags, mode)
    try:
        offset = 0
        while offset < len(encoded):
            written = os.write(descriptor, encoded[offset:])
            if written <= 0:
                raise OSError("short create-only write")
            offset += written
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    try:
        os.link(temp, path, follow_symlinks=False)
    finally:
        temp.unlink(missing_ok=True)
    directory_fd = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(directory_fd)
    finally:
        os.close(directory_fd)


def write_json_create_only(path: Path, value: dict[str, Any]) -> None:
    encoded = json.dumps(value, sort_keys=True, separators=(",", ":")).encode("utf-8")
    if len(encoded) > MAX_RECEIPT_BYTES:
        raise ValueError("runtime receipt exceeds its size bound")
    write_bytes_create_only(path, encoded)


def copy_file_create_only(source: Path, destination: Path) -> dict[str, Any]:
    source_before = fingerprint_file(source, label="built app binary")
    source_fd = os.open(source, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    destination_fd = os.open(
        destination,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0),
        0o500,
    )
    try:
        opened = os.fstat(source_fd)
        if (
            opened.st_dev != source_before["device"]
            or opened.st_ino != source_before["inode"]
            or opened.st_size != source_before["size"]
        ):
            raise RuntimeProofError("built app binary changed before staging began")
        while True:
            chunk = os.read(source_fd, 1024 * 1024)
            if not chunk:
                break
            offset = 0
            while offset < len(chunk):
                written = os.write(destination_fd, chunk[offset:])
                if written <= 0:
                    raise OSError("short staged binary write")
                offset += written
        os.fsync(destination_fd)
    finally:
        os.close(destination_fd)
        os.close(source_fd)
    source_after = fingerprint_file(source, label="built app binary")
    if not same_fingerprint(source_after, source_before):
        raise RuntimeProofError("built app binary changed while it was staged")
    directory_fd = os.open(destination.parent, os.O_RDONLY)
    try:
        os.fsync(directory_fd)
    finally:
        os.close(directory_fd)
    staged = fingerprint_file(destination, label="staged app binary")
    if (
        staged["size"] != source_before["size"]
        or staged["sha256"] != source_before["sha256"]
    ):
        raise RuntimeProofError("staged app binary is not byte-identical to Cargo output")
    return staged


def read_json_object(path: Path, label: str) -> dict[str, Any]:
    try:
        metadata = path.lstat()
        if (
            not stat.S_ISREG(metadata.st_mode)
            or metadata.st_nlink != 1
            or metadata.st_size > MAX_RECEIPT_BYTES
        ):
            raise ValueError(f"{label} is not a bounded regular file")
        descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
        try:
            opened = os.fstat(descriptor)
            if (
                not stat.S_ISREG(opened.st_mode)
                or opened.st_ino != metadata.st_ino
                or opened.st_dev != metadata.st_dev
                or opened.st_size > MAX_RECEIPT_BYTES
            ):
                raise ValueError(f"{label} changed while being opened")
            encoded = os.read(descriptor, MAX_RECEIPT_BYTES + 1)
        finally:
            os.close(descriptor)
        if len(encoded) > MAX_RECEIPT_BYTES:
            raise ValueError(f"{label} exceeds its size bound")
        value = json.loads(encoded.decode("utf-8"))
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise ValueError(f"{label} is unreadable or invalid") from exc
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be a JSON object")
    return value


def enhanced_task_dir(root: Path) -> Path:
    """Resolve the exact task evidence directory in canonical and verify snapshots."""

    harness_runtime = os.environ.get("MURMUR_HARNESS_RUNTIME_DIR")
    if harness_runtime:
        runtime_root = Path(harness_runtime)
        if not runtime_root.is_absolute():
            raise RuntimeProofError("Harness runtime evidence directory must be absolute")
        runtime_root = runtime_root.resolve()
        task_dir = runtime_root.parent.parent.parent
        expected_runtime = task_dir / "runtime" / "checks" / "runtime-smoke"
        if (
            runtime_root != expected_runtime
            or task_dir.name != ENHANCED_RUNTIME_TASK_ID
        ):
            raise RuntimeProofError(
                "Harness runtime evidence directory is outside the exact task"
            )
        return task_dir
    return (
        common_dir(root)
        / "agent-harness"
        / "v2"
        / "tasks"
        / ENHANCED_RUNTIME_TASK_ID
    )


def validate_task_provenance(root: Path) -> dict[str, Any]:
    task_dir = enhanced_task_dir(root)
    task_path = task_dir / "task.json"
    task_fingerprint = fingerprint_file(task_path, label="Harness task contract")
    task = read_json_object(task_path, "Harness task contract")
    head = git_head(root)
    branch = task.get("branch")
    canonical_worktree = Path(str(task.get("worktree_path", ""))).resolve()
    git_common = Path(str(task.get("git_common_dir", ""))).resolve()
    driver = Path(str(task.get("repo_realpath", ""))).resolve()
    if (
        task.get("schema_version") != 2
        or task.get("task_id") != ENHANCED_RUNTIME_TASK_ID
        or task_dir
        != git_common
        / "agent-harness"
        / "v2"
        / "tasks"
        / ENHANCED_RUNTIME_TASK_ID
        or not driver.is_dir()
        or (driver / ".git").resolve() != git_common
        or not isinstance(branch, str)
        or not branch.startswith("agent/v2/")
        or not valid_lowercase_hex(task.get("base_sha"), 40)
        or not valid_lowercase_hex(task.get("contract_sha256"), 64)
        or task.get("base_sha") != head
        or "runtime" not in task.get("claims", [])
    ):
        raise RuntimeProofError("Harness task contract is not bound to this exact worktree")

    proof_mode = "canonical"
    if root == canonical_worktree:
        if common_dir(root) != git_common or git_branch(root) != branch:
            raise RuntimeProofError(
                "Harness task contract is not bound to this exact worktree"
            )
    else:
        state_path = task_dir / "state.json"
        state = read_json_object(state_path, "Harness task state")
        attempt_id = state.get("attempt_id")
        if not valid_lowercase_hex(attempt_id, 64):
            raise RuntimeProofError("Harness verify snapshot has no valid attempt id")
        attempt_dir = task_dir / "attempts" / attempt_id
        plan_path = attempt_dir / "plan.json"
        plan = read_json_object(plan_path, "Harness attempt plan")
        expected_snapshot = canonical_worktree.parent / f"verify-{attempt_id}"
        if (
            root != expected_snapshot
            or root.parent != canonical_worktree.parent
            or not (root / ".git").is_dir()
            or common_dir(root) != (root / ".git").resolve()
            or git_branch(root)
            or state.get("schema_version") != 2
            or state.get("status") != "VERIFYING"
            or state.get("phase") != "checks"
            or state.get("task_id") != ENHANCED_RUNTIME_TASK_ID
            or Path(str(state.get("plan_path", ""))).resolve() != plan_path
            or state.get("diff_sha256") != plan.get("diff_sha256")
            or state.get("plan_sha256") != plan.get("plan_sha256")
            or state.get("protocol_sha256") != plan.get("protocol_sha256")
            or plan.get("schema_version") != 2
            or plan.get("task_id") != ENHANCED_RUNTIME_TASK_ID
            or plan.get("base_sha") != task.get("base_sha")
            or plan.get("contract_sha256") != task.get("contract_sha256")
            or not valid_lowercase_hex(plan.get("tree_sha"), 40)
            or git_index_tree(root) != plan.get("tree_sha")
            or not tracked_worktree_matches_index(root)
        ):
            raise RuntimeProofError(
                "Harness verify snapshot is not bound to its exact attempt plan"
            )
        proof_mode = "verified-snapshot"
    return {
        "path": str(task_path),
        "sha256": task_fingerprint["sha256"],
        "gitHead": head,
        "baseSha": task["base_sha"],
        "contractSha256": task["contract_sha256"],
        "mode": proof_mode,
    }


def validate_restart_intent(
    path: Path, first_pid: int, challenge: dict[str, Any]
) -> str:
    intent = read_json_object(path, "restart intent")
    run_id = intent.get("runId")
    if (
        set(intent)
        != {
            "schemaVersion",
            "runId",
            "firstPid",
            "phaseOnePassed",
            "runnerNonce",
            "binarySha256",
            "selectorSuccesses",
            "lockedAuditOutcomes",
            "restartCanaryRows",
        }
        or intent.get("schemaVersion") != 1
        or intent.get("firstPid") != first_pid
        or intent.get("phaseOnePassed") is not True
        or run_id != challenge["runId"]
        or intent.get("runnerNonce") != challenge["runnerNonce"]
        or intent.get("binarySha256") != challenge["binarySha256"]
        or intent.get("selectorSuccesses") != 5
        or intent.get("lockedAuditOutcomes") != 2
        or intent.get("restartCanaryRows") != 2
    ):
        raise ValueError("restart intent failed its runner binding")
    if not isinstance(run_id, str):
        raise ValueError("restart intent run id is invalid")
    return run_id


def validate_runtime_witness(
    witness: dict[str, Any],
    *,
    challenge: dict[str, Any],
    phase_two_nonce: str,
    first_pid: int,
    second_pid: int,
    proxy_connections: int,
) -> dict[str, Any]:
    if (
        set(witness)
        != {
            "schemaVersion",
            "verdict",
            "runId",
            "runnerNonce",
            "phaseTwoNonce",
            "binarySha256",
            "networkProfileSha256",
            "runnerSourceSha256",
            "taskContractSha256",
            "gitHead",
            "firstPid",
            "secondPid",
            "checks",
        }
        or witness.get("schemaVersion") != 1
        or witness.get("verdict") != "PASS"
        or witness.get("runId") != challenge["runId"]
        or witness.get("runnerNonce") != challenge["runnerNonce"]
        or witness.get("phaseTwoNonce") != phase_two_nonce
        or witness.get("binarySha256") != challenge["binarySha256"]
        or witness.get("networkProfileSha256") != challenge["networkProfileSha256"]
        or witness.get("runnerSourceSha256") != challenge["runnerSourceSha256"]
        or witness.get("taskContractSha256") != challenge["taskContractSha256"]
        or witness.get("gitHead") != challenge["gitHead"]
        or witness.get("firstPid") != first_pid
        or witness.get("secondPid") != second_pid
        or first_pid == second_pid
        or witness.get("checks") != EXPECTED_RUNTIME_CHECKS
        or proxy_connections != 0
    ):
        raise ValueError("runtime witness failed its runner binding")
    return {
        "schema_version": 1,
        "first_pid": first_pid,
        "second_pid": second_pid,
        "pid_changed": True,
        "binary_sha256": challenge["binarySha256"],
        "external_provider_egress_connections": proxy_connections,
        "checks": EXPECTED_RUNTIME_CHECKS,
    }


def poll_json_pipe(
    descriptor: int,
    buffer: bytearray,
    parsed: dict[str, Any] | None,
) -> tuple[dict[str, Any] | None, bool]:
    eof = False
    while True:
        readable, _, _ = select.select([descriptor], [], [], 0)
        if not readable:
            break
        try:
            chunk = os.read(descriptor, 4096)
        except BlockingIOError:
            break
        if not chunk:
            eof = True
            break
        buffer.extend(chunk)
        if len(buffer) > MAX_RECEIPT_BYTES:
            raise ValueError("runtime witness pipe exceeds its size bound")
    if parsed is None and b"\n" in buffer:
        line, remainder = bytes(buffer).split(b"\n", 1)
        if remainder:
            raise ValueError("runtime witness pipe contains multiple messages")
        try:
            value = json.loads(line.decode("utf-8"))
        except (UnicodeError, json.JSONDecodeError) as exc:
            raise ValueError("runtime witness pipe is invalid") from exc
        if not isinstance(value, dict):
            raise ValueError("runtime witness pipe must contain one JSON object")
        parsed = value
        buffer.clear()
        buffer.extend(remainder)
    elif parsed is not None and buffer:
        raise ValueError("runtime witness pipe contains trailing data")
    return parsed, eof


def drain_json_pipe_to_eof(
    descriptor: int,
    buffer: bytearray,
    parsed: dict[str, Any] | None,
    deadline: float,
) -> dict[str, Any]:
    eof = False
    while time.monotonic() < deadline:
        parsed, eof = poll_json_pipe(descriptor, buffer, parsed)
        if eof:
            break
        time.sleep(0.02)
    if not eof or parsed is None or buffer:
        raise ValueError("runtime witness pipe did not close after one bounded message")
    return parsed


def process_group_exists(pgid: int) -> bool:
    try:
        os.killpg(pgid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


def direct_child_pids(parent_pid: int) -> list[int]:
    result = subprocess.run(
        ["pgrep", "-P", str(parent_pid)],
        text=True,
        capture_output=True,
        timeout=3,
        check=False,
    )
    if result.returncode == 1:
        return []
    if result.returncode != 0:
        raise RuntimeError("pgrep could not audit the guardian's direct children")
    return sorted(
        {
            int(field)
            for field in result.stdout.split()
            if field.isascii() and field.isdigit()
        }
    )


def inherited_network_sandbox_is_active() -> bool:
    """Ask the macOS kernel whether outbound networking is restricted."""

    if sys.platform != "darwin":
        return False
    library_path = ctypes.util.find_library("sandbox")
    if not library_path:
        raise RuntimeProofError("cannot resolve the macOS sandbox library")
    try:
        library = ctypes.CDLL(library_path)
        sandbox_check = library.sandbox_check
        sandbox_check.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
        sandbox_check.restype = ctypes.c_int
        denied = sandbox_check(os.getpid(), b"network-outbound", 0)
    except (AttributeError, OSError) as exc:
        raise RuntimeProofError("cannot inspect the inherited macOS sandbox") from exc
    return denied == 1


class GuardedProcess:
    def __init__(
        self,
        *,
        label: str,
        guardian: subprocess.Popen[bytes],
        child_pid: int,
        liveness_fd: int,
        result_path: Path,
    ) -> None:
        self.label = label
        self.guardian = guardian
        self.child_pid = child_pid
        self.liveness_fd = liveness_fd
        self.result_path = result_path

    def result_if_exited(self) -> dict[str, Any] | None:
        if self.guardian.poll() is None:
            return None
        if self.guardian.returncode != 0:
            raise RuntimeError(f"{self.label} guardian exited with {self.guardian.returncode}")
        result = read_json_object(self.result_path, f"{self.label} guardian result")
        if result.get("schema_version") != 1 or result.get("child_pid") != self.child_pid:
            raise RuntimeError(f"{self.label} guardian result is not bound to its child")
        return result


def launch_guarded(
    *,
    label: str,
    command: list[str],
    root: Path,
    env: dict[str, str],
    log: BinaryIO,
    result_path: Path,
    timeout_seconds: float,
    child_start_deadline: float,
    inherited_fds: Iterable[int] = (),
) -> GuardedProcess:
    guardian_path = root / ".agents" / "harness" / "process_guardian.py"
    if not guardian_path.is_file():
        raise RuntimeError("the pinned process guardian is missing")
    inherited = tuple(inherited_fds)
    pass_arguments: list[str] = []
    for descriptor in inherited:
        pass_arguments.extend(["--pass-fd", str(descriptor)])
    parent_fd, guardian_fd = os.pipe()
    guardian = subprocess.Popen(
        [
            sys.executable,
            str(guardian_path),
            "--parent-fd",
            str(parent_fd),
            "--result",
            str(result_path),
            "--cwd",
            str(root),
            "--timeout-seconds",
            str(max(1.0, timeout_seconds)),
            "--term-grace-seconds",
            "5",
            *pass_arguments,
            "--",
            *command,
        ],
        cwd=str(root),
        env=env,
        stdin=subprocess.DEVNULL,
        stdout=log,
        stderr=subprocess.STDOUT,
        pass_fds=(parent_fd, *inherited),
        start_new_session=True,
    )
    os.close(parent_fd)
    try:
        while time.monotonic() < child_start_deadline:
            children = direct_child_pids(guardian.pid)
            if len(children) == 1:
                return GuardedProcess(
                    label=label,
                    guardian=guardian,
                    child_pid=children[0],
                    liveness_fd=guardian_fd,
                    result_path=result_path,
                )
            if len(children) > 1:
                raise RuntimeError(f"{label} guardian created multiple direct children")
            if guardian.poll() is not None:
                raise RuntimeError(f"{label} guardian exited before starting its child")
            time.sleep(0.05)
        raise RuntimeError(f"{label} guardian did not start its child")
    except BaseException:
        os.close(guardian_fd)
        try:
            os.killpg(guardian.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        guardian.wait(timeout=10)
        raise


def close_liveness(process: GuardedProcess) -> None:
    if process.liveness_fd >= 0:
        os.close(process.liveness_fd)
        process.liveness_fd = -1


def validate_natural_guardian(
    process: GuardedProcess,
    result: dict[str, Any],
    *,
    expected_exit: int,
) -> None:
    close_liveness(process)
    if (
        result.get("exit_code") != expected_exit
        or result.get("timed_out") is not False
        or result.get("parent_lost") is not False
        or result.get("guardian_signal") is not None
        or result.get("leader_exited_with_live_group") is not False
        or result.get("termination_reason") is not None
        or process_group_exists(process.child_pid)
    ):
        raise RuntimeProofError(f"{process.label} did not exit as a quiescent owned process")


def wait_guarded_natural(
    process: GuardedProcess,
    *,
    deadline: float,
    expected_exit: int,
) -> dict[str, Any]:
    while time.monotonic() < deadline:
        result = process.result_if_exited()
        if result is not None:
            validate_natural_guardian(process, result, expected_exit=expected_exit)
            return result
        time.sleep(0.05)
    raise RuntimeProofError(f"{process.label} did not finish before its deadline")


def request_guarded_shutdown(process: GuardedProcess) -> dict[str, Any]:
    if process.guardian.poll() is not None:
        raise RuntimeProofError(f"{process.label} exited before controlled shutdown")
    try:
        os.killpg(process.child_pid, signal.SIGTERM)
    except ProcessLookupError as exc:
        raise RuntimeProofError(f"{process.label} vanished before controlled shutdown") from exc
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        result = process.result_if_exited()
        if result is not None:
            close_liveness(process)
            if (
                result.get("exit_code") not in (0, -signal.SIGTERM, 128 + signal.SIGTERM)
                or result.get("timed_out") is not False
                or result.get("parent_lost") is not False
                or result.get("guardian_signal") is not None
                or result.get("leader_exited_with_live_group") is not False
                or result.get("termination_reason") is not None
                or process_group_exists(process.child_pid)
            ):
                raise RuntimeProofError(
                    f"{process.label} did not honor runner-controlled shutdown cleanly"
                )
            return result
        time.sleep(0.05)
    raise RuntimeProofError(f"{process.label} did not stop under live-parent supervision")


def fallback_terminate_guarded(process: GuardedProcess | None) -> None:
    if process is None:
        return
    close_liveness(process)
    try:
        process.guardian.wait(timeout=20)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.guardian.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            process.guardian.wait(timeout=10)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.guardian.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.guardian.wait(timeout=5)
    deadline = time.monotonic() + 5
    while process_group_exists(process.child_pid) and time.monotonic() < deadline:
        time.sleep(0.05)
    if process_group_exists(process.child_pid):
        raise RuntimeError(f"owned {process.label} process group survived guardian cleanup")


def owned_process_group_survivors(
    processes: Iterable[GuardedProcess | None],
) -> list[int]:
    """Audit the exact PGIDs published by the parent-death guardians."""

    return sorted(
        {
            process.child_pid
            for process in processes
            if process is not None and process_group_exists(process.child_pid)
        }
    )


def lsof_text_records(pid: int) -> list[dict[str, str]]:
    result = subprocess.run(
        ["lsof", "-nP", "-a", "-p", str(pid), "-d", "txt", "-F", "pDint"],
        text=True,
        capture_output=True,
        timeout=3,
        check=False,
    )
    if result.returncode != 0:
        return []
    records: list[dict[str, str]] = []
    current: dict[str, str] | None = None
    for line in result.stdout.splitlines():
        if not line:
            continue
        field, value = line[0], line[1:]
        if field == "f":
            if current is not None:
                records.append(current)
            current = {"f": value}
        elif current is not None and field in {"D", "i", "n", "t"}:
            current[field] = value
    if current is not None:
        records.append(current)
    return records


def prove_mapped_executable(
    pid: int,
    expected: dict[str, Any],
    *,
    deadline: float,
) -> dict[str, Any]:
    while time.monotonic() < deadline:
        for record in lsof_text_records(pid):
            try:
                device = int(record.get("D", ""), 0)
                inode = int(record.get("i", ""), 10)
                mapped_path = str(Path(record.get("n", "")).resolve())
            except (OSError, TypeError, ValueError):
                continue
            if (
                record.get("f") == "txt"
                and record.get("t") == "REG"
                and mapped_path == expected["path"]
                and device == expected["device"]
                and inode == expected["inode"]
            ):
                current = fingerprint_file(Path(expected["path"]), label="staged app binary")
                if not same_fingerprint(current, expected):
                    raise RuntimeProofError("staged app binary changed after process launch")
                return {
                    "pid": pid,
                    "path": mapped_path,
                    "device": device,
                    "inode": inode,
                    "sha256": current["sha256"],
                }
        time.sleep(0.05)
    raise RuntimeProofError("the launched PID never mapped the staged app binary")


class ExternalEgressObserver:
    def __init__(self) -> None:
        self._socket = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self._socket.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self._socket.bind(("127.0.0.1", 0))
        self._socket.listen()
        self._socket.settimeout(0.2)
        self._stop = threading.Event()
        self._lock = threading.Lock()
        self._connections = 0
        self._health_connections = 0
        self._unauthorized_connections = 0
        self._failed = False
        self._health_path = f"/runner-health-{secrets.token_hex(16)}"
        self._authorization_token = secrets.token_hex(32)
        credentials = f"murmur-harness:{self._authorization_token}".encode("ascii")
        self._authorization_value = b"Basic " + base64.b64encode(credentials)
        self._thread = threading.Thread(target=self._run, daemon=True)
        self._thread.start()

    @property
    def endpoint(self) -> str:
        host, port = self._socket.getsockname()
        return f"http://murmur-harness:{self._authorization_token}@{host}:{port}"

    @property
    def address(self) -> tuple[str, int]:
        host, port = self._socket.getsockname()
        return str(host), int(port)

    def _run(self) -> None:
        while not self._stop.is_set():
            try:
                connection, _ = self._socket.accept()
            except socket.timeout:
                continue
            except OSError:
                if not self._stop.is_set():
                    self._failed = True
                return
            with connection:
                try:
                    connection.settimeout(0.5)
                    prefix = bytearray()
                    while len(prefix) < 4096 and not prefix.endswith(b"\r\n\r\n"):
                        chunk = connection.recv(1)
                        if not chunk:
                            break
                        prefix.extend(chunk)
                except OSError:
                    prefix = bytearray()
            header_lines = bytes(prefix).splitlines()
            first_line = header_lines[0] if header_lines else b""
            authorization = next(
                (
                    line.split(b":", 1)[1].strip()
                    for line in header_lines[1:]
                    if line.lower().startswith(b"proxy-authorization:")
                ),
                b"",
            )
            if not secrets.compare_digest(authorization, self._authorization_value):
                with self._lock:
                    self._unauthorized_connections += 1
                continue
            expected_health = (
                f"GET http://murmur-harness.invalid{self._health_path} HTTP/1.1"
            ).encode("ascii")
            is_health = first_line == expected_health
            with self._lock:
                if is_health:
                    self._health_connections += 1
                else:
                    # No hostname is exempt. A GitHub update check is egress just like a
                    # provider connection and must have been disabled by the exact renderer.
                    self._connections += 1

    def connections(self) -> int:
        with self._lock:
            return self._connections

    def unauthorized_connections(self) -> int:
        with self._lock:
            return self._unauthorized_connections

    def assert_healthy(self) -> None:
        if self._failed or not self._thread.is_alive():
            raise RuntimeError("external egress observer failed")

    def prove_liveness(self) -> None:
        self.assert_healthy()
        with socket.create_connection(self.address, timeout=1) as connection:
            request_line = (
                f"GET http://murmur-harness.invalid{self._health_path} HTTP/1.1\r\n"
            ).encode("ascii")
            request = (
                request_line
                + b"Host: murmur-harness.invalid\r\n"
                + b"Proxy-Authorization: "
                + self._authorization_value
                + b"\r\nConnection: close\r\n\r\n"
            )
            connection.sendall(request)
        deadline = time.monotonic() + 1
        while time.monotonic() < deadline:
            with self._lock:
                if self._health_connections == 1:
                    return
            time.sleep(0.01)
        raise RuntimeError("external egress observer liveness was not witnessed")

    def prove_unauthorized_rejected(self) -> None:
        self.assert_healthy()
        with socket.create_connection(self.address, timeout=1) as connection:
            connection.sendall(
                b"GET http://unauthorized.invalid/ HTTP/1.1\r\n"
                b"Host: unauthorized.invalid\r\nConnection: close\r\n\r\n"
            )
        deadline = time.monotonic() + 1
        while time.monotonic() < deadline:
            with self._lock:
                if (
                    self._unauthorized_connections == 1
                    and self._health_connections == 0
                    and self._connections == 0
                ):
                    return
            time.sleep(0.01)
        raise RuntimeError("external egress observer accepted an unauthorized local client")

    def settle_and_close(self) -> int:
        time.sleep(0.5)
        self.assert_healthy()
        self._stop.set()
        self._socket.close()
        self._thread.join(timeout=2)
        if self._thread.is_alive() or self._failed:
            raise RuntimeError("external egress observer did not close cleanly")
        return self.connections()

    def force_close(self) -> None:
        self._stop.set()
        self._socket.close()
        self._thread.join(timeout=2)


def verify_external_encrypted_database(
    isolated_home: Path,
    sqlcipher_binary: str,
) -> dict[str, Any]:
    data_root = isolated_home / "Library" / "Application Support"
    candidates = list(data_root.rglob("meetnotes.sqlite")) if data_root.is_dir() else []
    if len(candidates) != 1:
        raise RuntimeProofError("the isolated runtime did not produce exactly one database")
    database = candidates[0]
    before = fingerprint_file(database, label="isolated SQLCipher database")
    with database.open("rb") as handle:
        encrypted_header = handle.read(16) != b"SQLite format 3\0"
    uri = f"file:{urllib.parse.quote(str(database))}?mode=ro&immutable=1"
    plain_query_rejected = False
    try:
        connection = sqlite3.connect(uri, uri=True)
        try:
            connection.execute("SELECT COUNT(*) FROM sqlite_master").fetchone()
        except sqlite3.DatabaseError:
            plain_query_rejected = True
        finally:
            connection.close()
    except sqlite3.DatabaseError:
        plain_query_rejected = True

    script = f""".bail on
PRAGMA key = "x'{DEV_DEK}'";
PRAGMA query_only = ON;
PRAGMA cipher_integrity_check;
PRAGMA integrity_check;
SELECT CASE WHEN
  (SELECT COUNT(*) FROM sqlite_master
    WHERE type='table' AND name IN (
      'reminders','reminder_sources','reminder_due_occurrences',
      'reminder_audit_cache','reminder_pending_suggestions'
    )) = 5
  AND (SELECT COUNT(*) FROM reminders WHERE title='Runtime reminder probe') = 1
THEN 'schema-ok' ELSE 'schema-fail' END;
"""
    result = subprocess.run(
        [sqlcipher_binary, "-batch", "-noheader", "-readonly", str(database)],
        input=script,
        text=True,
        capture_output=True,
        timeout=15,
        check=False,
    )
    output = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    cipher_ok = (
        result.returncode == 0
        and output.count("ok") >= 2
        and output[-1:] == ["schema-ok"]
        and not result.stderr.strip()
    )
    after = fingerprint_file(database, label="isolated SQLCipher database")
    unchanged = same_fingerprint(after, before)
    if not encrypted_header or not plain_query_rejected or not cipher_ok or not unchanged:
        raise RuntimeProofError("external keyed SQLCipher/plain-SQLite proof failed")
    return {
        "encryptedHeader": True,
        "plainSqliteRejected": True,
        "cipherIntegrityCheck": "ok",
        "sqliteIntegrityCheck": "ok",
        "schemaAndReminder": "schema-ok",
        "databaseSha256": before["sha256"],
        "databaseInodeStable": True,
    }


def selected_timeout(
    root: Path,
    *,
    explicit: int | None,
    warm_timeout: int,
    cold_timeout: int,
) -> tuple[str, int]:
    mode = runtime_mode(root)
    timeout = explicit if explicit is not None else (
        warm_timeout if mode == "warm" else cold_timeout
    )
    if timeout < 1:
        raise ValueError("runtime timeout must be positive")
    return mode, timeout


def emit(
    verdict: str,
    reason: str,
    log_path: Path | None,
    started_at: str,
    runtime_evidence: dict[str, Any] | None = None,
) -> int:
    payload = {
        "schema_version": 1,
        "verdict": verdict,
        "reason": reason,
        "started_at": started_at,
        "finished_at": utc_now(),
        "log_path": str(log_path) if log_path else None,
    }
    if runtime_evidence is not None:
        payload["runtime_evidence"] = runtime_evidence
    print(json.dumps(payload, sort_keys=True))
    if verdict == "PASS":
        return 0
    if verdict == "BLOCKED":
        return 3
    return 1


def main() -> int:
    parser = argparse.ArgumentParser(description="Safely prove that the real Tauri dev app boots")
    parser.add_argument("--timeout", type=int)
    parser.add_argument("--warm-timeout", type=int, default=240)
    parser.add_argument("--cold-timeout", type=int, default=900)
    parser.add_argument(
        "--preflight",
        action="store_true",
        help="check ownership/prerequisites and report the cold/warm budget without launching",
    )
    parser.add_argument("--settle-seconds", type=int, default=10)
    parser.add_argument("--runtime-write-probe", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    started_at = utc_now()

    root = repo_root()
    harness_task = os.environ.get("MURMUR_HARNESS_TASK", "")
    enhanced_required = harness_task == ENHANCED_RUNTIME_TASK_ID
    if enhanced_required and os.environ.get("MURMUR_HARNESS") != "1":
        return emit(
            "FAIL",
            "the enhanced runtime proof requires an exact Harness-owned task environment",
            None,
            started_at,
        )
    try:
        mode, timeout = selected_timeout(
            root,
            explicit=args.timeout,
            warm_timeout=args.warm_timeout,
            cold_timeout=args.cold_timeout,
        )
    except ValueError as exc:
        return emit("FAIL", str(exc), None, started_at)

    runtime_root = runtime_dir(root)
    preflight_task: dict[str, Any] | None = None
    if enhanced_required:
        try:
            preflight_task = validate_task_provenance(root)
            task_directory = Path(preflight_task["path"]).parent.resolve()
            if not path_is_within(runtime_root.resolve(), task_directory):
                raise RuntimeProofError(
                    "enhanced runtime evidence directory is outside its exact Harness task"
                )
        except (OSError, ValueError, RuntimeError) as exc:
            return emit("FAIL", str(exc), None, started_at)
    runtime_root.mkdir(parents=True, exist_ok=True)
    run_id = f"boot-{int(time.time())}-{os.getpid()}-{secrets.token_hex(8)}"
    if args.runtime_write_probe:
        probe_path = runtime_root / f"{run_id}-write-probe.json"
        write_json_create_only(probe_path, {"schemaVersion": 1, "writable": True})
        return emit("PASS", "task-private runtime log is writable", probe_path, started_at)

    required_tools = ("lsof", "pgrep", "npm", "cargo")
    for tool in required_tools:
        if shutil.which(tool) is None:
            return emit("BLOCKED", f"{tool} is unavailable; runtime probe cannot start", None, started_at)
    sqlcipher_binary = shutil.which("sqlcipher")
    sandbox_binary = "/usr/bin/sandbox-exec"
    if enhanced_required:
        try:
            if preflight_task is None:
                raise RuntimeProofError("enhanced task provenance disappeared")
            runtime_metadata = runtime_root.lstat()
            if (
                not stat.S_ISDIR(runtime_metadata.st_mode)
                or runtime_root.is_symlink()
            ):
                raise RuntimeProofError("enhanced runtime evidence directory is unsafe")
        except (OSError, ValueError, RuntimeError) as exc:
            return emit("FAIL", str(exc), None, started_at)
        if sys.platform != "darwin" or not Path(sandbox_binary).is_file():
            return emit(
                "BLOCKED",
                "macOS sandbox-exec is required for the enhanced no-egress proof",
                None,
                started_at,
            )
        if sqlcipher_binary is None:
            return emit(
                "BLOCKED",
                "the SQLCipher CLI is required for external keyed integrity proof",
                None,
                started_at,
            )
    try:
        for port in (1420, 8765):
            owner = listener(port)
            if owner:
                return emit(
                    "BLOCKED",
                    f"exclusive runtime port {port} is already owned; refusing to kill or reuse it: {owner}",
                    None,
                    started_at,
                )
    except (RuntimeError, subprocess.TimeoutExpired) as exc:
        return emit("BLOCKED", str(exc), None, started_at)
    if not (root / "package.json").is_file():
        return emit("FAIL", "package.json is missing from the Git root", None, started_at)
    angular_bin = root / "node_modules" / ".bin" / "ng"
    if not angular_bin.is_file():
        return emit(
            "BLOCKED",
            "the local Angular CLI is unavailable; run npm ci before the runtime probe",
            None,
            started_at,
        )
    if args.preflight:
        return emit(
            "PASS",
            f"runtime preflight clear; {mode} start budget is {timeout}s",
            None,
            started_at,
        )

    run_dir = runtime_root / run_id
    try:
        run_dir.mkdir(mode=0o700, exist_ok=False)
    except OSError:
        return emit("FAIL", "could not create a unique runtime evidence directory", None, started_at)
    log_path = run_dir / "boot.log"
    original_home = Path.home()
    temp_root = Path(tempfile.mkdtemp(prefix=f"murmur-{run_id}-"))
    cargo_proc: GuardedProcess | None = None
    frontend_proc: GuardedProcess | None = None
    first_app_proc: GuardedProcess | None = None
    second_app_proc: GuardedProcess | None = None
    observer: ExternalEgressObserver | None = None
    observer_closed = False
    witness_read_fd = -1
    witness_write_fd = -1
    witness_buffer = bytearray()
    pipe_witness: dict[str, Any] | None = None
    failure_reason: str | None = None
    cleanup_errors: list[str] = []
    pass_reason: str | None = None
    runtime_evidence: dict[str, Any] | None = None
    challenge: dict[str, Any] | None = None
    task_provenance: dict[str, Any] | None = None
    staged_identity: dict[str, Any] | None = None
    app_identities: list[dict[str, Any]] = []
    sqlcipher_evidence: dict[str, Any] | None = None
    provider_connections: int | None = None
    unauthorized_connections_rejected: int | None = None
    guardian_results: dict[str, dict[str, Any]] = {}
    phase_two_nonce: str | None = None
    temp_removed = False
    network_enforcement: str | None = None

    challenge_path = run_dir / RUNTIME_CHALLENGE_FILE
    intent_path = run_dir / RESTART_INTENT_FILE
    generation_path = run_dir / RESTART_GENERATION_FILE
    witness_path = run_dir / RUNTIME_WITNESS_FILE
    runner_pass_path = run_dir / RUNNER_PASS_FILE
    profile_path = run_dir / "app-loopback.sb"
    staged_binary = temp_root / "Murmur-runtime-bin"
    runner_source = Path(__file__).resolve()
    wrapper_source = root / "scripts" / "harness-runtime-smoke"
    outer_harness_owns_cargo_lane = (
        enhanced_required
        and preflight_task is not None
        and preflight_task.get("mode") == "verified-snapshot"
    )
    if outer_harness_owns_cargo_lane:
        try:
            if not inherited_network_sandbox_is_active():
                raise RuntimeProofError(
                    "verified snapshot lacks a kernel-enforced outbound network restriction"
                )
            network_enforcement = "inherited-harness-seatbelt"
        except RuntimeProofError as exc:
            return emit("FAIL", str(exc), log_path, started_at)

    try:
        isolated_home = temp_root / "home"
        isolated_home.mkdir(parents=True)
        env = os.environ.copy()
        env.update(
            {
                "HOME": str(isolated_home),
                "CFFIXED_USER_HOME": str(isolated_home),
                "CARGO_HOME": env.get("CARGO_HOME", str(original_home / ".cargo")),
                "RUSTUP_HOME": env.get("RUSTUP_HOME", str(original_home / ".rustup")),
                "TMPDIR": str(temp_root / "tmp"),
                "npm_config_cache": str(temp_root / "npm-cache"),
                "MURMUR_DEV_DEK": DEV_DEK,
                "MURMUR_DEV_KEK": DEV_DEK,
                "MURMUR_HARNESS_RUNTIME_DIR": str(run_dir),
                "MISTRALRS_METAL_PRECOMPILE": "0",
            }
        )
        env["CARGO_TARGET_DIR"] = str(cargo_target_dir(root, env))
        Path(env["TMPDIR"]).mkdir(parents=True)

        with log_path.open("wb") as log:
            deadline = time.monotonic() + timeout
            cargo_command = [
                "cargo",
                "build",
                "--manifest-path",
                "src-tauri/Cargo.toml",
                "--bin",
                "Murmur",
            ]
            if not outer_harness_owns_cargo_lane:
                cargo_command = [
                    str(root / "scripts" / "agent-resource-run"),
                    "--deadline-seconds",
                    str(max(1, int(deadline - time.monotonic()))),
                    "--",
                    *cargo_command,
                ]
            frontend_proc = launch_guarded(
                label="Angular",
                command=[str(angular_bin), "serve", "--port", "1420"],
                root=root,
                env=env,
                log=log,
                result_path=run_dir / "angular-guardian.json",
                timeout_seconds=timeout + args.settle_seconds + 60,
                child_start_deadline=min(deadline, time.monotonic() + 15),
            )
            cargo_proc = launch_guarded(
                label="Cargo build",
                command=cargo_command,
                root=root,
                env=env,
                log=log,
                result_path=run_dir / "cargo-guardian.json",
                timeout_seconds=max(1, deadline - time.monotonic()),
                child_start_deadline=min(deadline, time.monotonic() + 15),
            )
            guardian_results["cargo"] = wait_guarded_natural(
                cargo_proc,
                deadline=deadline + 10,
                expected_exit=0,
            )
            app_binary = cargo_target_dir(root, env) / "debug" / "Murmur"
            if not app_binary.is_file() or app_binary.is_symlink():
                raise RuntimeProofError(
                    "Cargo succeeded but its exact real Rust app binary is missing or unsafe"
                )

            while time.monotonic() < deadline:
                frontend_result = frontend_proc.result_if_exited()
                if frontend_result is not None:
                    raise RuntimeProofError(
                        f"Angular process exited early with {frontend_result.get('exit_code')}"
                    )
                angular_pids = listener_pids(1420)
                if angular_pids and angular_pids != {frontend_proc.child_pid}:
                    raise RuntimeProofError("Angular port ownership changed to a foreign process")
                if angular_ready() and angular_pids == {frontend_proc.child_pid}:
                    break
                time.sleep(0.25)
            else:
                raise RuntimeProofError(
                    f"timed out after {timeout}s waiting for Angular ({mode} start)"
                )

            app_env = env.copy()
            app_command = [str(app_binary.resolve())]
            if enhanced_required:
                if sqlcipher_binary is None:
                    raise RuntimeProofError("SQLCipher disappeared after preflight")
                task_provenance = validate_task_provenance(root)
                runner_fingerprint = fingerprint_file(
                    runner_source, label="runtime runner source"
                )
                wrapper_fingerprint = fingerprint_file(
                    wrapper_source, label="runtime runner wrapper"
                )
                write_bytes_create_only(profile_path, NETWORK_PROFILE)
                profile_fingerprint = fingerprint_file(
                    profile_path, label="runtime network sandbox profile"
                )
                staged_identity = copy_file_create_only(app_binary, staged_binary)
                observer = ExternalEgressObserver()
                observer.prove_unauthorized_rejected()
                unauthorized_connections_rejected = observer.unauthorized_connections()
                observer.prove_liveness()
                for key in (
                    "ANTHROPIC_API_KEY",
                    "OPENAI_API_KEY",
                    "CLAUDE_API_KEY",
                    "OLLAMA_HOST",
                ):
                    app_env.pop(key, None)
                for key in (
                    "HTTP_PROXY",
                    "HTTPS_PROXY",
                    "ALL_PROXY",
                    "http_proxy",
                    "https_proxy",
                    "all_proxy",
                ):
                    app_env[key] = observer.endpoint
                app_env["NO_PROXY"] = ""
                app_env["no_proxy"] = ""
                runner_nonce = secrets.token_hex(32)
                challenge = {
                    "schemaVersion": 1,
                    "taskId": ENHANCED_RUNTIME_TASK_ID,
                    "runId": run_id,
                    "runnerPid": os.getpid(),
                    "runnerNonce": runner_nonce,
                    "binarySha256": staged_identity["sha256"],
                    "networkProfileSha256": profile_fingerprint["sha256"],
                    "runnerSourceSha256": runner_fingerprint["sha256"],
                    "taskContractSha256": task_provenance["sha256"],
                    "gitHead": task_provenance["gitHead"],
                    "startedAt": started_at,
                }
                write_json_create_only(challenge_path, challenge)
                app_env.update(
                    {
                        "MURMUR_HARNESS_RUN_ID": run_id,
                        "MURMUR_HARNESS_RUNNER_NONCE": runner_nonce,
                        "MURMUR_HARNESS_BINARY_SHA256": staged_identity["sha256"],
                        "MURMUR_HARNESS_EGRESS_PROXY": observer.endpoint,
                    }
                )
                current_stage = fingerprint_file(staged_binary, label="staged app binary")
                if not same_fingerprint(current_stage, staged_identity):
                    raise RuntimeProofError("staged app binary changed before first launch")
                if outer_harness_owns_cargo_lane:
                    app_command = [str(staged_binary)]
                else:
                    network_enforcement = "nested-runtime-seatbelt"
                    app_command = [
                        sandbox_binary,
                        "-f",
                        str(profile_path),
                        str(staged_binary),
                    ]

            first_app_proc = launch_guarded(
                label="first Rust app",
                command=app_command,
                root=root,
                env=app_env,
                log=log,
                result_path=run_dir / "first-app-guardian.json",
                timeout_seconds=max(1, deadline - time.monotonic()) + 30,
                child_start_deadline=min(deadline, time.monotonic() + 15),
            )
            if enhanced_required:
                if staged_identity is None:
                    raise RuntimeProofError("staged binary identity is absent")
                app_identities.append(
                    prove_mapped_executable(
                        first_app_proc.child_pid,
                        staged_identity,
                        deadline=min(deadline, time.monotonic() + 10),
                    )
                )

            restart_run_id: str | None = None
            while time.monotonic() < deadline:
                frontend_result = frontend_proc.result_if_exited()
                if frontend_result is not None:
                    raise RuntimeProofError(
                        f"Angular process exited early with {frontend_result.get('exit_code')}"
                    )
                if listener_pids(1420) != {frontend_proc.child_pid}:
                    raise RuntimeProofError(
                        "Angular readiness is no longer owned by its supervised child"
                    )

                if restart_run_id is None and intent_path.is_file():
                    if not enhanced_required or challenge is None:
                        raise RuntimeProofError(
                            "an enhanced restart intent appeared in an ordinary runtime task"
                        )
                    restart_run_id = validate_restart_intent(
                        intent_path, first_app_proc.child_pid, challenge
                    )

                if restart_run_id is not None and second_app_proc is None:
                    first_result = first_app_proc.result_if_exited()
                    if first_result is None:
                        time.sleep(0.1)
                        continue
                    validate_natural_guardian(first_app_proc, first_result, expected_exit=0)
                    guardian_results["firstApp"] = first_result
                    if listener_pids(8765):
                        raise RuntimeProofError("the MCP port was not closed between app generations")
                    if staged_identity is None or challenge is None:
                        raise RuntimeProofError("enhanced generation state is incomplete")
                    current_stage = fingerprint_file(staged_binary, label="staged app binary")
                    if not same_fingerprint(current_stage, staged_identity):
                        raise RuntimeProofError("staged app binary changed before second launch")
                    phase_two_nonce = secrets.token_hex(32)
                    witness_read_fd, witness_write_fd = os.pipe()
                    os.set_blocking(witness_read_fd, False)
                    second_env = app_env.copy()
                    second_env["MURMUR_HARNESS_PHASE_TWO_NONCE"] = phase_two_nonce
                    second_env["MURMUR_HARNESS_WITNESS_FD"] = str(witness_write_fd)
                    second_app_proc = launch_guarded(
                        label="second Rust app",
                        command=app_command,
                        root=root,
                        env=second_env,
                        log=log,
                        result_path=run_dir / "second-app-guardian.json",
                        timeout_seconds=max(1, deadline - time.monotonic()) + 30,
                        child_start_deadline=min(deadline, time.monotonic() + 15),
                        inherited_fds=(witness_write_fd,),
                    )
                    os.close(witness_write_fd)
                    witness_write_fd = -1
                    app_identities.append(
                        prove_mapped_executable(
                            second_app_proc.child_pid,
                            staged_identity,
                            deadline=min(deadline, time.monotonic() + 10),
                        )
                    )
                    write_json_create_only(
                        generation_path,
                        {
                            "schemaVersion": 1,
                            "runId": restart_run_id,
                            "runnerNonce": challenge["runnerNonce"],
                            "binarySha256": staged_identity["sha256"],
                            "firstPid": first_app_proc.child_pid,
                            "secondPid": second_app_proc.child_pid,
                            "runnerOwned": True,
                        },
                    )

                if second_app_proc is not None and witness_read_fd >= 0:
                    pipe_witness, _ = poll_json_pipe(
                        witness_read_fd, witness_buffer, pipe_witness
                    )

                active_app = second_app_proc or first_app_proc
                active_result = active_app.result_if_exited()
                if active_result is not None:
                    generation = "restarted" if restart_run_id is not None else "real"
                    raise RuntimeProofError(
                        f"{generation} Rust app process exited early with "
                        f"{active_result.get('exit_code')}"
                    )

                mcp_pids = listener_pids(8765)
                if mcp_pids and mcp_pids != {active_app.child_pid}:
                    raise RuntimeProofError(
                        "the MCP listener is not owned by the active supervised Rust process"
                    )
                if angular_ready() and mcp_pids == {active_app.child_pid}:
                    if enhanced_required:
                        if (
                            restart_run_id is None
                            or second_app_proc is None
                            or phase_two_nonce is None
                            or pipe_witness is None
                            or witness_path.exists()
                        ):
                            raise RuntimeProofError(
                                "enhanced MCP appeared without one private-pipe restart witness"
                            )
                    elif any(
                        path.exists()
                        for path in (
                            intent_path,
                            generation_path,
                            witness_path,
                            challenge_path,
                        )
                    ):
                        raise RuntimeProofError(
                            "enhanced runtime artifacts appeared in an ordinary task"
                        )
                    break
                time.sleep(0.25)
            else:
                raise RuntimeProofError(
                    f"timed out after {timeout}s waiting for Angular, the real Rust MCP "
                    f"listener, and any private-pipe witness ({mode} start)"
                )

            time.sleep(max(0, args.settle_seconds))
            active_app = second_app_proc or first_app_proc
            if frontend_proc.result_if_exited() is not None:
                raise RuntimeProofError("Angular process exited during settle")
            if active_app.result_if_exited() is not None:
                raise RuntimeProofError("real Rust app process exited during settle")
            if listener_pids(1420) != {frontend_proc.child_pid}:
                raise RuntimeProofError("Angular listener ownership changed during settle")
            if listener_pids(8765) != {active_app.child_pid}:
                raise RuntimeProofError("MCP listener ownership changed during settle")

            if enhanced_required:
                if (
                    challenge is None
                    or observer is None
                    or first_app_proc is None
                    or second_app_proc is None
                    or phase_two_nonce is None
                    or pipe_witness is None
                    or staged_identity is None
                    or task_provenance is None
                    or sqlcipher_binary is None
                ):
                    raise RuntimeProofError(
                        "enhanced runtime proof did not establish both generations"
                    )
                observer.assert_healthy()
                guardian_results["secondApp"] = request_guarded_shutdown(second_app_proc)
                pipe_witness = drain_json_pipe_to_eof(
                    witness_read_fd,
                    witness_buffer,
                    pipe_witness,
                    time.monotonic() + 5,
                )
                os.close(witness_read_fd)
                witness_read_fd = -1
                if listener_pids(8765):
                    raise RuntimeProofError("MCP remained open after second app shutdown")
                sqlcipher_evidence = verify_external_encrypted_database(
                    isolated_home, sqlcipher_binary
                )
                guardian_results["angular"] = request_guarded_shutdown(frontend_proc)
                provider_connections = observer.settle_and_close()
                observer_closed = True
                if provider_connections != 0:
                    raise RuntimeProofError(
                        "the external proxy observed a non-health egress connection"
                    )
                if observer.unauthorized_connections() != 1:
                    raise RuntimeProofError(
                        "the external proxy did not reject exactly one unauthorized local client"
                    )
                runtime_evidence = validate_runtime_witness(
                    pipe_witness,
                    challenge=challenge,
                    phase_two_nonce=phase_two_nonce,
                    first_pid=first_app_proc.child_pid,
                    second_pid=second_app_proc.child_pid,
                    proxy_connections=provider_connections,
                )
                runtime_evidence["unauthorized_loopback_connections_rejected"] = (
                    unauthorized_connections_rejected
                )
                current_stage = fingerprint_file(staged_binary, label="staged app binary")
                if not same_fingerprint(current_stage, staged_identity):
                    raise RuntimeProofError("staged app binary changed after execution")
                current_task = validate_task_provenance(root)
                if current_task["sha256"] != task_provenance["sha256"]:
                    raise RuntimeProofError("Harness task contract changed during execution")
                if (
                    fingerprint_file(runner_source, label="runtime runner source")["sha256"]
                    != challenge["runnerSourceSha256"]
                    or fingerprint_file(profile_path, label="network sandbox profile")["sha256"]
                    != challenge["networkProfileSha256"]
                ):
                    raise RuntimeProofError("runtime proof source or network profile changed")
                pass_reason = (
                    "Angular served; two mapped copies of the exact Rust binary completed "
                    "a private-pipe restart proof with zero observed or permitted remote egress"
                )
            else:
                guardian_results["firstApp"] = request_guarded_shutdown(first_app_proc)
                guardian_results["angular"] = request_guarded_shutdown(frontend_proc)
                pass_reason = (
                    "Angular served and the directly supervised real Rust MCP listener stayed alive"
                )

        text = log_path.read_text(errors="replace")
        fatal_markers = ("thread 'main' panicked", "fatal runtime error", "Abort trap", "SIGABRT")
        hits = [marker for marker in fatal_markers if marker.lower() in text.lower()]
        if hits:
            raise RuntimeProofError(f"fatal marker(s) in boot log: {', '.join(hits)}")
        if listener_pids(1420) or listener_pids(8765):
            raise RuntimeProofError("runtime listeners survived controlled shutdown")
        survivors = owned_process_group_survivors(
            (second_app_proc, first_app_proc, frontend_proc, cargo_proc)
        )
        if survivors:
            raise RuntimeProofError("an owned runtime process group survived supervision")
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as exc:
        failure_reason = str(exc)
    finally:
        if witness_write_fd >= 0:
            try:
                os.close(witness_write_fd)
            except OSError as exc:
                cleanup_errors.append(f"witness writer: {exc}")
        if witness_read_fd >= 0:
            try:
                os.close(witness_read_fd)
            except OSError as exc:
                cleanup_errors.append(f"witness reader: {exc}")
        for process in (second_app_proc, first_app_proc, frontend_proc, cargo_proc):
            if process is None or process.guardian.poll() is not None:
                continue
            try:
                fallback_terminate_guarded(process)
            except Exception as exc:
                cleanup_errors.append(f"{process.label}: {exc}")
        if observer is not None and not observer_closed:
            try:
                observer.force_close()
            except Exception as exc:
                cleanup_errors.append(f"egress observer: {exc}")
        try:
            shutil.rmtree(temp_root)
            temp_removed = not temp_root.exists()
            if not temp_removed:
                cleanup_errors.append("temporary HOME still exists after cleanup")
        except Exception as exc:
            cleanup_errors.append(f"temporary HOME: {exc}")

    try:
        if listener_pids(1420) or listener_pids(8765):
            cleanup_errors.append("runtime listener remained after final cleanup")
    except Exception as exc:
        cleanup_errors.append(f"listener audit: {exc}")
    try:
        survivors = owned_process_group_survivors(
            (second_app_proc, first_app_proc, frontend_proc, cargo_proc)
        )
        if survivors:
            cleanup_errors.append("owned process group remained after final cleanup")
    except Exception as exc:
        cleanup_errors.append(f"process-group audit: {exc}")

    if cleanup_errors:
        return emit(
            "FAIL",
            f"owned runtime cleanup was incomplete: {'; '.join(cleanup_errors)}",
            log_path,
            started_at,
        )
    if failure_reason is not None:
        return emit("FAIL", failure_reason, log_path, started_at)
    if pass_reason is None:
        return emit("FAIL", "runtime proof ended without a verdict", log_path, started_at)

    if enhanced_required:
        if (
            challenge is None
            or task_provenance is None
            or staged_identity is None
            or pipe_witness is None
            or runtime_evidence is None
            or sqlcipher_evidence is None
            or provider_connections is None
            or unauthorized_connections_rejected != 1
            or phase_two_nonce is None
            or len(app_identities) != 2
            or not temp_removed
        ):
            return emit(
                "FAIL",
                "enhanced runtime proof lacks final runner-owned evidence",
                log_path,
                started_at,
            )
        try:
            # The app never writes a shared witness file. Only the runner publishes the
            # already-validated pipe message after both app generations and cleanup finish.
            write_json_create_only(witness_path, pipe_witness)
            artifact_hashes = {
                "challenge": fingerprint_file(
                    challenge_path, label="runtime challenge"
                )["sha256"],
                "restartIntent": fingerprint_file(
                    intent_path, label="restart intent"
                )["sha256"],
                "restartGeneration": fingerprint_file(
                    generation_path, label="restart generation"
                )["sha256"],
                "runtimeWitness": fingerprint_file(
                    witness_path, label="runtime witness"
                )["sha256"],
                "cargoGuardian": fingerprint_file(
                    run_dir / "cargo-guardian.json", label="Cargo guardian result"
                )["sha256"],
                "angularGuardian": fingerprint_file(
                    run_dir / "angular-guardian.json", label="Angular guardian result"
                )["sha256"],
                "firstAppGuardian": fingerprint_file(
                    run_dir / "first-app-guardian.json", label="first app guardian result"
                )["sha256"],
                "secondAppGuardian": fingerprint_file(
                    run_dir / "second-app-guardian.json", label="second app guardian result"
                )["sha256"],
            }
            wrapper_fingerprint = fingerprint_file(
                wrapper_source, label="runtime runner wrapper"
            )
            runner_pass = {
                "schemaVersion": 1,
                "verdict": "PASS",
                "taskId": ENHANCED_RUNTIME_TASK_ID,
                "runId": run_id,
                "startedAt": started_at,
                "finishedAt": utc_now(),
                "gitHead": challenge["gitHead"],
                "binarySha256": challenge["binarySha256"],
                "stagedBinary": staged_identity,
                "appGenerations": app_identities,
                "provenance": {
                    "taskContractSha256": challenge["taskContractSha256"],
                    "runnerSourceSha256": challenge["runnerSourceSha256"],
                    "runnerWrapperSha256": wrapper_fingerprint["sha256"],
                    "networkProfileSha256": challenge["networkProfileSha256"],
                },
                "restart": {
                    "firstPid": first_app_proc.child_pid if first_app_proc else None,
                    "secondPid": second_app_proc.child_pid if second_app_proc else None,
                    "phaseTwoNonceSha256": hashlib.sha256(
                        phase_two_nonce.encode("ascii")
                    ).hexdigest(),
                    "restartCanaryRows": 2,
                    "witnessSha256": artifact_hashes["runtimeWitness"],
                },
                "network": {
                    "sandbox": "loopback-only",
                    "enforcement": network_enforcement,
                    "kernelRestrictedOutbound": True,
                    "externalProxyConnections": provider_connections,
                    "unauthorizedLocalConnectionsRejected": unauthorized_connections_rejected,
                    "observerClosedAfterApps": True,
                },
                "sqlcipher": sqlcipher_evidence,
                "cleanup": {
                    "portsClosed": True,
                    "ownedProcessGroupSurvivors": 0,
                    "temporaryHomeRemoved": True,
                    "guardiansParentLost": any(
                        result.get("parent_lost") is not False
                        for result in guardian_results.values()
                    ),
                },
                "artifactSha256": artifact_hashes,
                "checks": EXPECTED_RUNTIME_CHECKS,
            }
            if runner_pass["cleanup"]["guardiansParentLost"]:
                raise RuntimeProofError("a PASS guardian reported parent_lost")
            write_json_create_only(runner_pass_path, runner_pass)
            runner_pass_hash = fingerprint_file(
                runner_pass_path, label="final runner PASS"
            )["sha256"]
            runtime_evidence.update(
                {
                    "runner_pass_path": str(runner_pass_path),
                    "runner_pass_sha256": runner_pass_hash,
                    "runtime_witness_path": str(witness_path),
                    "runtime_witness_sha256": artifact_hashes["runtimeWitness"],
                    "network_sandbox": "loopback-only",
                    "external_sqlcipher_cli": True,
                    "temporary_home_removed": True,
                    "ports_closed": True,
                }
            )
        except (OSError, ValueError, RuntimeError) as exc:
            return emit("FAIL", str(exc), log_path, started_at)

    return emit("PASS", pass_reason, log_path, started_at, runtime_evidence)


if __name__ == "__main__":
    sys.exit(main())
