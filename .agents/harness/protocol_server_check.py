#!/usr/bin/env python3
"""Fail-closed runner for the pinned murmur-server protocol check.

The Harness executes this file from the immutable client verification snapshot.
It proves the task-local sibling checkout before replacing this process with the
complete server workspace test.  The first stdout line is a canonical facts
record; verifier.py admits only that fixed shape into check/reviewer evidence.
"""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
from typing import Any, Dict, Mapping, Sequence, Set, Tuple


DATABASE_URL = "postgresql://postgres:postgres@localhost:5433/urc_dev"
TEST_ARGV = ("cargo", "test", "--workspace", "--", "--test-threads=1")
FACTS_PREFIX = "MURMUR_HARNESS_PROTOCOL_SERVER_FACTS="
SHA1_RE = re.compile(r"[0-9a-f]{40}")


class ProtocolServerCheckError(RuntimeError):
    """The task-local server checkout cannot support an honest verdict."""


def _canonical_json(value: Any) -> bytes:
    return json.dumps(
        value,
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
    ).encode("utf-8")


def _facts_hash(facts: Dict[str, Any]) -> str:
    payload = {key: value for key, value in facts.items() if key != "facts_sha256"}
    return hashlib.sha256(_canonical_json(payload)).hexdigest()


def _git(
    repo: Path,
    arguments: Sequence[str],
    *,
    accepted: Sequence[int] = (0,),
) -> subprocess.CompletedProcess[str]:
    git_env = {
        key: value for key, value in os.environ.items() if not key.startswith("GIT_")
    }
    git_env.update(
        {
            "GIT_CONFIG_GLOBAL": "/dev/null",
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_LITERAL_PATHSPECS": "1",
            "GIT_NO_REPLACE_OBJECTS": "1",
            "GIT_OPTIONAL_LOCKS": "0",
            "GIT_TERMINAL_PROMPT": "0",
            "LC_ALL": "C",
        }
    )
    completed = subprocess.run(
        [
            "git",
            "-C",
            str(repo),
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.preloadIndex=false",
            "-c",
            "core.untrackedCache=false",
            *arguments,
        ],
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        env=git_env,
    )
    if completed.returncode not in accepted:
        detail = completed.stderr.strip() or completed.stdout.strip() or "no detail"
        raise ProtocolServerCheckError(
            f"git {' '.join(arguments)} failed ({completed.returncode}): {detail}"
        )
    return completed


