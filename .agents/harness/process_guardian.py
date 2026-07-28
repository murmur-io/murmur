#!/usr/bin/env python3
"""Out-of-process parent-death and deadline guardian for harness children.

The harness process can be SIGKILLed, so an in-process ``finally`` block is not
enough to reap a Cargo/model child or release inherited flock descriptors. This
small supervisor receives a liveness pipe from its parent, starts exactly one
new-session child, and kills that owned process group if the pipe closes, a
signal arrives, or the deadline expires.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import time
from typing import Any, Dict, Optional, Sequence


POLL_SECONDS = 0.1


def utc_now() -> str:
    return (
        dt.datetime.now(dt.timezone.utc)
        .isoformat(timespec="seconds")
        .replace("+00:00", "Z")
    )


def atomic_json(path: Path, document: Dict[str, Any]) -> None:
    """Publish one guardian result without ever replacing an older artifact."""

    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    payload = (
        json.dumps(document, sort_keys=True, separators=(",", ":")) + "\n"
    ).encode("utf-8")
    descriptor = os.open(
        temporary,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL,
        0o600,
    )
    try:
        written = os.write(descriptor, payload)
        if written != len(payload):
            raise OSError("short guardian result write")
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    try:
        # A hard-link publish is atomic and fails with EEXIST. ``os.replace``
        # would silently destroy the prior crash evidence if an immediate
        # resume accidentally reused a result path.
        os.link(temporary, path)
    finally:
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass
    directory = os.open(path.parent, os.O_RDONLY)
    try:
        os.fsync(directory)
    finally:
        os.close(directory)


def group_exists(pgid: int) -> bool:
    try:
        os.killpg(pgid, 0)
        return True
    except ProcessLookupError:
        return False
    except PermissionError:
        return True


def terminate_group(process: subprocess.Popen[Any], grace: float) -> None:
    pgid = process.pid
    if not group_exists(pgid):
        process.poll()
        return
    try:
        os.killpg(pgid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    deadline = time.monotonic() + max(0.0, grace)
    while time.monotonic() < deadline:
        process.poll()  # reap a responsive group leader
        if not group_exists(pgid):
            return
        time.sleep(POLL_SECONDS)
    kill_deadline = time.monotonic() + 5.0
    permission_error: Optional[PermissionError] = None
    while time.monotonic() < kill_deadline:
        process.poll()
        if not group_exists(pgid):
            return
        try:
            os.killpg(pgid, signal.SIGKILL)
        except ProcessLookupError:
            return
        except PermissionError as exc:
            # A just-exited, not-yet-reaped leader can transiently report EPERM.
            # Reap/poll and retry, but never spin forever or silently release.
            permission_error = exc
        time.sleep(POLL_SECONDS)
    process.poll()
    if group_exists(pgid):
        if permission_error is not None:
            raise RuntimeError(
                f"could not kill owned process group {pgid}: {permission_error}"
            )
        raise RuntimeError(f"owned process group {pgid} survived SIGKILL")


def parse_args(argv: Sequence[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--parent-fd", required=True, type=int)
    parser.add_argument("--stdin-fd", type=int)
    parser.add_argument("--result", required=True)
    parser.add_argument("--cwd", required=True)
    parser.add_argument("--timeout-seconds", required=True, type=float)
    parser.add_argument("--term-grace-seconds", type=float, default=3.0)
    parser.add_argument("--pass-fd", action="append", type=int, default=[])
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    if args.command[:1] == ["--"]:
        args.command = args.command[1:]
    if not args.command:
        parser.error("missing child command")
    if args.timeout_seconds <= 0:
        parser.error("timeout must be positive")
    return args


def main(argv: Sequence[str]) -> int:
    args = parse_args(argv)
    result_path = Path(args.result)
    parent_fd = args.parent_fd
    stdin_fd: Optional[int] = args.stdin_fd
    stop_signal: Optional[int] = None

    def request_stop(signum: int, _frame: Any) -> None:
        nonlocal stop_signal
        stop_signal = signum

    for signum in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(signum, request_stop)

    started = time.monotonic()
    started_at = utc_now()
    child: Optional[subprocess.Popen[Any]] = None
    timed_out = False
    parent_lost = False
    leader_exited_with_live_group = False
    termination_reason: Optional[str] = None
    try:
        child = subprocess.Popen(
            list(args.command),
            cwd=args.cwd,
            env=os.environ.copy(),
            stdin=stdin_fd if stdin_fd is not None else subprocess.DEVNULL,
            stdout=None,
            stderr=None,
            start_new_session=True,
            pass_fds=tuple(args.pass_fd),
        )
        if stdin_fd is not None:
            os.close(stdin_fd)
            stdin_fd = None
        deadline = started + args.timeout_seconds
        while True:
            if child.poll() is not None:
                leader_exited_with_live_group = group_exists(child.pid)
                if leader_exited_with_live_group:
                    termination_reason = "leader-exited-with-live-group"
                break
            if stop_signal is not None:
                termination_reason = "guardian-signal"
                break
            now = time.monotonic()
            if now >= deadline:
                timed_out = True
                termination_reason = "timeout"
                break
            readable, _, _ = select.select(
                [parent_fd],
                [],
                [],
                min(POLL_SECONDS, max(0.0, deadline - now)),
            )
            if readable:
                marker = os.read(parent_fd, 1)
                if marker == b"":
                    parent_lost = True
                    termination_reason = "parent-lost"
                    break
        # A successful group leader is not proof that the owned process group
        # is gone: shell commands, Cargo wrappers, and model CLIs can leave
        # same-PGID descendants alive. Always inspect and drain the group before
        # publishing a result, including after the leader has already exited.
        if group_exists(child.pid):
            terminate_group(child, args.term_grace_seconds)
        leader_exit_code = child.wait()
        # A command which returned success while leaving owned background work
        # behind did not terminate cleanly. Cleanup prevents an orphan, but it
        # must not green-wash the check/model invocation that leaked it.
        exit_code = (
            125
            if leader_exited_with_live_group and leader_exit_code == 0
            else leader_exit_code
        )
        atomic_json(
            result_path,
            {
                "schema_version": 1,
                "child_pid": child.pid,
                "exit_code": exit_code,
                "leader_exit_code": leader_exit_code,
                "timed_out": timed_out,
                "parent_lost": parent_lost,
                "guardian_signal": stop_signal,
                "leader_exited_with_live_group": leader_exited_with_live_group,
                "termination_reason": termination_reason,
                "started_at": started_at,
                "finished_at": utc_now(),
                "duration_ms": int((time.monotonic() - started) * 1000),
            },
        )
        return 0
    except BaseException as exc:
        if child is not None and child.poll() is None:
            terminate_group(child, args.term_grace_seconds)
        try:
            atomic_json(
                result_path,
                {
                    "schema_version": 1,
                    "child_pid": child.pid if child is not None else None,
                    "exit_code": child.poll() if child is not None else None,
                    "leader_exit_code": child.poll() if child is not None else None,
                    "timed_out": timed_out,
                    "parent_lost": parent_lost,
                    "guardian_signal": stop_signal,
                    "leader_exited_with_live_group": leader_exited_with_live_group,
                    "termination_reason": termination_reason,
                    "started_at": started_at,
                    "finished_at": utc_now(),
                    "duration_ms": int((time.monotonic() - started) * 1000),
                    "error": f"{type(exc).__name__}: {exc}",
                },
            )
        except BaseException:
            pass
        return 125
    finally:
        try:
            os.close(parent_fd)
        except OSError:
            pass
        if stdin_fd is not None:
            try:
                os.close(stdin_fd)
            except OSError:
                pass


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
