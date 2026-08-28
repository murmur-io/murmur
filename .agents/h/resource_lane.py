#!/usr/bin/env python3
"""Crash-safe FIFO admission for Murmur's workspace-wide Cargo lane."""

from __future__ import annotations

from dataclasses import dataclass
import fcntl
import json
import os
from pathlib import Path
import stat
import time
from typing import Any, IO, List, Optional
import uuid


QUEUE_DIRECTORY = "cargo-queue"
ADMIN_LOCK = "admin.lock"
SEQUENCE_FILE = "sequence"
TICKET_SUFFIX = ".ticket"


class LaneQueueError(RuntimeError):
    """The FIFO metadata cannot be used safely."""


@dataclass
class LaneTicket:
    queue_root: Path
    path: Path
    handle: IO[str]
    sequence: int
    queued_at: str
    queued_epoch: float
    released: bool = False


def _ensure_real_directory(path: Path) -> None:
    path.mkdir(parents=True, exist_ok=True)
    metadata = path.lstat()
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
        raise LaneQueueError(
            f"resource queue path is not a real directory: {path}"
        )


def _open_regular(path: Path, flags: int) -> IO[str]:
    descriptor = os.open(
        path,
        flags | getattr(os, "O_NOFOLLOW", 0),
        0o600,
    )
    try:
        if not stat.S_ISREG(os.fstat(descriptor).st_mode):
            raise LaneQueueError(
                f"resource queue file is not regular: {path}"
            )
        return os.fdopen(descriptor, "r+", encoding="utf-8")
    except BaseException:
        os.close(descriptor)
        raise


def _admin(queue_root: Path) -> IO[str]:
    handle = _open_regular(
        queue_root / ADMIN_LOCK,
        os.O_RDWR | os.O_CREAT,
    )
    fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
    return handle


def _live_names_locked(queue_root: Path) -> List[str]:
    live: List[str] = []
    for path in sorted(queue_root.glob(f"*{TICKET_SUFFIX}")):
        try:
            metadata = path.lstat()
        except FileNotFoundError:
            continue
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
            raise LaneQueueError(
                f"resource queue ticket is not a regular file: {path}"
            )
        try:
            candidate = _open_regular(path, os.O_RDWR)
        except FileNotFoundError:
            continue
        try:
            try:
                fcntl.flock(
                    candidate.fileno(),
                    fcntl.LOCK_EX | fcntl.LOCK_NB,
                )
            except BlockingIOError:
                live.append(path.name)
                continue
        finally:
            candidate.close()
        path.unlink(missing_ok=True)
    return live


def _next_sequence_locked(queue_root: Path) -> int:
    sequence_path = queue_root / SEQUENCE_FILE
    current = 0
    if sequence_path.exists() or sequence_path.is_symlink():
        with _open_regular(sequence_path, os.O_RDWR) as handle:
            raw = handle.read().strip()
        try:
            current = int(raw or "0")
        except ValueError as exc:
            raise LaneQueueError(
                f"resource queue sequence is malformed: {sequence_path}"
            ) from exc
        if current < 0:
            raise LaneQueueError(
                f"resource queue sequence is negative: {sequence_path}"
            )
    sequence = current + 1
    temporary = (
        queue_root
        / f".sequence-{os.getpid()}-{uuid.uuid4().hex}.tmp"
    )
    descriptor = os.open(
        temporary,
        os.O_WRONLY
        | os.O_CREAT
        | os.O_EXCL
        | getattr(os, "O_NOFOLLOW", 0),
        0o600,
    )
    try:
        payload = f"{sequence}\n".encode("ascii")
        if os.write(descriptor, payload) != len(payload):
            raise LaneQueueError("short resource queue sequence write")
        os.fsync(descriptor)
    finally:
        os.close(descriptor)
    try:
        os.replace(temporary, sequence_path)
        directory = os.open(queue_root, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        temporary.unlink(missing_ok=True)
    return sequence


def join_lane_queue(
    resource_root: Path,
    *,
    task: str,
    command: str,
) -> LaneTicket:
    """Append one flock-backed ticket to the FIFO."""

    _ensure_real_directory(resource_root)
    queue_root = resource_root / QUEUE_DIRECTORY
    _ensure_real_directory(queue_root)
    admin = _admin(queue_root)
    handle: Optional[IO[str]] = None
    path: Optional[Path] = None
    try:
        _live_names_locked(queue_root)
        sequence = _next_sequence_locked(queue_root)
        queued_at = time.strftime(
            "%Y-%m-%dT%H:%M:%SZ",
            time.gmtime(),
        )
        path = queue_root / (
            f"{sequence:020d}-{os.getpid()}-{uuid.uuid4().hex}"
            f"{TICKET_SUFFIX}"
        )
        handle = _open_regular(
            path,
            os.O_RDWR | os.O_CREAT | os.O_EXCL,
        )
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
        json.dump(
            {
                "schema_version": 1,
                "sequence": sequence,
                "pid": os.getpid(),
                "task": task,
                "command": command,
                "queued_at": queued_at,
            },
            handle,
            sort_keys=True,
        )
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
        return LaneTicket(
            queue_root,
            path,
            handle,
            sequence,
            queued_at,
            time.time(),
        )
    except BaseException:
        if handle is not None:
            handle.close()
        if path is not None:
            path.unlink(missing_ok=True)
        raise
    finally:
        admin.close()


def lane_queue_status(ticket: LaneTicket) -> dict[str, Any]:
    """Return one-based position after reaping dead waiters."""

    if ticket.released:
        raise LaneQueueError("resource lane ticket was already released")
    admin = _admin(ticket.queue_root)
    try:
        live = _live_names_locked(ticket.queue_root)
        try:
            position = live.index(ticket.path.name) + 1
        except ValueError as exc:
            raise LaneQueueError(
                f"live resource queue ticket disappeared: {ticket.path}"
            ) from exc
        return {
            "position": position,
            "depth": len(live),
            "sequence": ticket.sequence,
            "queued_at": ticket.queued_at,
            "wait_seconds": max(
                0.0,
                time.time() - ticket.queued_epoch,
            ),
        }
    finally:
        admin.close()


def leave_lane_queue(ticket: LaneTicket) -> None:
    """Idempotently remove only the caller's ticket."""

    if ticket.released:
        return
    admin = _admin(ticket.queue_root)
    try:
        ticket.path.unlink(missing_ok=True)
        ticket.handle.close()
        ticket.released = True
    finally:
        admin.close()