def _git_bytes(
    repo: Path,
    arguments: Sequence[str],
    *,
    accepted: Sequence[int] = (0,),
) -> subprocess.CompletedProcess[bytes]:
    git_env = {
        key: value for key, value in os.environ.items() if not key.startswith("GIT_")
    }
    git_env.update(
        {
            "GIT_CONFIG_GLOBAL": "/dev/null",
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_LITERAL_PATHSPECS": "1",
            "GIT_NO_REPLACE_OBJECTS": "1",
            "GIT_OPTIONAL_LOCKS": "0",
            "GIT_TERMINAL_PROMPT": "0",
            "LC_ALL": "C",
        }
    )
    completed = subprocess.run(
        [
            "git",
            "-C",
            str(repo),
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.preloadIndex=false",
            "-c",
            "core.untrackedCache=false",
            *arguments,
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        env=git_env,
    )
    if completed.returncode not in accepted:
        detail = completed.stderr.decode("utf-8", "replace").strip()
        if not detail:
            detail = completed.stdout.decode("utf-8", "replace").strip() or "no detail"
        raise ProtocolServerCheckError(
            f"git {' '.join(arguments)} failed ({completed.returncode}): {detail}"
        )
    return completed


def _reject_external_git_config(server: Path) -> None:
    """Reject local config directives that import bytes outside local `.git`."""

    git_environment = {
        key: value for key, value in os.environ.items() if not key.startswith("GIT_")
    }
    git_environment.update(
        {
            "GIT_CONFIG_GLOBAL": "/dev/null",
            "GIT_CONFIG_NOSYSTEM": "1",
            "GIT_TERMINAL_PROMPT": "0",
            "LC_ALL": "C",
        }
    )
    for name in ("config", "config.worktree"):
        config_path = server / ".git" / name
        try:
            metadata = config_path.lstat()
        except FileNotFoundError:
            continue
        except OSError as exc:
            raise ProtocolServerCheckError(
                f"server Git {name} cannot be inspected"
            ) from exc
        if (
            stat.S_ISLNK(metadata.st_mode)
            or not stat.S_ISREG(metadata.st_mode)
            or metadata.st_nlink != 1
        ):
            raise ProtocolServerCheckError(
                f"server Git {name} must be a local single-link regular file"
            )
        completed = subprocess.run(
            [
                "git",
                "config",
                "--file",
                str(config_path),
                "--no-includes",
                "--null",
                "--list",
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            env=git_environment,
        )
        if completed.returncode != 0:
            detail = completed.stderr.decode("utf-8", "replace").strip() or "no detail"
            raise ProtocolServerCheckError(
                f"server Git {name} is malformed: {detail}"
            )
        for record in completed.stdout.split(b"\0"):
            if not record:
                continue
            key = record.split(b"\n", 1)[0].decode("utf-8", "surrogateescape").lower()
            if key == "include.path" or (
                key.startswith("includeif.") and key.endswith(".path")
            ):
                raise ProtocolServerCheckError(
                    "server Git config must not include external config"
                )


def _regular_file(path: Path, label: str) -> None:
    try:
        metadata = path.lstat()
    except OSError as exc:
        raise ProtocolServerCheckError(f"{label} is missing: {path}") from exc
    if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(metadata.st_mode):
        raise ProtocolServerCheckError(
            f"{label} must be a regular non-symlink file: {path}"
        )
    if metadata.st_nlink != 1:
        raise ProtocolServerCheckError(f"{label} must be a single-link file: {path}")


def _open_directory(path: Path, label: str) -> int:
    flags = os.O_RDONLY
    if hasattr(os, "O_CLOEXEC"):
        flags |= os.O_CLOEXEC
    if hasattr(os, "O_DIRECTORY"):
        flags |= os.O_DIRECTORY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(path, flags)
    except OSError as exc:
        raise ProtocolServerCheckError(f"{label} is not a real directory") from exc
    if not stat.S_ISDIR(os.fstat(descriptor).st_mode):
        os.close(descriptor)
        raise ProtocolServerCheckError(f"{label} is not a real directory")
    return descriptor


def _bound_name_stat(
    parent_fd: int,
    name: str,
    opened: os.stat_result,
    label: str,
) -> os.stat_result:
    try:
        rebound = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
    except OSError as exc:
        raise ProtocolServerCheckError(f"{label} changed during inspection") from exc
    if (
        rebound.st_dev != opened.st_dev
        or rebound.st_ino != opened.st_ino
        or rebound.st_mode != opened.st_mode
    ):
        raise ProtocolServerCheckError(f"{label} changed during inspection")
    return rebound


def _read_regular_fd(
    parent_fd: int,
    name: str,
    relative: bytes,
    *,
    require_single_link: bool,
) -> Tuple[int, str]:
    try:
        before = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
    except OSError as exc:
        raise ProtocolServerCheckError(
            "checkout entry cannot be inspected: " + os.fsdecode(relative)
        ) from exc
    if not stat.S_ISREG(before.st_mode):
        raise ProtocolServerCheckError(
            "checkout entries must be regular files: " + os.fsdecode(relative)
        )
    if require_single_link and before.st_nlink != 1:
        raise ProtocolServerCheckError(
            "checkout files must be single-link: " + os.fsdecode(relative)
        )
    flags = os.O_RDONLY
    if hasattr(os, "O_CLOEXEC"):
        flags |= os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(name, flags, dir_fd=parent_fd)
    except OSError as exc:
        raise ProtocolServerCheckError(
            "checkout entry cannot be opened safely: " + os.fsdecode(relative)
        ) from exc
    try:
        opened = os.fstat(descriptor)
        if (
            not stat.S_ISREG(opened.st_mode)
            or opened.st_dev != before.st_dev
            or opened.st_ino != before.st_ino
            or (require_single_link and opened.st_nlink != 1)
        ):
            raise ProtocolServerCheckError(
                "checkout entry changed during inspection: " + os.fsdecode(relative)
            )
        digest = hashlib.sha256()
        byte_count = 0
        while True:
            chunk = os.read(descriptor, 1024 * 1024)
            if not chunk:
                break
            byte_count += len(chunk)
            digest.update(chunk)
        finished = os.fstat(descriptor)
        if (
            finished.st_dev != opened.st_dev
            or finished.st_ino != opened.st_ino
            or finished.st_mode != opened.st_mode
            or finished.st_nlink != opened.st_nlink
            or finished.st_size != opened.st_size
            or finished.st_mtime_ns != opened.st_mtime_ns
            or finished.st_ctime_ns != opened.st_ctime_ns
            or byte_count != opened.st_size
        ):
            raise ProtocolServerCheckError(
                "checkout entry changed during inspection: " + os.fsdecode(relative)
            )
        _bound_name_stat(parent_fd, name, opened, os.fsdecode(relative))
        return byte_count, digest.hexdigest()
    finally:
        os.close(descriptor)


def _manifest_digest(
    entries: Mapping[bytes, Tuple[str, str, int, str]],
) -> str:
    digest = hashlib.sha256()
    for path in sorted(entries):
        mode, oid, byte_count, content_sha256 = entries[path]
        digest.update(len(path).to_bytes(8, "big") + path)
        for value in (mode, oid, str(byte_count), content_sha256):
            encoded = value.encode("ascii")
            digest.update(len(encoded).to_bytes(8, "big") + encoded)
    return digest.hexdigest()


def _validate_repo_path(path: bytes) -> None:
    if (
        not path
        or path.startswith(b"/")
        or b"\0" in path
        or any(part in {b"", b".", b".."} for part in path.split(b"/"))
    ):
        raise ProtocolServerCheckError("pinned server tree contains an unsafe path")


def _verify_git_object(kind: bytes, oid: str, content: bytes) -> None:
    framed = kind + b" " + str(len(content)).encode("ascii") + b"\0" + content
    if hashlib.sha1(framed).hexdigest() != oid:
        raise ProtocolServerCheckError(
            "pinned server object bytes do not match their SHA-1 identity"
        )


def _commit_tree_oid(commit: bytes) -> str:
    first_line, separator, _ = commit.partition(b"\n")
    prefix = b"tree "
    if not separator or not first_line.startswith(prefix):
        raise ProtocolServerCheckError("pinned server commit has no canonical tree")
    try:
        tree_oid = first_line[len(prefix) :].decode("ascii")
    except UnicodeError as exc:
        raise ProtocolServerCheckError("pinned server tree identity is malformed") from exc
    if SHA1_RE.fullmatch(tree_oid) is None:
        raise ProtocolServerCheckError("pinned server tree identity is malformed")
    return tree_oid


def _tree_manifest_entries(
    server: Path,
    tree_oid: str,
    *,
    prefix: bytes = b"",
    depth: int = 0,
) -> Dict[bytes, Tuple[str, str, int, str]]:
    """Parse and self-hash every object reachable from one pinned tree."""

    if depth > 1024:
        raise ProtocolServerCheckError("pinned server tree nesting is excessive")
    tree_bytes = _git_bytes(server, ("cat-file", "tree", tree_oid)).stdout
    _verify_git_object(b"tree", tree_oid, tree_bytes)
    entries: Dict[bytes, Tuple[str, str, int, str]] = {}
    local_names: set[bytes] = set()
    cursor = 0
    while cursor < len(tree_bytes):
        mode_end = tree_bytes.find(b" ", cursor)
        name_end = tree_bytes.find(b"\0", mode_end + 1)
        if mode_end <= cursor or name_end <= mode_end + 1:
            raise ProtocolServerCheckError("pinned server tree is malformed")
        oid_start = name_end + 1
        oid_end = oid_start + 20
        if oid_end > len(tree_bytes):
            raise ProtocolServerCheckError("pinned server tree is malformed")
        raw_mode = tree_bytes[cursor:mode_end]
        name = tree_bytes[mode_end + 1 : name_end]
        if (
            name in local_names
            or name in {b"", b".", b".."}
            or b"/" in name
            or b"\0" in name
        ):
            raise ProtocolServerCheckError("pinned server tree is malformed")
        local_names.add(name)
        oid = tree_bytes[oid_start:oid_end].hex()
        path = prefix + (b"/" if prefix else b"") + name
        _validate_repo_path(path)
        if raw_mode == b"40000":
            children = _tree_manifest_entries(
                server,
                oid,
                prefix=path,
                depth=depth + 1,
            )
            if set(entries).intersection(children):
                raise ProtocolServerCheckError("pinned server tree is malformed")
            entries.update(children)
        elif raw_mode in {b"100644", b"100755"}:
            content = _git_bytes(server, ("cat-file", "blob", oid)).stdout
            _verify_git_object(b"blob", oid, content)
            entries[path] = (
                raw_mode.decode("ascii"),
                oid,
                len(content),
                hashlib.sha256(content).hexdigest(),
            )
        else:
            raise ProtocolServerCheckError(
                "pinned server tree may contain only directories and regular files"
            )
        cursor = oid_end
    return entries


def _head_manifest(
    server: Path,
    revision: str,
) -> Tuple[str, Dict[bytes, Tuple[str, str, int, str]], str]:
    object_format = _git(
        server, ("rev-parse", "--show-object-format=storage")
    ).stdout.strip()
    if object_format != "sha1":
        raise ProtocolServerCheckError("pinned server repository must use SHA-1 objects")
    commit = _git_bytes(server, ("cat-file", "commit", revision)).stdout
    _verify_git_object(b"commit", revision, commit)
    tree_oid = _commit_tree_oid(commit)
    entries = _tree_manifest_entries(server, tree_oid)
    return tree_oid, entries, _manifest_digest(entries)


def _index_manifest(
    server: Path,
    expected: Mapping[bytes, Tuple[str, str, int, str]],
) -> str:
    indexed: Dict[bytes, Tuple[str, str, int, str]] = {}
    raw_index = _git_bytes(
        server, ("ls-files", "--stage", "--full-name", "-z")
    ).stdout
    for record in raw_index.split(b"\0"):
        if not record:
            continue
        try:
            header, path = record.split(b"\t", 1)
            raw_mode, raw_oid, raw_stage = header.split(b" ", 2)
            mode = raw_mode.decode("ascii")
            oid = raw_oid.decode("ascii")
            stage = raw_stage.decode("ascii")
        except (UnicodeError, ValueError) as exc:
            raise ProtocolServerCheckError("pinned server index is malformed") from exc
        _validate_repo_path(path)
        expected_entry = expected.get(path)
        if (
            stage != "0"
            or expected_entry is None
            or mode != expected_entry[0]
            or oid != expected_entry[1]
            or path in indexed
        ):
            raise ProtocolServerCheckError(
                "pinned server index differs from the pinned revision"
            )
        indexed[path] = expected_entry
    if set(indexed) != set(expected):
        raise ProtocolServerCheckError(
            "pinned server index differs from the pinned revision"
        )
    return _manifest_digest(indexed)


def _open_child_directory(
    parent_fd: int,
    name: str,
    relative: bytes,
) -> Tuple[int, os.stat_result]:
    try:
        before = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
    except OSError as exc:
        raise ProtocolServerCheckError(
            "checkout directory cannot be inspected: " + os.fsdecode(relative)
        ) from exc
    if not stat.S_ISDIR(before.st_mode):
        raise ProtocolServerCheckError(
            "checkout entry must be a real directory: " + os.fsdecode(relative)
        )
    flags = os.O_RDONLY
    if hasattr(os, "O_CLOEXEC"):
        flags |= os.O_CLOEXEC
    if hasattr(os, "O_DIRECTORY"):
        flags |= os.O_DIRECTORY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(name, flags, dir_fd=parent_fd)
    except OSError as exc:
        raise ProtocolServerCheckError(
            "checkout directory cannot be opened safely: " + os.fsdecode(relative)
        ) from exc
    opened = os.fstat(descriptor)
    if (
        not stat.S_ISDIR(opened.st_mode)
        or opened.st_dev != before.st_dev
        or opened.st_ino != before.st_ino
    ):
        os.close(descriptor)
        raise ProtocolServerCheckError(
            "checkout directory changed during inspection: " + os.fsdecode(relative)
        )
    return descriptor, opened


def _worktree_manifest(
    server_fd: int,
    expected: Mapping[bytes, Tuple[str, str, int, str]],
) -> str:
    expected_directories: Set[bytes] = set()
    for path in expected:
        parts = path.split(b"/")
        for index in range(1, len(parts)):
            expected_directories.add(b"/".join(parts[:index]))

    observed: Dict[bytes, Tuple[str, str, int, str]] = {}
    observed_directories: Set[bytes] = set()

    def visit(directory_fd: int, prefix: bytes, *, root: bool) -> None:
        try:
            names = sorted(os.listdir(directory_fd), key=os.fsencode)
        except OSError as exc:
            raise ProtocolServerCheckError("server worktree cannot be enumerated") from exc
        for name in names:
            raw_name = os.fsencode(name)
            if root and raw_name == b".git":
                continue
            if raw_name in {b"", b".", b".."} or b"/" in raw_name:
                raise ProtocolServerCheckError("server worktree contains an unsafe path")
            relative = raw_name if not prefix else prefix + b"/" + raw_name
            try:
                metadata = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
            except OSError as exc:
                raise ProtocolServerCheckError(
                    "server worktree entry cannot be inspected: " + os.fsdecode(relative)
                ) from exc
            if stat.S_ISDIR(metadata.st_mode):
                if relative not in expected_directories:
                    raise ProtocolServerCheckError(
                        "pinned server checkout contains an extra directory: "
                        + os.fsdecode(relative)
                    )
                child_fd, opened = _open_child_directory(directory_fd, name, relative)
                try:
                    observed_directories.add(relative)
                    visit(child_fd, relative, root=False)
                    finished = os.fstat(child_fd)
                    if (
                        finished.st_dev != opened.st_dev
                        or finished.st_ino != opened.st_ino
                        or finished.st_mode != opened.st_mode
                        or finished.st_mtime_ns != opened.st_mtime_ns
                        or finished.st_ctime_ns != opened.st_ctime_ns
                    ):
                        raise ProtocolServerCheckError(
                            "checkout directory changed during inspection: "
                            + os.fsdecode(relative)
                        )
                    _bound_name_stat(directory_fd, name, opened, os.fsdecode(relative))
                finally:
                    os.close(child_fd)
                continue
            expected_entry = expected.get(relative)
            if expected_entry is None:
                raise ProtocolServerCheckError(
                    "pinned server checkout contains an untracked entry: "
                    + os.fsdecode(relative)
                )
            byte_count, content_sha256 = _read_regular_fd(
                directory_fd,
                name,
                relative,
                require_single_link=True,
            )
            executable = bool(metadata.st_mode & 0o111)
            actual_mode = "100755" if executable else "100644"
            if (
                actual_mode != expected_entry[0]
                or byte_count != expected_entry[2]
                or content_sha256 != expected_entry[3]
            ):
                raise ProtocolServerCheckError(
                    "pinned server checkout tracked bytes differ from the pinned revision: "
                    + os.fsdecode(relative)
                )
            observed[relative] = expected_entry

    visit(server_fd, b"", root=True)
    if set(observed) != set(expected) or observed_directories != expected_directories:
        raise ProtocolServerCheckError(
            "pinned server checkout does not exactly materialize the pinned revision"
        )
    digest = hashlib.sha256()
    for directory in sorted(observed_directories):
        digest.update(b"D" + len(directory).to_bytes(8, "big") + directory)
    digest.update(bytes.fromhex(_manifest_digest(observed)))
    return digest.hexdigest()


def _metadata_manifest(server_fd: int) -> Tuple[str, int, str]:
    git_fd, git_root = _open_child_directory(server_fd, ".git", b".git")
    digest = hashlib.sha256()
    entry_count = 0
    checkout_mode = "local-isolated-clone"

    def visit(directory_fd: int, prefix: bytes) -> None:
        nonlocal checkout_mode, entry_count
        try:
            names = sorted(os.listdir(directory_fd), key=os.fsencode)
        except OSError as exc:
            raise ProtocolServerCheckError("server Git metadata cannot be enumerated") from exc
        for name in names:
            raw_name = os.fsencode(name)
            relative = raw_name if not prefix else prefix + b"/" + raw_name
            try:
                metadata = os.stat(name, dir_fd=directory_fd, follow_symlinks=False)
            except OSError as exc:
                raise ProtocolServerCheckError(
                    "server Git metadata cannot be inspected: " + os.fsdecode(relative)
                ) from exc
            entry_count += 1
            if stat.S_ISLNK(metadata.st_mode):
                raise ProtocolServerCheckError(
                    "server Git metadata must not contain symlinks: "
                    + os.fsdecode(relative)
                )
            if stat.S_ISDIR(metadata.st_mode):
                child_fd, opened = _open_child_directory(directory_fd, name, relative)
                try:
                    digest.update(b"D" + len(relative).to_bytes(8, "big") + relative)
                    visit(child_fd, relative)
                    finished = os.fstat(child_fd)
                    if (
                        finished.st_dev != opened.st_dev
                        or finished.st_ino != opened.st_ino
                        or finished.st_mode != opened.st_mode
                        or finished.st_mtime_ns != opened.st_mtime_ns
                        or finished.st_ctime_ns != opened.st_ctime_ns
                    ):
                        raise ProtocolServerCheckError(
                            "server Git metadata changed during inspection: "
                            + os.fsdecode(relative)
                        )
                    _bound_name_stat(directory_fd, name, opened, os.fsdecode(relative))
                finally:
                    os.close(child_fd)
                continue
            if not stat.S_ISREG(metadata.st_mode):
                raise ProtocolServerCheckError(
                    "server Git metadata may contain only directories and regular files: "
                    + os.fsdecode(relative)
                )
            byte_count, content_sha256 = _read_regular_fd(
                directory_fd,
                name,
                relative,
                require_single_link=True,
            )
            digest.update(b"F" + len(relative).to_bytes(8, "big") + relative)
            digest.update(byte_count.to_bytes(8, "big"))
            digest.update(bytes.fromhex(content_sha256))
            if relative == b"objects/info/alternates":
                flags = os.O_RDONLY
                if hasattr(os, "O_CLOEXEC"):
                    flags |= os.O_CLOEXEC
                if hasattr(os, "O_NOFOLLOW"):
                    flags |= os.O_NOFOLLOW
                alternate_fd = os.open(name, flags, dir_fd=directory_fd)
                try:
                    alternate_bytes = b""
                    while True:
                        chunk = os.read(alternate_fd, 64 * 1024)
                        if not chunk:
                            break
                        alternate_bytes += chunk
                finally:
                    os.close(alternate_fd)
                lines = alternate_bytes.splitlines()
                if (
                    len(lines) != 1
                    or not lines[0]
                    or not os.path.isabs(os.fsdecode(lines[0]))
                ):
                    raise ProtocolServerCheckError(
                        "server object alternates must contain one absolute non-empty path"
                    )
                checkout_mode = "local-shared-clone"

    try:
        visit(git_fd, b"")
        finished = os.fstat(git_fd)
        if (
            finished.st_dev != git_root.st_dev
            or finished.st_ino != git_root.st_ino
            or finished.st_mode != git_root.st_mode
            or finished.st_mtime_ns != git_root.st_mtime_ns
            or finished.st_ctime_ns != git_root.st_ctime_ns
        ):
            raise ProtocolServerCheckError(
                "server Git metadata changed during inspection"
            )
        _bound_name_stat(server_fd, ".git", git_root, ".git")
        return digest.hexdigest(), entry_count, checkout_mode
    finally:
        os.close(git_fd)


def _read_pin(client: Path) -> str:
    pin_path = client / ".murmur-server-revision"
    try:
        before = pin_path.lstat()
    except OSError as exc:
        raise ProtocolServerCheckError("pinned server revision is missing") from exc
    if (
        not stat.S_ISREG(before.st_mode)
        or stat.S_ISLNK(before.st_mode)
        or before.st_nlink != 1
    ):
        raise ProtocolServerCheckError("pinned server revision is unsafe")
    flags = os.O_RDONLY
    if hasattr(os, "O_CLOEXEC"):
        flags |= os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(pin_path, flags)
        try:
            opened = os.fstat(descriptor)
            if (
                not stat.S_ISREG(opened.st_mode)
                or opened.st_nlink != 1
                or opened.st_dev != before.st_dev
                or opened.st_ino != before.st_ino
                or opened.st_mode != before.st_mode
            ):
                raise ProtocolServerCheckError("pinned server revision is unsafe")
            content = b""
            while True:
                chunk = os.read(descriptor, 4096)
                if not chunk:
                    break
                content += chunk
            finished = os.fstat(descriptor)
        finally:
            os.close(descriptor)
    except OSError as exc:
        raise ProtocolServerCheckError("pinned server revision is unreadable") from exc
    stable_fields = (
        "st_dev",
        "st_ino",
        "st_mode",
        "st_nlink",
        "st_size",
        "st_mtime_ns",
        "st_ctime_ns",
    )
    try:
        rebound = pin_path.lstat()
    except OSError as exc:
        raise ProtocolServerCheckError("pinned server revision is unsafe") from exc
    if (
        any(getattr(finished, field) != getattr(opened, field) for field in stable_fields)
        or any(getattr(rebound, field) != getattr(finished, field) for field in stable_fields)
        or len(content) != opened.st_size
    ):
        raise ProtocolServerCheckError("pinned server revision is unsafe")
    try:
        revision = content.decode("ascii").strip()
    except UnicodeError as exc:
        raise ProtocolServerCheckError("pinned server revision is malformed") from exc
    if SHA1_RE.fullmatch(revision) is None:
        raise ProtocolServerCheckError("pinned server revision is malformed")
    return revision


def _server_path_matches_fd(server: Path, server_fd: int) -> None:
    try:
        path_metadata = server.lstat()
    except OSError as exc:
        raise ProtocolServerCheckError(
            "task-local server checkout changed during inspection"
        ) from exc
    opened = os.fstat(server_fd)
    if (
        not stat.S_ISDIR(path_metadata.st_mode)
        or path_metadata.st_dev != opened.st_dev
        or path_metadata.st_ino != opened.st_ino
        or path_metadata.st_mode != opened.st_mode
    ):
        raise ProtocolServerCheckError(
            "task-local server checkout changed during inspection"
        )


def _capture_checkout(
    client: Path,
    server: Path,
    server_fd: int,
) -> Dict[str, Any]:
    _server_path_matches_fd(server, server_fd)
    revision = _read_pin(client)
    expected_git = (server / ".git").resolve()
    git_metadata_sha256, metadata_entry_count, checkout_mode = _metadata_manifest(
        server_fd
    )
    _reject_external_git_config(server)
    git_dir = Path(
        _git(server, ("rev-parse", "--path-format=absolute", "--git-dir")).stdout.strip()
    ).resolve()
    git_common_dir = Path(
        _git(
            server,
            ("rev-parse", "--path-format=absolute", "--git-common-dir"),
        ).stdout.strip()
    ).resolve()
    top_level = Path(
        _git(server, ("rev-parse", "--show-toplevel")).stdout.strip()
    ).resolve()
    if git_dir != expected_git or git_common_dir != expected_git:
        raise ProtocolServerCheckError(
            "task-local server checkout must use isolated Git metadata"
        )
    if top_level != server.resolve():
        raise ProtocolServerCheckError("task-local server checkout is not its Git root")
    resolved_revision = _git(
        server,
        ("rev-parse", "--verify", "--end-of-options", f"{revision}^{{commit}}"),
    ).stdout.strip()
    head = _git(server, ("rev-parse", "HEAD")).stdout.strip()
    if resolved_revision != revision or head != revision:
        raise ProtocolServerCheckError(
            f"server HEAD differs from pinned revision: expected {revision}, found {head}"
        )
    symbolic = _git(
        server,
        ("symbolic-ref", "--quiet", "HEAD"),
        accepted=(0, 1),
    )
    if symbolic.returncode != 1:
        branch = symbolic.stdout.strip() or "unknown branch"
        raise ProtocolServerCheckError(
            f"pinned server checkout must be detached, found {branch}"
        )
    head_tree, expected, head_manifest_sha256 = _head_manifest(server, revision)
    index_manifest_sha256 = _index_manifest(server, expected)
    worktree_manifest_sha256 = _worktree_manifest(server_fd, expected)
    confirmed_metadata, confirmed_count, confirmed_mode = _metadata_manifest(server_fd)
    if (
        confirmed_metadata != git_metadata_sha256
        or confirmed_count != metadata_entry_count
        or confirmed_mode != checkout_mode
    ):
        raise ProtocolServerCheckError(
            "server Git metadata changed during protocol preflight"
        )
    _server_path_matches_fd(server, server_fd)
    snapshot_values = {
        "revision": revision,
        "head_tree": head_tree,
        "checkout_mode": checkout_mode,
        "git_metadata_sha256": git_metadata_sha256,
        "head_manifest_sha256": head_manifest_sha256,
        "index_manifest_sha256": index_manifest_sha256,
        "worktree_manifest_sha256": worktree_manifest_sha256,
        "tracked_entry_count": len(expected),
        "metadata_entry_count": metadata_entry_count,
    }
    facts: Dict[str, Any] = {
        "schema_version": 2,
        "revision": revision,
        "head": head,
        "head_tree": head_tree,
        "detached": True,
        "clean": True,
        "checkout_mode": checkout_mode,
        "git_metadata_mode": "isolated",
        "git_dir": ".git",
        "git_common_dir": ".git",
        "git_metadata_sha256": git_metadata_sha256,
        "head_manifest_sha256": head_manifest_sha256,
        "index_manifest_sha256": index_manifest_sha256,
        "worktree_manifest_sha256": worktree_manifest_sha256,
        "checkout_snapshot_sha256": hashlib.sha256(
            _canonical_json(snapshot_values)
        ).hexdigest(),
        "tracked_entry_count": len(expected),
        "metadata_entry_count": metadata_entry_count,
        "pre_exec_rechecks": 1,
        "facts_sha256": "",
    }
    facts["facts_sha256"] = _facts_hash(facts)
    return facts


def _checkout_facts_with_fd(client_root: Path) -> Tuple[Dict[str, Any], int]:
    client = client_root.resolve()
    server = client.parent / "murmur-server"
    server_fd = _open_directory(server, "task-local pinned server checkout")
    try:
        first = _capture_checkout(client, server, server_fd)
        second = _capture_checkout(client, server, server_fd)
        if first != second:
            raise ProtocolServerCheckError(
                "server checkout changed during pre-execution verification"
            )
        return second, server_fd
    except Exception:
        os.close(server_fd)
        raise


def checkout_facts(client_root: Path) -> Dict[str, Any]:
    """Return a twice-captured, exact pre-exec snapshot of the pinned checkout."""

    facts, server_fd = _checkout_facts_with_fd(client_root)
    os.close(server_fd)
    return facts


def facts_line(facts: Dict[str, Any]) -> str:
    if facts.get("facts_sha256") != _facts_hash(facts):
        raise ProtocolServerCheckError("server checkout facts hash is stale")
    return FACTS_PREFIX + _canonical_json(facts).decode("utf-8")


def main() -> int:
    expected_arguments = ("--", *TEST_ARGV)
    if tuple(sys.argv[1:]) != expected_arguments:
        print(
            "protocol-server preflight: canonical test argv changed",
            file=sys.stderr,
            flush=True,
        )
        return 2
    if os.environ.get("DATABASE_URL") != DATABASE_URL:
        print(
            "protocol-server preflight: canonical DATABASE_URL changed",
            file=sys.stderr,
            flush=True,
        )
        return 2
    if os.environ.get("CARGO_BUILD_JOBS") != "2":
        print(
            "protocol-server preflight: canonical CARGO_BUILD_JOBS changed",
            file=sys.stderr,
            flush=True,
        )
        return 2
    try:
        client = Path.cwd().resolve()
        facts, server_fd = _checkout_facts_with_fd(client)
        os.fchdir(server_fd)
        os.close(server_fd)
        print(facts_line(facts), flush=True)
        exec_environment = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith("GIT_")
        }
        exec_environment.update(
            {
                "GIT_CONFIG_GLOBAL": "/dev/null",
                "GIT_CONFIG_NOSYSTEM": "1",
                "GIT_NO_REPLACE_OBJECTS": "1",
                "GIT_OPTIONAL_LOCKS": "0",
                "GIT_TERMINAL_PROMPT": "0",
            }
        )
        os.execvpe(TEST_ARGV[0], list(TEST_ARGV), exec_environment)
    except (OSError, ProtocolServerCheckError) as exc:
        print(f"protocol-server preflight: {exc}", file=sys.stderr, flush=True)
        return 2
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
