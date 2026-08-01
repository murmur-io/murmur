#!/usr/bin/env python3
"""Shared runtime primitives for the verifier-only Murmur Harness.

This module owns deterministic filesystem, process, sandbox, check, and reviewer
execution helpers. It does not implement a writer, repair loop, or task lifecycle.
"""

from __future__ import annotations

import argparse
import copy
import ctypes
import ctypes.util
import datetime as dt
import fcntl
import fnmatch
import hashlib
import json
import os
import platform
import pwd
import re
import shlex
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path, PurePosixPath
from typing import Any, Dict, Iterable, List, Mapping, Optional, Sequence, Tuple

import resource_lane


HARNESS_ROOT = Path(__file__).resolve().parent
SCHEMAS_DIR = HARNESS_ROOT / "schemas"
PROMPTS_DIR = HARNESS_ROOT / "prompts"
CONFIG_PATH = HARNESS_ROOT / "config.json"
PROCESS_GUARDIAN_PATH = HARNESS_ROOT / "process_guardian.py"
REVIEWER_CWD = Path("/var/empty")
REVIEWER_TOOL_DENIAL = {
    "hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": "deny",
        "permissionDecisionReason": (
            "Harness reviewers have no local tools. Review the supplied "
            "immutable diff and evidence, or return a schema-allowlisted "
            "typed probe request."
        ),
    }
}
CODEX_HOOK_TRUST_WARNING = (
    "`--dangerously-bypass-hook-trust` is enabled. Enabled hooks may run "
    "without review for this invocation."
)
TASK_ID_RE = re.compile(r"^[a-z0-9][a-z0-9._-]{1,63}$")
SHA1_RE = re.compile(r"^[0-9a-f]{40}$")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
REAL_MODEL_VENDORS = {"codex", "claude"}
MANAGED_CLEANUP_SIGNALS = frozenset((signal.SIGINT, signal.SIGTERM, signal.SIGHUP))
OUTER_SANDBOX_ENV = "MURMUR_HARNESS_OUTER_SANDBOX"
INHERITED_SANDBOX_META_CHECKS = frozenset(
    (
        "scripts/agent-harness selftest --ci",
        "python3 .agents/harness/v2_selftest.py",
        "bash .codex/hooks/selftest.sh",
    )
)


class HarnessError(RuntimeError):
    """An expected harness failure with a stable CLI exit code."""

    def __init__(self, message: str, exit_code: int = 2) -> None:
        super().__init__(message)
        self.exit_code = exit_code


class ManagedProcessTimeout(HarnessError):
    """Typed model-process wall timeout with runner-owned guardian evidence."""

    def __init__(
        self,
        label: str,
        timeout_seconds: float,
        process_result: Mapping[str, Any],
    ) -> None:
        self.label = label
        self.timeout_seconds = float(timeout_seconds)
        self.process_result = dict(process_result)
        self.timed_out = True
        log_path = str(process_result.get("log") or "unknown")
        guardian_path = str(process_result.get("guardian_path") or "unknown")
        super().__init__(
            f"{label} wall timeout after {self.timeout_seconds:g}s "
            f"(exit_code={process_result.get('exit_code')}, "
            f"duration_ms={process_result.get('duration_ms')}, "
            f"log={log_path}, guardian={guardian_path})",
            exit_code=124,
        )


class HarnessCancellation(BaseException):
    """Catchable SIGTERM/SIGHUP so managed child groups are drained first."""

    def __init__(self, signum: int) -> None:
        super().__init__(signum)
        self.signum = signum


def reviewer_tool_guard_command() -> str:
    """Return a deny-all hook whose executable state is entirely in argv."""
    denial_json = json.dumps(
        REVIEWER_TOOL_DENIAL,
        sort_keys=True,
        separators=(",", ":"),
    )
    return "/usr/bin/printf '%s\\n' " + shlex.quote(denial_json)


def reviewer_execution_cwd() -> Path:
    """Resolve a root-owned, non-writable cwd with no project configuration."""
    try:
        resolved = REVIEWER_CWD.resolve(strict=True)
    except (FileNotFoundError, OSError) as exc:
        raise HarnessError(f"reviewer cwd is unavailable: {REVIEWER_CWD}") from exc
    for component in (resolved, *resolved.parents):
        info = component.stat()
        if not stat.S_ISDIR(info.st_mode):
            raise HarnessError(f"reviewer cwd ancestor is not a directory: {component}")
        if info.st_uid != 0 or info.st_mode & (stat.S_IWGRP | stat.S_IWOTH):
            raise HarnessError(
                f"reviewer cwd ancestor is mutable by the invoking user: {component}"
            )
    return resolved


def reviewer_model_environment(
    environment: Mapping[str, str],
    *,
    vendor: str,
    cwd: Path,
) -> Dict[str, str]:
    """Remove mutable shell startup state from a verifier-only model process."""
    isolated = dict(environment)
    isolated["SHELL"] = "/bin/sh"
    if vendor == "codex":
        original_home = isolated.get("HOME")
        if "CODEX_HOME" not in isolated:
            if not original_home:
                raise HarnessError("Codex reviewer has no HOME or CODEX_HOME for auth")
            isolated["CODEX_HOME"] = str(Path(original_home) / ".codex")
        isolated["HOME"] = str(cwd)
    return isolated


def reviewer_tool_activity(log_path: Path, vendor: str) -> List[str]:
    """Return model tool events that make a verifier-only review inadmissible."""
    activity: List[str] = []
    claude_init_seen = False
    with log_path.open("r", encoding="utf-8", errors="replace") as handle:
        for line_number, line in enumerate(handle, start=1):
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            if not isinstance(event, dict):
                continue
            if vendor == "codex":
                event_type = event.get("type")
                if isinstance(event_type, str) and event_type.startswith("item."):
                    item = event.get("item")
                    item_type = item.get("type") if isinstance(item, dict) else None
                    if (
                        event_type == "item.completed"
                        and item_type == "error"
                        and item.get("message") == CODEX_HOOK_TRUST_WARNING
                    ):
                        continue
                    # A verifier-only Codex process may stream prose/reasoning, but
                    # every executable, hosted, file-mutating, or future unknown item
                    # is inadmissible. The closed allowlist makes new CLI tool shapes
                    # fail closed instead of silently bypassing this audit.
                    if item_type not in {"agent_message", "reasoning"}:
                        activity.append(
                            f"line {line_number}: {item_type or 'malformed-item'}"
                        )
                elif isinstance(event_type, str) and (
                    "tool" in event_type
                    or "command" in event_type
                    or "file_change" in event_type
                ):
                    activity.append(f"line {line_number}: {event_type}")
            elif vendor == "claude" and event.get("type") == "system":
                if event.get("subtype") != "init":
                    continue
                claude_init_seen = True
                tools = event.get("tools")
                mcp_servers = event.get("mcp_servers")
                if tools != ["StructuredOutput"]:
                    activity.append(
                        f"line {line_number}: unexpected-tools:{tools!r}"
                    )
                if mcp_servers != []:
                    activity.append(
                        f"line {line_number}: unexpected-mcp:{mcp_servers!r}"
                    )
            elif vendor == "claude" and event.get("type") == "assistant":
                message = event.get("message")
                content = message.get("content") if isinstance(message, dict) else None
                if not isinstance(content, list):
                    continue
                for block in content:
                    if isinstance(block, dict) and block.get("type") == "tool_use":
                        # Claude Code implements --json-schema by exposing one
                        # non-executing response channel named StructuredOutput.
                        # It is the only admissible tool-shaped block; Read, Bash,
                        # WebSearch, MCP, Task, and every future name still fail.
                        if block.get("name") == "StructuredOutput":
                            continue
                        activity.append(
                            f"line {line_number}: tool_use:{block.get('name', 'unknown')}"
                        )
    if vendor == "claude" and not claude_init_seen:
        activity.append("missing Claude init telemetry")
    return activity


def assert_reviewer_used_no_tools(log_path: Path, vendor: str) -> None:
    activity = reviewer_tool_activity(log_path, vendor)
    if activity:
        raise HarnessError(
            "reviewer-only model used a local or hosted tool; evidence is "
            "inadmissible: " + ", ".join(activity[:8])
        )


class TaskRunLock:
    """An atomically published task lock whose live owner holds an OS flock."""

    def __init__(self, path: Path, handle: Any) -> None:
        self.path = path
        self.handle = handle


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def canonical_json(value: Any) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _new_execution_id() -> str:
    return str(uuid.uuid4())


def _execution_artifact_path(base_path: Path, execution_id: str) -> Path:
    """Insert one canonical UUID before a base artifact's final suffix."""

    try:
        parsed = uuid.UUID(execution_id)
    except (AttributeError, TypeError, ValueError) as exc:
        raise HarnessError(f"invalid managed-process execution id: {execution_id!r}") from exc
    if str(parsed) != execution_id:
        raise HarnessError(f"managed-process execution id is not canonical: {execution_id!r}")
    return base_path.with_name(f"{base_path.stem}-{execution_id}{base_path.suffix}")


def load_json(path: Path) -> Any:
    try:
        with path.open("r", encoding="utf-8") as handle:
            return json.load(handle)
    except FileNotFoundError as exc:
        raise HarnessError(f"missing required file: {path}") from exc
    except json.JSONDecodeError as exc:
        raise HarnessError(f"invalid JSON in {path}: {exc}") from exc


def atomic_write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=str(path.parent))
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as handle:
            json.dump(value, handle, indent=2, sort_keys=True, ensure_ascii=False)
            handle.write("\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp_name, path)
    finally:
        try:
            os.unlink(tmp_name)
        except FileNotFoundError:
            pass


def atomic_write_bytes(path: Path, value: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=str(path.parent))
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(value)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp_name, path)
    finally:
        try:
            os.unlink(tmp_name)
        except FileNotFoundError:
            pass


def atomic_create_bytes(path: Path, value: bytes) -> None:
    """Atomically create an immutable artifact, failing instead of replacing."""

    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(prefix=f".{path.name}.", dir=str(path.parent))
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(value)
            handle.flush()
            os.fsync(handle.fileno())
        try:
            os.link(tmp_name, path)
        except FileExistsError as exc:
            raise HarnessError(f"refusing to overwrite prior execution artifact: {path}") from exc
        directory = os.open(path.parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        try:
            os.unlink(tmp_name)
        except FileNotFoundError:
            pass


def atomic_create_json(path: Path, value: Any) -> None:
    atomic_create_bytes(
        path,
        (
            json.dumps(value, indent=2, sort_keys=True, ensure_ascii=False) + "\n"
        ).encode("utf-8"),
    )


def append_jsonl(path: Path, value: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    encoded = json.dumps(value, sort_keys=True, ensure_ascii=False) + "\n"
    with path.open("a", encoding="utf-8") as handle:
        handle.write(encoded)
        handle.flush()
        os.fsync(handle.fileno())


def run_capture(
    argv: Sequence[str],
    cwd: Optional[Path] = None,
    *,
    check: bool = True,
    env: Optional[Mapping[str, str]] = None,
) -> subprocess.CompletedProcess:
    completed = subprocess.run(
        list(argv),
        cwd=str(cwd) if cwd else None,
        env=dict(env) if env else None,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=False,
    )
    if check and completed.returncode != 0:
        rendered = " ".join(argv)
        detail = (completed.stderr or completed.stdout).strip()
        raise HarnessError(f"command failed ({rendered}): {detail}")
    return completed


def git(cwd: Path, *args: str, check: bool = True) -> str:
    return run_capture(["git", *args], cwd, check=check).stdout.strip()


def git_bytes(cwd: Path, *args: str, check: bool = True) -> bytes:
    completed = subprocess.run(
        ["git", *args],
        cwd=str(cwd),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if check and completed.returncode != 0:
        detail = completed.stderr.decode("utf-8", "replace").strip()
        raise HarnessError(f"git {' '.join(args)} failed: {detail}")
    return completed.stdout


def repo_context(cwd: Path) -> Tuple[Path, Path]:
    try:
        top = Path(git(cwd, "rev-parse", "--show-toplevel")).resolve()
    except HarnessError as exc:
        raise HarnessError(f"not inside a Git repository: {cwd}") from exc

    listing = git(cwd, "worktree", "list", "--porcelain").splitlines()
    primary_line = next((line for line in listing if line.startswith("worktree ")), None)
    primary = Path(primary_line.split(" ", 1)[1]).resolve() if primary_line else top
    common_raw = git(cwd, "rev-parse", "--git-common-dir")
    common = Path(common_raw)
    if not common.is_absolute():
        common = (top / common).resolve()
    return primary, common


def load_config() -> Dict[str, Any]:
    config = load_json(CONFIG_PATH)
    if not isinstance(config, dict) or config.get("schema_version") != 2:
        raise HarnessError(f"unsupported harness config: {CONFIG_PATH}")
    return config


def load_schema(name: str) -> Dict[str, Any]:
    schema = load_json(SCHEMAS_DIR / f"{name}.schema.json")
    if not isinstance(schema, dict):
        raise HarnessError(f"schema is not an object: {name}")
    return schema


def schema_for_model_cli(schema: Mapping[str, Any], vendor: str) -> Dict[str, Any]:
    """Return the schema dialect accepted by a vendor CLI.

    Claude Code 2.1.206 validates ``--json-schema`` with a validator that does
    not bundle the draft-2020-12 meta-schema. The document keywords used by
    this harness are still supported; only the metadata declaration must be
    omitted at the CLI boundary. The canonical checked-in schema remains
    draft-2020-12 and is validated independently by the runner.
    """

    result = dict(schema)
    if vendor == "claude":
        result.pop("$schema", None)
        result.pop("$id", None)
    return result


def _schema_type_matches(value: Any, expected: str) -> bool:
    if expected == "object":
        return isinstance(value, dict)
    if expected == "array":
        return isinstance(value, list)
    if expected == "string":
        return isinstance(value, str)
    if expected == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    if expected == "number":
        return isinstance(value, (int, float)) and not isinstance(value, bool)
    if expected == "boolean":
        return isinstance(value, bool)
    if expected == "null":
        return value is None
    return True


def validate_schema(value: Any, schema: Dict[str, Any], *, label: str = "document") -> None:
    """Validate the JSON-Schema subset used by the checked-in contracts."""

    def visit(item: Any, node: Dict[str, Any], root: Dict[str, Any], path: str) -> None:
        if "$ref" in node:
            reference = node["$ref"]
            if not isinstance(reference, str) or not reference.startswith("#/"):
                raise HarnessError(f"{label}: unsupported schema reference {reference!r}")
            target: Any = root
            for part in reference[2:].split("/"):
                target = target[part.replace("~1", "/").replace("~0", "~")]
            visit(item, target, root, path)
            return

        expected = node.get("type")
        if expected and not _schema_type_matches(item, expected):
            raise HarnessError(f"{label}: {path} must be {expected}")
        if "const" in node and item != node["const"]:
            raise HarnessError(f"{label}: {path} must equal {node['const']!r}")
        if "enum" in node and item not in node["enum"]:
            raise HarnessError(f"{label}: {path} has unsupported value {item!r}")

        if isinstance(item, dict):
            required = node.get("required", [])
            missing = [key for key in required if key not in item]
            if missing:
                raise HarnessError(f"{label}: {path} missing fields: {', '.join(missing)}")
            properties = node.get("properties", {})
            if node.get("additionalProperties") is False:
                extras = sorted(set(item) - set(properties))
                if extras:
                    raise HarnessError(f"{label}: {path} has extra fields: {', '.join(extras)}")
            for key, child in item.items():
                if key in properties:
                    visit(child, properties[key], root, f"{path}.{key}")
                elif isinstance(node.get("additionalProperties"), dict):
                    visit(child, node["additionalProperties"], root, f"{path}.{key}")

        if isinstance(item, list):
            if "minItems" in node and len(item) < int(node["minItems"]):
                raise HarnessError(f"{label}: {path} has too few items")
            if "maxItems" in node and len(item) > int(node["maxItems"]):
                raise HarnessError(f"{label}: {path} has too many items")
            if node.get("uniqueItems"):
                encoded = [canonical_json(child) for child in item]
                if len(encoded) != len(set(encoded)):
                    raise HarnessError(f"{label}: {path} must contain unique items")
            child_schema = node.get("items")
            if isinstance(child_schema, dict):
                for index, child in enumerate(item):
                    visit(child, child_schema, root, f"{path}[{index}]")

        if isinstance(item, str):
            if "minLength" in node and len(item) < int(node["minLength"]):
                raise HarnessError(f"{label}: {path} is too short")
            if "maxLength" in node and len(item) > int(node["maxLength"]):
                raise HarnessError(f"{label}: {path} is too long")
            if "pattern" in node and not re.search(str(node["pattern"]), item):
                raise HarnessError(f"{label}: {path} does not match required pattern")
            if node.get("format") == "date-time":
                try:
                    dt.datetime.fromisoformat(item.replace("Z", "+00:00"))
                except ValueError as exc:
                    raise HarnessError(f"{label}: {path} is not an ISO date-time") from exc

        if isinstance(item, (int, float)) and not isinstance(item, bool):
            if "minimum" in node and item < node["minimum"]:
                raise HarnessError(f"{label}: {path} is below minimum")
            if "maximum" in node and item > node["maximum"]:
                raise HarnessError(f"{label}: {path} is above maximum")

    visit(value, schema, schema, "$")


def normalize_owned_path(raw: str) -> str:
    if not raw or "\x00" in raw:
        raise HarnessError("owned paths must be non-empty relative paths")
    raw = raw.replace("\\", "/")
    path = PurePosixPath(raw)
    if path.is_absolute() or any(part in {"", ".", ".."} for part in path.parts):
        raise HarnessError(f"unsafe owned path: {raw!r}")
    normalized = path.as_posix().rstrip("/")
    if normalized in {"", ".", "./"}:
        raise HarnessError(f"owned path may not be the repository root: {raw!r}")
    if normalized == ".git" or normalized.startswith(".git/"):
        raise HarnessError("the Git metadata directory can never be owned by a task")
    return normalized


def path_overlaps(left: str, right: str) -> bool:
    return left == right or left.startswith(right.rstrip("/") + "/") or right.startswith(left.rstrip("/") + "/")


def path_is_owned(path: str, owned_paths: Iterable[str]) -> bool:
    return any(path == owned or path.startswith(owned.rstrip("/") + "/") for owned in owned_paths)


def path_has_symlink_component(worktree: Path, relative: str) -> bool:
    current = worktree
    for part in PurePosixPath(relative).parts:
        current = current / part
        if current.is_symlink():
            return True
    return False


def unsafe_changed_nodes(worktree: Path, paths: Sequence[str]) -> List[str]:
    """Return present changed paths that are unsafe to stage or execute.

    A path-only sandbox cannot contain writes through a hard link: the name is
    inside the worktree while the inode may also be named outside it.  Reject
    every present changed node unless it is a single-link regular file, and
    reject symlinks in every path component.  Missing paths are ordinary Git
    deletions and remain valid.
    """

    unsafe: List[str] = []
    for relative in paths:
        if path_has_symlink_component(worktree, relative):
            unsafe.append(relative)
            continue
        path = worktree / relative
        try:
            metadata = path.lstat()
        except FileNotFoundError:
            continue
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
            unsafe.append(relative)
    return unsafe


def _glob_pattern_to_regex(pattern: str) -> str:
    """Translate a repo path glob into an anchored, path-aware regex.

    gitignore-style `**` spans path separators (zero or more whole segments); a
    single `*`/`?` never crosses `/`. This replaces a former static-prefix
    fallback that collapsed e.g. ``src-tauri/**/*.swift`` to the bare directory
    ``src-tauri`` and then matched EVERY file under it (the bug that spuriously
    flagged a pure ``commands/attachments.rs`` change as ``runtime`` and pulled
    in the env-fragile tauri-boot gate).
    """

    out: List[str] = ["^"]
    index = 0
    length = len(pattern)
    while index < length:
        if pattern.startswith("**/", index):
            out.append("(?:[^/]+/)*")
            index += 3
        elif pattern.startswith("**", index):
            out.append(".*")
            index += 2
        elif pattern[index] == "*":
            out.append("[^/]*")
            index += 1
        elif pattern[index] == "?":
            out.append("[^/]")
            index += 1
        else:
            out.append(re.escape(pattern[index]))
            index += 1
    out.append("$")
    return "".join(out)


def instruction_paths(repo_root: Path) -> List[Tuple[str, Path]]:
    paths: Dict[str, Path] = {}

    def include(label: str, path: Path) -> None:
        if path.is_file():
            paths[label] = path

    for relative in ("AGENTS.md", "CLAUDE.md", ".codex/config.toml", ".codex/requirements.toml", ".claude/settings.json"):
        include(relative, repo_root / relative)
    for directory in (
        ".codex/rules",
        ".claude/rules",
        ".codex/agents",
        ".claude/agents",
        # `.claude/learnings` is the canonical tree and is executable reviewer
        # input: `verifier.review_learnings_section` binds its curated
        # `## Recurring patterns` into every review prompt. It listed only the
        # generated `.codex` mirror before, so editing the canonical file that
        # actually reaches a reviewer left this fingerprint unmoved. The mirror
        # stays listed so a parity drift between the two is still tracked.
        ".claude/learnings",
        ".codex/learnings",
        ".codex/hooks",
        ".claude/hooks",
    ):
        root = repo_root / directory
        if root.is_dir():
            for path in sorted(path for path in root.rglob("*") if path.is_file()):
                include(path.relative_to(repo_root).as_posix(), path)

    # Harness files can be under active development and therefore absent from
    # an older task base. Fingerprint the executable control plane actually in
    # use, not a hypothetical copy from the base commit.
    source_repo = HARNESS_ROOT.parent.parent
    harness_files = [CONFIG_PATH, Path(__file__).resolve()]
    for name in (
        "cli.py",
        "verifier.py",
        "hook_guard.py",
        "config_audit.py",
        "resource_policy.py",
        "remote-policy.json",
    ):
        candidate = HARNESS_ROOT / name
        if candidate.is_file():
            harness_files.append(candidate)
    harness_files.extend(sorted(path for path in PROMPTS_DIR.rglob("*") if path.is_file()))
    harness_files.extend(sorted(path for path in SCHEMAS_DIR.rglob("*") if path.is_file()))
    checks_dir = HARNESS_ROOT / "checks"
    if checks_dir.is_dir():
        harness_files.extend(sorted(path for path in checks_dir.rglob("*") if path.is_file()))
    for name in (
        "agent-harness",
        "agent-resource-run",
        "agent-config-audit",
        "agent-remote-audit",
        "agent-sync-learnings",
        "verify-harness-attestation",
        "ci.sh",
    ):
        wrapper = source_repo / "scripts" / name
        if wrapper.is_file():
            harness_files.append(wrapper)
    # The remote-policy audit implementation lives directly under scripts/,
    # not .agents/harness/, so fingerprint it explicitly with the control
    # plane.
    remote_audit_impl = source_repo / "scripts" / "agent-remote-audit.py"
    if remote_audit_impl.is_file():
        harness_files.append(remote_audit_impl)
    for path in harness_files:
        try:
            label = path.relative_to(source_repo).as_posix()
        except ValueError:
            label = str(path)
        include(label, path)
    return sorted(paths.items())


def instructions_hash(repo_root: Path) -> str:
    digest = hashlib.sha256()
    for label, path in instruction_paths(repo_root):
        digest.update(label.encode("utf-8"))
        digest.update(b"\x00")
        digest.update(path.read_bytes())
        digest.update(b"\x00")
    return digest.hexdigest()


def has_murmur_server_path_dependency(repo_root: Path) -> bool:
    cargo_files = [repo_root / "Cargo.toml", repo_root / "src-tauri" / "Cargo.toml"]
    crates = repo_root / "crates"
    if crates.is_dir():
        cargo_files.extend(sorted(crates.glob("*/Cargo.toml")))
    for cargo_file in cargo_files:
        try:
            text = cargo_file.read_text(encoding="utf-8")
        except (OSError, UnicodeError):
            continue
        if re.search(r"path\s*=\s*[\"'][^\"']*murmur-server/", text):
            return True
    return False


def read_prompt(name: str) -> str:
    path = PROMPTS_DIR / f"{name}.md"
    try:
        return path.read_text(encoding="utf-8")
    except FileNotFoundError as exc:
        raise HarnessError(f"missing prompt template: {path}") from exc


def changed_paths(worktree: Path) -> List[str]:
    tracked = git_bytes(worktree, "diff", "--name-only", "-z", "HEAD")
    untracked = git_bytes(worktree, "ls-files", "--others", "--exclude-standard", "-z")
    values = set()
    for encoded in (tracked, untracked):
        for item in encoded.split(b"\x00"):
            if item:
                values.add(item.decode("utf-8", "surrogateescape"))
    if "node_modules" in values and managed_node_modules_link(worktree):
        values.remove("node_modules")
    return sorted(values)


def managed_node_modules_link(worktree: Path) -> bool:
    link = worktree / "node_modules"
    if not link.is_symlink():
        return False
    primary, _ = repo_context(worktree)
    try:
        return link.resolve(strict=True) == (primary / "node_modules").resolve(strict=True)
    except (FileNotFoundError, OSError):
        return False


def untracked_paths(worktree: Path) -> List[str]:
    encoded = git_bytes(worktree, "ls-files", "--others", "--exclude-standard", "-z")
    paths = [item.decode("utf-8", "surrogateescape") for item in encoded.split(b"\x00") if item]
    return [path for path in paths if not (path == "node_modules" and managed_node_modules_link(worktree))]


def staged_diff(worktree: Path) -> bytes:
    return git_bytes(
        worktree,
        "diff",
        "--cached",
        "--binary",
        "--full-index",
        "--no-ext-diff",
        "--no-renames",
        "--",
    )


def workspace_fingerprint(worktree: Path) -> str:
    """Hash tracked and untracked task-visible bytes without modifying Git."""

    digest = hashlib.sha256()
    digest.update(git_bytes(worktree, "diff", "--binary", "--full-index", "HEAD", "--"))
    for relative in sorted(untracked_paths(worktree)):
        encoded = relative.encode("utf-8", "surrogateescape")
        digest.update(b"\x00path\x00")
        digest.update(encoded)
        path = worktree / relative
        if path.is_file():
            digest.update(b"\x00bytes\x00")
            with path.open("rb") as handle:
                for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                    digest.update(chunk)
    return digest.hexdigest()


# A model can accidentally copy the renderer's truncation banner into a source file when it
# transfers a large tool result instead of the underlying bytes.  That failure mode is especially
# destructive: the banner is valid plain text while the omitted middle of the file is silently
# lost.  Reject the exact renderer marker at the trusted scope boundary before checks/review can
# green-wash the resulting partial file.
TOOL_OUTPUT_CONTAMINATION_MARKERS = (
    # Split the runner-owned sentinel so a newly created runtime module cannot
    # flag its own source as copied renderer output.
    b"Warning: truncated output " + b"(original token count:",
)


def tool_output_contaminated_paths(worktree: Path, paths: Sequence[str]) -> List[str]:
    contaminated: List[str] = []
    for relative in paths:
        path = worktree / relative
        if not path.is_file():
            continue
        try:
            tracked = run_capture(
                ["git", "ls-files", "--error-unmatch", "--", relative],
                worktree,
                check=False,
            ).returncode == 0
            if tracked:
                patch = git_bytes(
                    worktree,
                    "diff",
                    "--unified=0",
                    "--no-ext-diff",
                    "HEAD",
                    "--",
                    relative,
                )
                inspected = b"\n".join(
                    line[1:]
                    for line in patch.splitlines()
                    if line.startswith(b"+") and not line.startswith(b"+++")
                )
            else:
                inspected = path.read_bytes()
            if any(marker in inspected for marker in TOOL_OUTPUT_CONTAMINATION_MARKERS):
                contaminated.append(relative)
        except OSError as exc:
            raise HarnessError(f"could not inspect changed file {relative}: {exc}") from exc
    return sorted(set(contaminated))


def _owned_process_group_id(process: subprocess.Popen) -> int:
    """Return the exact detached group owned by one managed ``Popen``.

    Every caller creates the process with ``start_new_session=True``, so the
    child's pid is also its process-group id. Refuse broad/caller groups before
    signalling: a cleanup bug must never reach the operator's shell or app.
    """

    pgid = int(process.pid)
    if pgid <= 1 or pgid == os.getpgrp():
        raise HarnessError(f"refusing to signal unsafe managed process group {pgid}")
    return pgid


def _process_group_alive(pgid: int) -> bool:
    try:
        os.killpg(pgid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        # A group which still exists but became unsignalable is not safely
        # cleaned up. Keep treating it as alive so the caller fails closed.
        return True
    return True


def _wait_for_group_exit(process: subprocess.Popen, pgid: int, timeout_seconds: float) -> bool:
    deadline = time.monotonic() + timeout_seconds
    while _process_group_alive(pgid) and time.monotonic() < deadline:
        process.poll()  # reap the leader without assuming its exit ended the group
        time.sleep(0.05)
    process.poll()
    return not _process_group_alive(pgid)


def _terminate_process(process: subprocess.Popen) -> None:
    """Boundedly terminate the complete owned group, including grandchildren.

    Cancellation is deferred across the full TERM -> KILL -> reap sequence. Without
    this mask, a second signal (or a first signal after a nominal leader exit) can
    interrupt the cleanup's own grace period and strand a TERM-ignoring descendant.
    Pending signals are delivered as soon as the previous mask is restored, after
    the owned group is gone.
    """

    previous_mask = signal.pthread_sigmask(signal.SIG_BLOCK, MANAGED_CLEANUP_SIGNALS)
    try:
        pgid = _owned_process_group_id(process)
        if _process_group_alive(pgid):
            try:
                os.killpg(pgid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        if not _wait_for_group_exit(process, pgid, 3.0):
            try:
                os.killpg(pgid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            if not _wait_for_group_exit(process, pgid, 3.0):
                raise HarnessError(f"managed process group {pgid} survived SIGKILL")
        try:
            process.wait(timeout=0.5)
        except subprocess.TimeoutExpired as exc:
            raise HarnessError(f"managed process leader {process.pid} could not be reaped") from exc
    finally:
        signal.pthread_sigmask(signal.SIG_SETMASK, previous_mask)


def command_uses_playwright(command: str) -> bool:
    """Identify commands that need a task-private UI-server port."""

    return any(
        marker in command
        for marker in (
            "playwright",
            "test:e2e",
            "scripts/ci.sh",
        )
    )


def command_uses_cargo_lane(command: str) -> bool:
    """Identify direct and wrapped checks which can compile the heavy Rust/ML tree."""

    normalized = command.lower()
    if re.search(
        r"(?<![a-z0-9_.-])(?:cargo(?:-[a-z0-9_-]+)?|rustc)(?![a-z0-9_.-])",
        normalized,
    ):
        return True
    return any(
        marker in normalized
        for marker in (
            "scripts/ci.sh",
            ".agents/harness/checks/perf-contracts.sh",
            "scripts/harness-runtime-smoke",
            "npm run dev",
            "npm run tauri",
            "npx tauri",
            "scripts/e2e-core.sh",
            "scripts/e2e-mix.sh",
        )
    )


def command_needs_loopback(command: str) -> bool:
    """Return whether a deterministic check may use local TCP only."""

    normalized = command.lower()
    return command_uses_playwright(command) or any(
        marker in normalized
        for marker in (
            "scripts/harness-runtime-smoke",
            "npm run dev",
            "npx tauri",
            "npm run tauri",
            # The existing Rust suite owns ephemeral loopback listeners for its
            # HTTP timeout/retry tests. Full ci.sh already had loopback via its
            # Playwright leg; the focused rust-lib check needs the same scope.
            "cargo test",
            "cargo nextest",
            # The harness meta-selftest reserves task-private loopback ports to
            # prove concurrent Playwright tasks cannot collide. It never opens
            # external network access.
            "scripts/agent-harness selftest --ci",
        )
    )


def _real_user_home() -> Path:
    try:
        return Path(pwd.getpwuid(os.getuid()).pw_dir).resolve()
    except (KeyError, OSError) as exc:
        raise HarnessError("cannot resolve the real user home for the check sandbox") from exc


def _safe_tool_path(real_home: Path) -> str:
    candidates = [
        Path("/opt/homebrew/bin"),
        Path("/opt/homebrew/sbin"),
        Path("/usr/local/bin"),
        Path("/usr/local/sbin"),
        Path("/System/Cryptexes/App/usr/bin"),
        Path("/usr/bin"),
        Path("/bin"),
        Path("/usr/sbin"),
        Path("/sbin"),
        Path("/Library/Apple/usr/bin"),
        real_home / ".cargo" / "bin",
        real_home / ".bun" / "bin",
        real_home / ".local" / "bin",
    ]
    return os.pathsep.join(str(path) for path in candidates if path.is_dir())


def _check_runtime_paths(task_dir: Path) -> Dict[str, Path]:
    root = task_dir / "runtime" / "checks"
    paths = {
        "root": root,
        "home": root / "home",
        "tmp": root / "tmp",
        "xdg_cache": root / "xdg-cache",
        "npm_cache": root / "npm-cache",
        "clang_cache": root / "clang-cache",
        "profiles": root / "profiles",
        "cargo_home": root / "cargo-home",
        "cargo_target": root / "cargo-target",
        "runtime_smoke": root / "runtime-smoke",
    }
    for path in paths.values():
        path.mkdir(parents=True, exist_ok=True)
    return paths


def _prepare_private_cargo_home(runtime: Mapping[str, Path], real_home: Path) -> None:
    """Expose immutable dependency inputs through a task-private Cargo home."""

    cargo_home = runtime["cargo_home"]
    shared_home = (real_home / ".cargo").resolve()
    for name in ("registry", "git", "advisory-db"):
        target = shared_home / name
        link = cargo_home / name
        if not target.exists():
            if os.path.lexists(link):
                raise HarnessError(f"private Cargo cache link has no source: {link}")
            continue
        expected = target.resolve(strict=True)
        if os.path.lexists(link):
            if not link.is_symlink():
                raise HarnessError(f"private Cargo cache entry is not a runner-owned symlink: {link}")
            try:
                actual = link.resolve(strict=True)
            except (FileNotFoundError, OSError) as exc:
                raise HarnessError(f"private Cargo cache link is invalid: {link}") from exc
            if actual != expected:
                raise HarnessError(f"private Cargo cache link changed: {link}")
            continue
        link.symlink_to(expected, target_is_directory=True)


def command_needs_sherpa_archive(command: str, worktree: Optional[Path] = None) -> bool:
    """Return whether a check can compile the client-side sherpa-onnx dependency."""

    command_matches = any(
        marker in command
        for marker in (
            "src-tauri",
            "scripts/ci.sh",
            "scripts/e2e-core.sh",
            "scripts/e2e-mix.sh",
            "scripts/harness-runtime-smoke",
            # The performance-contract script compiles Rust without exposing
            # `src-tauri` in its command string. Match that capability
            # explicitly; other runner-owned checks (for example npm-lock)
            # must not inherit the heavyweight Sherpa input.
            ".agents/harness/checks/perf-contracts.sh",
            "npm run dev",
            "npm run tauri",
            "npx tauri",
        )
    )
    if not command_matches or worktree is None:
        return command_matches
    manifest = worktree / "src-tauri" / "Cargo.toml"
    try:
        return bool(re.search(r'(?m)^\s*sherpa-onnx\s*=', manifest.read_text(encoding="utf-8")))
    except (FileNotFoundError, OSError, UnicodeError):
        return False


def verified_sherpa_archive(
    worktree: Path,
    *,
    task_dir: Optional[Path] = None,
) -> Tuple[Path, str]:
    """Resolve the immutable host cache input and verify it before sandbox use."""

    config = load_config()
    raw = config.get("shared_artifacts", {}).get("sherpa_onnx", {})
    if not isinstance(raw, dict):
        raise HarnessError("shared_artifacts.sherpa_onnx must be configured")
    machine = platform.machine().lower()
    architecture = {"aarch64": "arm64", "arm64": "arm64", "x86_64": "x86_64"}.get(machine)
    if architecture is None:
        raise HarnessError(f"no pinned sherpa-onnx archive for host architecture {machine!r}")
    archive = raw.get("archives", {}).get(architecture, {})
    directory_raw = raw.get("directory")
    filename = archive.get("filename") if isinstance(archive, dict) else None
    expected_sha = archive.get("sha256") if isinstance(archive, dict) else None
    if (
        not isinstance(directory_raw, str)
        or not directory_raw
        or not isinstance(filename, str)
        or not filename
        or not SHA256_RE.fullmatch(str(expected_sha))
    ):
        raise HarnessError(f"invalid pinned sherpa-onnx artifact config for {architecture}")
    relative_directory = Path(directory_raw)
    if relative_directory.is_absolute() or ".." in relative_directory.parts:
        raise HarnessError("shared_artifacts.sherpa_onnx.directory must be repository-relative")

    def verified_existing(candidate: Path) -> Optional[str]:
        if not candidate.exists() and not candidate.is_symlink():
            return None
        if not candidate.is_file() or candidate.is_symlink():
            raise HarnessError(f"pinned sherpa-onnx archive is not a regular file: {candidate}")
        actual = sha256_file(candidate)
        if actual != expected_sha:
            raise HarnessError(
                f"pinned sherpa-onnx archive checksum mismatch: expected {expected_sha}, "
                f"found {actual} at {candidate}"
            )
        return actual

    primary, _ = repo_context(worktree)
    legacy_directory = (primary / relative_directory).resolve(strict=False)
    legacy_candidate = legacy_directory / filename
    if task_dir is None:
        actual_sha = verified_existing(legacy_candidate)
        if actual_sha is None:
            raise HarnessError(
                "pinned sherpa-onnx archive is unavailable; seed the checkout cache "
                f"before running an offline client Rust check: {legacy_candidate}"
            )
        return legacy_directory, actual_sha

    resource_root = shared_resource_root_for_task(task_dir)
    resource_root.mkdir(parents=True, exist_ok=True)
    if resource_root.is_symlink() or not resource_root.is_dir():
        raise HarnessError(f"workspace resource root is not a real directory: {resource_root}")
    shared_directory = resource_root
    for part in relative_directory.parts:
        shared_directory = shared_directory / part
        if shared_directory.is_symlink():
            raise HarnessError(
                f"workspace Sherpa cache directory is symlinked: {shared_directory}"
            )
        shared_directory.mkdir(exist_ok=True)
        if not shared_directory.is_dir():
            raise HarnessError(
                f"workspace Sherpa cache path is not a directory: {shared_directory}"
            )
    shared_candidate = shared_directory / filename
    actual_sha = verified_existing(shared_candidate)
    if actual_sha is not None:
        return shared_directory, actual_sha

    # Promote one already-downloaded, checksum-pinned legacy archive into the
    # workspace-wide runner cache. Never download here and never copy from the
    # isolated verification snapshot itself.
    workspace = resource_root.parent.parent
    task = load_json(task_dir / "task.json")
    task_worktree = Path(str(task.get("worktree_path", "")))
    source_directories: List[Path] = []
    if task_worktree.name:
        source_directories.append(
            (workspace / task_worktree.name / relative_directory).resolve(strict=False)
        )
    source_directories.append(
        (workspace / "meetnotes" / relative_directory).resolve(strict=False)
    )
    source_candidate: Optional[Path] = None
    seen_sources: set[Path] = set()
    for directory in source_directories:
        if directory == shared_directory or directory in seen_sources:
            continue
        seen_sources.add(directory)
        candidate = directory / filename
        if verified_existing(candidate) is not None:
            source_candidate = candidate
            break
    if source_candidate is None:
        raise HarnessError(
            "pinned sherpa-onnx archive is unavailable; seed the workspace shared cache "
            f"before running an offline client Rust check: {shared_candidate}"
        )

    fd, tmp_name = tempfile.mkstemp(
        prefix=f".{filename}.",
        dir=str(shared_directory),
    )
    try:
        with source_candidate.open("rb") as source, os.fdopen(fd, "wb") as target:
            shutil.copyfileobj(source, target)
            target.flush()
            os.fsync(target.fileno())
        tmp_path = Path(tmp_name)
        promoted_sha = sha256_file(tmp_path)
        if promoted_sha != expected_sha:
            raise HarnessError(
                "pinned sherpa-onnx archive changed while promoting it to the "
                f"workspace cache: {source_candidate}"
            )
        try:
            os.link(tmp_path, shared_candidate)
        except FileExistsError:
            concurrent_sha = verified_existing(shared_candidate)
            if concurrent_sha is None:
                raise HarnessError("workspace Sherpa cache raced with an invalid archive")
        directory_fd = os.open(shared_directory, os.O_RDONLY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    finally:
        try:
            os.unlink(tmp_name)
        except FileNotFoundError:
            pass
    final_sha = verified_existing(shared_candidate)
    if final_sha is None:
        raise HarnessError("workspace Sherpa cache promotion did not create the pinned archive")
    return shared_directory, final_sha


def build_check_environment(
    worktree: Path,
    task_dir: Path,
    *,
    playwright_port: Optional[int],
    expose_sherpa_archive: bool = False,
    outer_sandbox_meta_check: bool = False,
) -> Tuple[Dict[str, str], Dict[str, Path]]:
    """Build a fixed allowlist environment for untrusted check commands."""

    runtime = _check_runtime_paths(task_dir)
    real_home = _real_user_home()
    _prepare_private_cargo_home(runtime, real_home)
    rustup_home = (real_home / ".rustup").resolve()
    environment = {
        "PATH": _safe_tool_path(real_home),
        "HOME": str(runtime["home"]),
        "USER": pwd.getpwuid(os.getuid()).pw_name,
        "LOGNAME": pwd.getpwuid(os.getuid()).pw_name,
        "SHELL": "/bin/zsh",
        "TERM": "dumb",
        "LANG": "C.UTF-8",
        "NO_COLOR": "1",
        "CI": "1",
        "PYTHONDONTWRITEBYTECODE": "1",
        "TMPDIR": str(runtime["tmp"]) + os.sep,
        "XDG_CACHE_HOME": str(runtime["xdg_cache"]),
        "NPM_CONFIG_CACHE": str(runtime["npm_cache"]),
        "npm_config_cache": str(runtime["npm_cache"]),
        "CLANG_MODULE_CACHE_PATH": str(runtime["clang_cache"]),
        "CARGO_HOME": str(runtime["cargo_home"].resolve()),
        "RUSTUP_HOME": str(rustup_home),
        "CARGO_TARGET_DIR": str(runtime["cargo_target"].resolve()),
        "CARGO_BUILD_JOBS": "2",
        "CARGO_NET_OFFLINE": "true",
        "CARGO_TERM_COLOR": "never",
        "RUST_TEST_THREADS": "1",
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_CONFIG_NOSYSTEM": "1",
        "MISTRALRS_METAL_PRECOMPILE": "0",
        "MURMUR_HARNESS": "1",
        "MURMUR_HARNESS_TASK": task_dir.name,
        "MURMUR_HARNESS_RUNTIME_DIR": str(runtime["runtime_smoke"].resolve()),
        "MURMUR_HARNESS_INSTRUCTIONS_SHA256": instructions_hash(
            Path(load_json(task_dir / "task.json")["worktree_path"])
        ),
    }
    playwright_cache = real_home / "Library" / "Caches" / "ms-playwright"
    environment["PLAYWRIGHT_BROWSERS_PATH"] = str(playwright_cache.resolve())
    if playwright_port is not None:
        environment["MURMUR_E2E_PORT"] = str(playwright_port)
    if expose_sherpa_archive:
        archive_dir, archive_sha = verified_sherpa_archive(
            worktree,
            task_dir=task_dir,
        )
        environment["SHERPA_ONNX_ARCHIVE_DIR"] = str(archive_dir)
        environment["MURMUR_HARNESS_SHERPA_ARCHIVE_SHA256"] = archive_sha
    if outer_sandbox_meta_check:
        environment[OUTER_SANDBOX_ENV] = "1"
    return environment, runtime


def _seatbelt_literal(path: Path) -> str:
    return json.dumps(str(path.resolve()))


def build_check_seatbelt_profile(
    worktree: Path,
    task_dir: Path,
    *,
    runtime: Mapping[str, Path],
    network_mode: str,
    expose_sherpa_archive: bool = False,
) -> str:
    """Build the fail-closed macOS Seatbelt profile for deterministic checks."""

    if network_mode not in {"none", "loopback"}:
        raise HarnessError(f"unsupported check network mode: {network_mode}")
    primary, common = repo_context(worktree)
    real_home = _real_user_home()
    read_paths = {
        Path("/System"),
        Path("/usr"),
        Path("/bin"),
        Path("/dev"),
        Path("/sbin"),
        Path("/Library"),
        Path("/opt/homebrew"),
        Path("/usr/local"),
        Path("/private/etc"),
        Path("/private/var/db"),
        Path("/private/var/run"),
        worktree.resolve(),
        common.resolve(),
        task_dir.resolve(),
        (real_home / ".cargo").resolve(),
        (real_home / ".rustup").resolve(),
    }
    selected_developer = run_capture(["xcode-select", "-p"], check=False)
    if selected_developer.returncode == 0 and selected_developer.stdout.strip():
        developer_dir = Path(selected_developer.stdout.strip()).resolve()
        allowed_developer_roots = (
            Path("/Applications").resolve(),
            Path("/Library/Developer").resolve(),
        )
        if any(developer_dir == root or root in developer_dir.parents for root in allowed_developer_roots):
            # GitHub's macOS runner selects a versioned Xcode bundle under
            # /Applications. xcrun and the linker dlopen both Developer/ SDK
            # content and Contents/SharedFrameworks; keep that one selected
            # bundle read-only instead of exposing every installed app.
            developer_read_root = developer_dir
            if developer_dir.name == "Developer" and developer_dir.parent.name == "Contents":
                developer_read_root = developer_dir.parent.parent
            read_paths.add(developer_read_root)
    for optional in (
        real_home / ".bun" / "bin",
        real_home / ".local" / "bin",
        real_home / "Library" / "Caches" / "ms-playwright",
        worktree.parent / "murmur-server",
    ):
        read_paths.add(optional.resolve())
    # The hook selftest's Rust-backed mini-repository intentionally lives
    # below the sealed harness source tree. Cargo walks cwd ancestors and will
    # therefore load that tree's committed workspace config. Permit only the
    # exact config leaf for that descendant fixture; ordinary task worktrees
    # must never gain read access to the runner's checkout.
    harness_source_root = HARNESS_ROOT.parents[1]
    if harness_source_root in worktree.resolve().parents:
        cargo_config = harness_source_root / ".cargo" / "config.toml"
        if cargo_config.is_file() and not cargo_config.is_symlink():
            read_paths.add(cargo_config.resolve())
    modules = worktree / "node_modules"
    if modules.is_symlink():
        try:
            modules_target = modules.resolve(strict=True)
            if not modules_target.is_dir():
                raise HarnessError(
                    "shared node_modules target is not a real directory"
                )
            read_paths.add(modules_target)
            # esbuild resolves package metadata from the physical symlink
            # target and may walk back to the manifest beside that target. A
            # v2 verification snapshot is a self-contained repository, so its
            # Git primary is the snapshot rather than the checkout that owns
            # the shared dependency directory. Derive the metadata leaves
            # from the physical target and permit only real regular files, not
            # their parent checkout.
            for manifest_name in ("package.json", "package-lock.json"):
                manifest = modules_target.parent / manifest_name
                try:
                    metadata = manifest.lstat()
                except FileNotFoundError:
                    continue
                if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISREG(
                    metadata.st_mode
                ):
                    raise HarnessError(
                        "shared node_modules manifest is not a real regular "
                        f"file: {manifest}"
                    )
                read_paths.add(manifest.resolve(strict=True))
        except (FileNotFoundError, OSError) as exc:
            raise HarnessError("shared node_modules link became invalid before a check") from exc
    if expose_sherpa_archive:
        sherpa_dir, _ = verified_sherpa_archive(
            worktree,
            task_dir=task_dir,
        )
        read_paths.add(sherpa_dir)

    # Seatbelt's file-read-data filter also applies to directory enumeration.
    # Native resolvers such as esbuild enumerate every ancestor on the way to a
    # symlink target. Permit only the literal ancestor directories; never their
    # subtrees. The explicit leaf paths above remain the sole subtree grants.
    read_ancestors: set[Path] = set()
    for path in read_paths:
        for parent in path.parents:
            read_ancestors.add(parent)

    # The check process never needs direct access to contracts, logs, reviews,
    # or attestations. Those files are written by the parent runner through
    # already-open handles. Keep mutable child state in explicit private leaves
    # so test code cannot pre-create evidence-path symlinks for a later phase.
    write_paths = {worktree.resolve()}
    for key in (
        "home",
        "tmp",
        "xdg_cache",
        "npm_cache",
        "clang_cache",
        "cargo_home",
        "cargo_target",
        "runtime_smoke",
    ):
        write_paths.add(runtime[key].resolve())

    sensitive_paths = [
        real_home / "Desktop",
        real_home / "Documents",
        real_home / "Downloads",
        real_home / "Library" / "Keychains",
        real_home / "Library" / "Application Support" / "MeetNotes",
        real_home / "Library" / "Application Support" / "Murmur",
        real_home / ".aws",
        real_home / ".claude",
        real_home / ".codex",
        real_home / ".config",
        real_home / ".docker",
        real_home / ".git-credentials",
        real_home / ".gitconfig",
        real_home / ".gnupg",
        real_home / ".kube",
        real_home / ".netrc",
        real_home / ".npmrc",
        real_home / ".ssh",
        real_home / ".cargo" / "credentials",
        real_home / ".cargo" / "credentials.toml",
    ]

    system_temp_roots = {
        Path(tempfile.gettempdir()),
        Path(tempfile.gettempdir()).resolve(),
    }
    xcrun_cache_filters = [
        f'(regex #{json.dumps(f"^{re.escape(str(root))}/xcrun_db-[^/]+$")})'
        for root in sorted(system_temp_roots, key=str)
    ]
    xcrun_cache_literals = [
        f'(literal {_seatbelt_literal(root / "xcrun_db")})'
        for root in sorted(system_temp_roots, key=str)
    ]

    read_filters = ['(literal "/")', *xcrun_cache_filters, *xcrun_cache_literals]
    for path in sorted(read_ancestors, key=str):
        read_filters.append(f'(literal {_seatbelt_literal(path)})')
    for path in sorted(read_paths, key=str):
        literal = _seatbelt_literal(path)
        read_filters.extend((f'(literal {literal})', f'(subpath {literal})'))
    read_scope = '(require-any ' + " ".join(read_filters) + ')'
    write_filters: List[str] = [
        '(literal "/dev/null")',
        *xcrun_cache_filters,
        *xcrun_cache_literals,
    ]
    for path in sorted(write_paths, key=str):
        literal = _seatbelt_literal(path)
        write_filters.extend((f'(literal {literal})', f'(subpath {literal})'))
    write_scope = '(require-any ' + " ".join(write_filters) + ')'
    lines = [
        '(version 1)',
        # Browsers and the macOS compiler stack need a wide set of non-file
        # Mach/IOKit/shared-memory operations. Start from platform defaults,
        # then constrain content reads, every write, signals, credentials and
        # network with negative capability filters.
        '(allow default)',
        f'(deny file-read-data (require-not {read_scope}))',
        f'(deny file-write* (require-not {write_scope}))',
        '(deny signal (require-not (target same-sandbox)))',
        '(deny appleevent-send)',
        '(deny mach-lookup (global-name "com.apple.securityd"))',
        '(deny mach-lookup (global-name "com.apple.securityd.xpc"))',
        '(deny mach-lookup (global-name "com.apple.securityd.system"))',
        '(deny mach-lookup (global-name "com.apple.security.agent"))',
        '(deny mach-lookup (global-name "com.apple.SecurityServer"))',
        '(deny mach-lookup (global-name "com.apple.authd"))',
        '(deny mach-lookup (global-name "com.apple.akd"))',
        '(deny mach-lookup (global-name "com.apple.secd"))',
    ]
    for path in ((real_home / ".cargo").resolve(), (real_home / ".rustup").resolve()):
        literal = _seatbelt_literal(path)
        lines.append(f'(deny file-write* (literal {literal}) (subpath {literal}))')
    for path in sensitive_paths:
        literal = _seatbelt_literal(path)
        lines.append(f'(deny file-read* (literal {literal}) (subpath {literal}))')
        lines.append(f'(deny file-write* (literal {literal}) (subpath {literal}))')
    if network_mode == "loopback":
        lines.extend(
            [
                '(deny network-bind (require-not (local ip "localhost:*")))',
                '(deny network-inbound (require-not (local ip "localhost:*")))',
                '(deny network-outbound (require-not (remote ip "localhost:*")))',
            ]
        )
    else:
        lines.append('(deny network*)')
    return "\n".join(lines) + "\n"


def sandboxed_check_argv(profile: str, command: str) -> List[str]:
    sandbox = Path("/usr/bin/sandbox-exec")
    if sys.platform != "darwin" or not sandbox.is_file() or not os.access(sandbox, os.X_OK):
        raise HarnessError("deterministic checks require executable macOS /usr/bin/sandbox-exec")
    return [str(sandbox), "-p", profile, "/bin/zsh", "-f", "-c", command]


def command_is_inherited_sandbox_meta_check(command: str) -> bool:
    return command.strip() in INHERITED_SANDBOX_META_CHECKS


def inherited_outer_sandbox_is_active() -> bool:
    """Prove a nested meta-selftest is already inside the outer Seatbelt.

    The environment marker is routing metadata, never authority: a host process
    can forge it. The sandbox_check result is the fail-closed kernel proof.
    """

    if os.environ.get(OUTER_SANDBOX_ENV) != "1":
        return False
    if sys.platform != "darwin":
        raise HarnessError("inherited check sandbox is supported only on macOS")
    library_path = ctypes.util.find_library("sandbox")
    if not library_path:
        raise HarnessError("cannot resolve macOS sandbox library")
    try:
        library = ctypes.CDLL(library_path)
        sandbox_check = library.sandbox_check
        sandbox_check.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
        sandbox_check.restype = ctypes.c_int
        denied = sandbox_check(os.getpid(), b"network-outbound", 0)
    except (AttributeError, OSError) as exc:
        raise HarnessError("cannot verify inherited macOS sandbox") from exc
    if denied != 1:
        raise HarnessError(
            "refusing inherited-sandbox mode without a kernel-enforced no-network outer sandbox"
        )
    return True


def _pid_is_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
        return True
    except PermissionError:
        return True
    except ProcessLookupError:
        return False


def _release_owned_directory_lock(lock: Optional[Path]) -> None:
    if lock is None:
        return
    owner_path = lock / "owner.json"
    try:
        owner = load_json(owner_path)
        if int(owner.get("pid", -1)) != os.getpid():
            raise HarnessError(f"refusing to release lock owned by another process: {lock}")
        owner_path.unlink()
        lock.rmdir()
    except FileNotFoundError:
        pass


def git_common_for_task_dir(task_dir: Path) -> Path:
    """Resolve the Git common directory for nested task evidence."""

    resolved = task_dir.resolve()
    for candidate in (resolved, *resolved.parents):
        if candidate.name == "agent-harness":
            return candidate.parent
    raise HarnessError(f"task evidence is outside agent-harness: {task_dir}")


def shared_resource_root_for_common(git_common_dir: Path) -> Path:
    """Return the Murmur workspace-wide resource root.

    A dedicated harness driver is intentionally a standalone clone, so its
    Git common directory differs from the user's primary checkout.  A lock
    stored in either ``.git`` would therefore serialize only half the machine.
    Every Murmur checkout kept under the same workspace parent instead shares
    the narrowly-scoped sibling ``.murmur-agent-tasks/.resources`` directory.
    """

    completed = subprocess.run(
        [
            "git",
            "--git-dir",
            str(git_common_dir),
            "worktree",
            "list",
            "--porcelain",
        ],
        text=True,
        capture_output=True,
        check=False,
    )
    if completed.returncode != 0:
        raise HarnessError(
            "cannot resolve the repository primary for shared resources: "
            + completed.stderr.strip()
        )
    first = next(
        (
            line.removeprefix("worktree ").strip()
            for line in completed.stdout.splitlines()
            if line.startswith("worktree ")
        ),
        "",
    )
    if not first:
        raise HarnessError("cannot resolve a primary worktree for shared resources")
    primary = Path(first).resolve()
    return primary.parent / ".murmur-agent-tasks" / ".resources"


def shared_resource_root_for_task(task_dir: Path) -> Path:
    return shared_resource_root_for_common(git_common_for_task_dir(task_dir))


def _cargo_lane_owner(lock_handle: Any) -> Dict[str, Any]:
    try:
        lock_handle.seek(0)
        raw = lock_handle.read().strip()
        document = json.loads(raw) if raw else {}
    except (OSError, json.JSONDecodeError):
        document = {}
    if not isinstance(document, dict):
        document = {}
    return {
        "owner_pid": document.get("pid", "unknown"),
        "task": document.get("task", document.get("task_id", "unknown")),
        "command": document.get("command", "unknown"),
        "since": document.get(
            "since", document.get("acquired_at", "unknown")
        ),
    }


def _print_cargo_lane_wait(
    lock_handle: Any,
    queue_status: Mapping[str, Any],
) -> None:
    owner = _cargo_lane_owner(lock_handle)
    print(
        "agent-harness: waiting for Cargo lane "
        "owner_pid={owner_pid} task={task} command={command} since={since} "
        "queue_position={queue_position}/{queue_depth} queued_since={queued_since} "
        "heartbeat={heartbeat}".format(
            **owner,
            queue_position=queue_status["position"],
            queue_depth=queue_status["depth"],
            queued_since=queue_status["queued_at"],
            heartbeat=utc_now(),
        ),
        file=sys.stderr,
        flush=True,
    )


def acquire_cargo_lane(
    task_dir: Path,
    timeout_seconds: float,
    *,
    command: str = "unknown",
    heartbeat_seconds: float = 5.0,
) -> Any:
    """Acquire the one cross-task Rust build lane.

    This is a kernel advisory lock, not a PID-file lock: normal exit, cancellation,
    crash and SIGKILL all release it automatically when the descriptor closes. The
    small JSON payload is diagnostic only and never used to infer ownership.
    """

    # Share the exact kernel lane used by scripts/agent-resource-run across the
    # Harness, operator checkout, and standalone driver clone.
    resource_root = shared_resource_root_for_task(task_dir)
    resource_root.mkdir(parents=True, exist_ok=True)
    lock_path = resource_root / "cargo.lock"
    ticket = resource_lane.join_lane_queue(
        resource_root,
        task=task_dir.name,
        command=command,
    )
    lock_handle = lock_path.open("a+", encoding="utf-8")
    deadline = time.monotonic() + timeout_seconds
    next_heartbeat: Optional[float] = None
    try:
        while True:
            queue_status = resource_lane.lane_queue_status(ticket)
            acquired = False
            if queue_status["position"] == 1:
                try:
                    fcntl.flock(
                        lock_handle.fileno(),
                        fcntl.LOCK_EX | fcntl.LOCK_NB,
                    )
                    acquired = True
                except BlockingIOError:
                    pass
            if acquired:
                break
            now = time.monotonic()
            if next_heartbeat is None or now >= next_heartbeat:
                _print_cargo_lane_wait(lock_handle, queue_status)
                next_heartbeat = now + max(0.01, heartbeat_seconds)
            if now >= deadline:
                owner = _cargo_lane_owner(lock_handle)
                raise HarnessError(
                    "timed out waiting for the shared Cargo build lane "
                    "owner_pid={owner_pid} task={task} command={command} "
                    "since={since} queue_position={position}/{depth} "
                    "queued_since={queued_at}".format(
                        **owner,
                        **queue_status,
                    )
                )
            time.sleep(
                min(
                    0.1,
                    max(0.0, deadline - now),
                    max(0.01, next_heartbeat - now),
                )
            )
        lock_handle.seek(0)
        lock_handle.truncate()
        json.dump(
            {
                "pid": os.getpid(),
                "cwd": str(Path.cwd()),
                "task_id": task_dir.name,
                "task": task_dir.name,
                "command": command,
                "acquired_at": utc_now(),
                "since": utc_now(),
            },
            lock_handle,
            sort_keys=True,
        )
        lock_handle.write("\n")
        lock_handle.flush()
        resource_lane.leave_lane_queue(ticket)
        return lock_handle
    except BaseException:
        resource_lane.leave_lane_queue(ticket)
        lock_handle.close()
        raise


def release_cargo_lane(lock_handle: Optional[Any]) -> None:
    if lock_handle is None:
        return
    # Close only this runner's descriptor. An explicit LOCK_UN would release the shared open-file
    # description even if an inherited managed descendant somehow survived cleanup. Plain close
    # keeps the lane held by any such survivor and releases it automatically once the last holder
    # exits.
    lock_handle.close()


def acquire_playwright_port(task_dir: Path, timeout_seconds: float) -> Tuple[Path, int]:
    """Reserve a task-private loopback port across concurrent harness runs."""

    locks_root = shared_resource_root_for_task(task_dir) / "playwright-ports"
    locks_root.mkdir(parents=True, exist_ok=True)
    port_floor = 42000
    port_count = 20000
    seed = int(hashlib.sha256(task_dir.name.encode("utf-8")).hexdigest()[:8], 16)
    deadline = time.monotonic() + timeout_seconds
    offset = 0
    while time.monotonic() < deadline:
        port = port_floor + ((seed + offset) % port_count)
        offset += 1
        lock = locks_root / f"{port}.lock"
        try:
            lock.mkdir()
            atomic_write_json(
                lock / "owner.json",
                {
                    "pid": os.getpid(),
                    "task_id": task_dir.name,
                    "port": port,
                    "acquired_at": utc_now(),
                },
            )
        except FileExistsError:
            owner_path = lock / "owner.json"
            try:
                owner_pid = int(load_json(owner_path)["pid"])
            except (HarnessError, KeyError, TypeError, ValueError):
                owner_pid = 0
            if owner_pid > 0 and not _pid_is_alive(owner_pid):
                try:
                    owner_path.unlink()
                    lock.rmdir()
                    offset -= 1
                    continue
                except (FileNotFoundError, OSError):
                    pass
            continue

        probe = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        try:
            probe.bind(("127.0.0.1", port))
        except OSError:
            _release_owned_directory_lock(lock)
            continue
        finally:
            probe.close()
        return lock, port
    raise HarnessError("timed out reserving a task-private Playwright port")


def _write_all(descriptor: int, payload: bytes) -> None:
    view = memoryview(payload)
    while view:
        written = os.write(descriptor, view)
        if written <= 0:
            raise OSError("short write to managed child stdin")
        view = view[written:]


def run_guarded_process(
    argv: Sequence[str],
    *,
    cwd: Path,
    timeout_seconds: float,
    stdout_handle: Any,
    stderr_handle: Any,
    result_path: Path,
    stdin_bytes: Optional[bytes] = None,
    env: Optional[Mapping[str, str]] = None,
    inherited_fds: Sequence[int] = (),
    term_grace_seconds: float = 3.0,
) -> Dict[str, Any]:
    """Run through an out-of-process parent-death guardian.

    If this runner is SIGKILLed, the liveness pipe closes in the guardian. The
    guardian then terminates the exact new-session child group before releasing
    inherited resource-lane descriptors.
    """

    if not PROCESS_GUARDIAN_PATH.is_file():
        raise HarnessError(f"process guardian is missing: {PROCESS_GUARDIAN_PATH}")
    result_path.parent.mkdir(parents=True, exist_ok=True)
    try:
        result_path.lstat()
    except FileNotFoundError:
        pass
    else:
        raise HarnessError(
            f"refusing to overwrite prior guardian artifact: {result_path}"
        )
    parent_read, parent_write = os.pipe()
    stdin_read: Optional[int] = None
    stdin_write: Optional[int] = None
    if stdin_bytes is not None:
        stdin_read, stdin_write = os.pipe()
    pass_fds = [parent_read, *inherited_fds]
    if stdin_read is not None:
        pass_fds.append(stdin_read)
    command = [
        sys.executable,
        str(PROCESS_GUARDIAN_PATH),
        "--parent-fd",
        str(parent_read),
        "--result",
        str(result_path),
        "--cwd",
        str(cwd),
        "--timeout-seconds",
        str(float(timeout_seconds)),
        "--term-grace-seconds",
        str(float(term_grace_seconds)),
    ]
    if stdin_read is not None:
        command.extend(["--stdin-fd", str(stdin_read)])
    for descriptor in inherited_fds:
        command.extend(["--pass-fd", str(descriptor)])
    command.extend(["--", *list(argv)])
    guardian: Optional[subprocess.Popen[Any]] = None
    try:
        guardian = subprocess.Popen(
            command,
            cwd=str(cwd),
            env=dict(env) if env else None,
            stdin=subprocess.DEVNULL,
            stdout=stdout_handle,
            stderr=stderr_handle,
            start_new_session=True,
            pass_fds=tuple(pass_fds),
        )
        os.close(parent_read)
        parent_read = -1
        if stdin_read is not None:
            os.close(stdin_read)
            stdin_read = None
        if stdin_write is not None:
            try:
                _write_all(stdin_write, stdin_bytes or b"")
            finally:
                os.close(stdin_write)
                stdin_write = None
        try:
            guardian.wait(
                timeout=float(timeout_seconds) + max(0.0, term_grace_seconds) + 10.0
            )
        except subprocess.TimeoutExpired:
            _terminate_process(guardian)
        if guardian.returncode is None:
            guardian.wait()
        if not result_path.is_file():
            raise HarnessError(
                "managed child guardian exited without a durable result"
            )
        result = load_json(result_path)
        if result.get("parent_lost"):
            raise HarnessError("managed child observed an unexpected parent death")
        if result.get("error"):
            raise HarnessError(f"managed child guardian failed: {result['error']}")
        if guardian.returncode != 0:
            raise HarnessError(
                f"managed child guardian exited {guardian.returncode}"
            )
        if not isinstance(result.get("exit_code"), int):
            raise HarnessError("managed child result has no exit code")
        result["guardian_path"] = str(result_path)
        result["guardian_sha256"] = sha256_file(result_path)
        return result
    except BaseException:
        # Closing this descriptor is the parent-death signal. Give the guardian
        # time to reap its owned group before escalating against the guardian.
        try:
            os.close(parent_write)
        except OSError:
            pass
        parent_write = -1
        if guardian is not None and guardian.poll() is None:
            try:
                guardian.wait(timeout=max(0.0, term_grace_seconds) + 5.0)
            except subprocess.TimeoutExpired:
                _terminate_process(guardian)
        raise
    finally:
        for descriptor in (parent_read, parent_write, stdin_read, stdin_write):
            if descriptor is not None and descriptor >= 0:
                try:
                    os.close(descriptor)
                except OSError:
                    pass


def run_logged_process(
    argv: Sequence[str],
    *,
    cwd: Path,
    timeout_seconds: float,
    log_path: Path,
    stdin_bytes: Optional[bytes] = None,
    env: Optional[Mapping[str, str]] = None,
    execution_id: Optional[str] = None,
    term_grace_seconds: float = 3.0,
) -> Dict[str, Any]:
    execution_id = execution_id or _new_execution_id()
    log_path = _execution_artifact_path(log_path, execution_id)
    log_path.parent.mkdir(parents=True, exist_ok=True)
    started = time.monotonic()
    with log_path.open("xb") as log_handle:
        guarded = run_guarded_process(
            argv,
            cwd=cwd,
            timeout_seconds=timeout_seconds,
            stdout_handle=log_handle,
            stderr_handle=subprocess.STDOUT,
            result_path=log_path.with_suffix(log_path.suffix + ".guardian.json"),
            stdin_bytes=stdin_bytes,
            env=env,
            term_grace_seconds=term_grace_seconds,
        )
    duration_ms = int((time.monotonic() - started) * 1000)
    return {
        "exit_code": guarded["exit_code"],
        "leader_exit_code": guarded.get("leader_exit_code"),
        "leader_exited_with_live_group": bool(
            guarded.get("leader_exited_with_live_group")
        ),
        "termination_reason": guarded.get("termination_reason"),
        "timed_out": bool(guarded.get("timed_out")),
        "duration_ms": duration_ms,
        "execution_id": execution_id,
        "log": str(log_path),
        "log_sha256": sha256_file(log_path),
        "guardian_path": guarded["guardian_path"],
        "guardian_sha256": guarded["guardian_sha256"],
    }


# A check's stdout is DEVELOPER-CONTROLLED (the developer authors the code a check runs), so it
# can never be authoritative over the outcome — the harness derives PASS/FAIL from the exit
# code alone. The single exception is an ENVIRONMENTAL block: a stray dev server owning an
# exclusive port must read as "cannot evaluate here", not as a red test. That signal is
# trustworthy ONLY from a runner-owned probe whose command is
# a canonical, scope-verified runner script (the exact-diff planner rejects any
# non-owned change before checks run) that decides on foreign port ownership
# BEFORE executing developer code.
# So it is bound to a specific canonical check id AND a dedicated exit code — never stdout.
ENVIRONMENT_PROBE_CHECK_IDS = frozenset({"tauri-boot"})
ENVIRONMENT_BLOCKED_EXIT_CODE = 3


def _check_outcome(check_id: str, exit_code: int, timed_out: bool) -> Tuple[bool, str]:
    """Derive ``(passed, outcome)`` from a check's exit code only. Returns outcome
    ``BLOCKED`` (not ``passed``, but not a code FAIL either) only for a runner-owned
    environment probe that exits with the dedicated blocked code."""

    if timed_out:
        return (False, "FAIL")
    if exit_code == 0:
        return (True, "PASS")
    if check_id in ENVIRONMENT_PROBE_CHECK_IDS and exit_code == ENVIRONMENT_BLOCKED_EXIT_CODE:
        return (False, "BLOCKED")
    return (False, "FAIL")


def _probe_blocked_reason(stdout_path: Path) -> Optional[str]:
    """Best-effort human reason from an env-probe's JSON stdout — informational ONLY.

    The BLOCKED decision is made by :func:`_check_outcome` from the exit code; this merely
    surfaces the probe's own message and never influences the verdict.
    """

    last = None
    try:
        with stdout_path.open("r", encoding="utf-8", errors="replace") as handle:
            for line in handle:
                stripped = line.strip()
                if stripped:
                    last = stripped
        if last is None:
            return None
        obj = json.loads(last)
    except (OSError, json.JSONDecodeError):
        return None
    if isinstance(obj, dict) and isinstance(obj.get("reason"), str):
        return obj["reason"].strip() or None
    return None


def run_check(
    worktree: Path,
    task_dir: Path,
    check: Mapping[str, Any],
    phase: str,
    *,
    bound_environment: Optional[Mapping[str, str]] = None,
) -> Dict[str, Any]:
    safe_phase = re.sub(r"[^a-z0-9._-]+", "-", phase.lower())
    execution_id = _new_execution_id()
    log_path = _execution_artifact_path(
        task_dir / "logs" / f"{safe_phase}-{check['id']}.log",
        execution_id,
    )
    stdout_path = log_path.with_suffix(".stdout.log")
    stderr_path = log_path.with_suffix(".stderr.log")
    guardian_result_path = _execution_artifact_path(
        task_dir
        / "runtime"
        / "guardians"
        / f"{safe_phase}-{check['id']}.json",
        execution_id,
    )
    uses_playwright = command_uses_playwright(str(check["command"]))
    needs_loopback = command_needs_loopback(str(check["command"]))
    needs_sherpa = command_needs_sherpa_archive(str(check["command"]), worktree)
    started = time.monotonic()
    started_at = utc_now()
    deadline = started + float(check["timeout_seconds"])
    timed_out = False
    exit_code: Optional[int] = None
    guardian_path: Optional[str] = None
    guardian_sha256: Optional[str] = None
    resource_wait_ms = 0
    cargo_lane: Optional[Any] = None
    playwright_lock: Optional[Path] = None
    playwright_port: Optional[int] = None
    inherited_sandbox = inherited_outer_sandbox_is_active()
    try:
        if command_uses_cargo_lane(str(check["command"])):
            remaining_for_cargo = deadline - time.monotonic()
            if remaining_for_cargo <= 0:
                raise HarnessError(f"check {check['id']} exhausted its deadline before the Cargo lane")
            resource_wait_started = time.monotonic()
            cargo_lane = acquire_cargo_lane(
                task_dir,
                remaining_for_cargo,
                command=str(check["command"]),
            )
            resource_wait_ms = int(
                (time.monotonic() - resource_wait_started) * 1000
            )
        if uses_playwright:
            remaining_for_port = deadline - time.monotonic()
            if remaining_for_port <= 0:
                raise HarnessError(f"check {check['id']} exhausted its deadline before a UI port")
            playwright_lock, playwright_port = acquire_playwright_port(task_dir, remaining_for_port)
        environment, runtime = build_check_environment(
            worktree,
            task_dir,
            playwright_port=playwright_port,
            expose_sherpa_archive=needs_sherpa,
            outer_sandbox_meta_check=command_is_inherited_sandbox_meta_check(
                str(check["command"])
            ),
        )
        bound_environment_record = dict(
            sorted((bound_environment or {}).items())
        )
        if set(bound_environment_record) - {"MURMUR_HARNESS_BASE_SHA"}:
            raise HarnessError("check requested an unsupported bound environment key")
        base_sha = bound_environment_record.get("MURMUR_HARNESS_BASE_SHA")
        if base_sha is not None and not SHA1_RE.fullmatch(base_sha):
            raise HarnessError("check bound base SHA is malformed")
        for name, value in bound_environment_record.items():
            if name in environment:
                raise HarnessError(
                    f"check bound environment would replace runner key {name}"
                )
            environment[name] = value
        network_mode = "loopback" if needs_loopback else "none"
        profile = build_check_seatbelt_profile(
            worktree,
            task_dir,
            runtime=runtime,
            network_mode=network_mode,
            expose_sherpa_archive=needs_sherpa,
        )
        profile_path = _execution_artifact_path(
            runtime["profiles"] / f"{safe_phase}-{check['id']}.sb",
            execution_id,
        )
        atomic_create_bytes(profile_path, profile.encode("utf-8"))
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise HarnessError(f"check {check['id']} exhausted its deadline before process start")
        stdout_path.parent.mkdir(parents=True, exist_ok=True)
        with stdout_path.open("xb") as stdout_handle, stderr_path.open("xb") as stderr_handle:
            check_argv = (
                ["/bin/zsh", "-f", "-c", str(check["command"])]
                if inherited_sandbox
                else sandboxed_check_argv(profile, str(check["command"]))
            )
            guarded = run_guarded_process(
                check_argv,
                cwd=worktree,
                timeout_seconds=remaining,
                stdout_handle=stdout_handle,
                stderr_handle=stderr_handle,
                result_path=guardian_result_path,
                env=environment,
                inherited_fds=(
                    (cargo_lane.fileno(),) if cargo_lane is not None else ()
                ),
            )
            exit_code = int(guarded["exit_code"])
            timed_out = bool(guarded.get("timed_out"))
            guardian_path = str(guarded["guardian_path"])
            guardian_sha256 = str(guarded["guardian_sha256"])
            leader_exit_code = guarded.get("leader_exit_code")
            leader_exited_with_live_group = bool(
                guarded.get("leader_exited_with_live_group")
            )
            termination_reason = guarded.get("termination_reason")
    finally:
        try:
            _release_owned_directory_lock(playwright_lock)
        finally:
            release_cargo_lane(cargo_lane)
    if exit_code is None or guardian_path is None or guardian_sha256 is None:
        raise HarnessError(f"check {check['id']} did not start")
    duration_ms = int((time.monotonic() - started) * 1000)
    combined = (
        b"=== stdout ===\n"
        + stdout_path.read_bytes()
        + b"\n=== stderr ===\n"
        + stderr_path.read_bytes()
    )
    atomic_create_bytes(log_path, combined)
    result = {
        "exit_code": exit_code,
        "leader_exit_code": leader_exit_code,
        "leader_exited_with_live_group": leader_exited_with_live_group,
        "termination_reason": termination_reason,
        "timed_out": timed_out,
        "duration_ms": duration_ms,
        "resource_wait_ms": resource_wait_ms,
        "execution_id": execution_id,
        "log_path": str(log_path),
        "log_sha256": sha256_file(log_path),
        "stdout_path": str(stdout_path),
        "stdout_sha256": sha256_file(stdout_path),
        "stderr_path": str(stderr_path),
        "stderr_sha256": sha256_file(stderr_path),
        "sandbox_profile_path": str(profile_path),
        "sandbox_profile_sha256": sha256_file(profile_path),
        "guardian_path": guardian_path,
        "guardian_sha256": guardian_sha256,
        "sandbox_mode": "inherited" if inherited_sandbox else "direct",
        "environment_keys_sha256": sha256_bytes(canonical_json(sorted(environment))),
        "environment_sha256": sha256_bytes(canonical_json(environment)),
        "bound_environment": bound_environment_record,
        "playwright_port": playwright_port,
        "network_mode": network_mode,
        "started_at": started_at,
        "created_at": utc_now(),
    }
    # Outcome is derived from the EXIT CODE only (stdout is developer-controlled). A runner-owned
    # environment probe may report BLOCKED via its dedicated exit code — "cannot evaluate in
    # this environment", not a red test — so a stray dev server owning a port does not read as
    # a code FAIL. See _check_outcome for the security rationale.
    passed, outcome = _check_outcome(check["id"], result["exit_code"], result["timed_out"])
    blocked_reason = _probe_blocked_reason(stdout_path) if outcome == "BLOCKED" else None
    evidence = {
        "id": check["id"],
        "command": check["command"],
        "phase": phase,
        **result,
        "passed": passed,
        "outcome": outcome,
        "blocked_reason": blocked_reason,
    }
    append_jsonl(
        task_dir / "events.jsonl",
        {
            "at": utc_now(),
            "event": "check",
            "id": evidence["id"],
            "phase": phase,
            "exit_code": evidence["exit_code"],
            "leader_exit_code": evidence["leader_exit_code"],
            "leader_exited_with_live_group": evidence[
                "leader_exited_with_live_group"
            ],
            "termination_reason": evidence["termination_reason"],
            "timed_out": evidence["timed_out"],
            "duration_ms": evidence["duration_ms"],
            "resource_wait_ms": evidence["resource_wait_ms"],
            "passed": evidence["passed"],
            "outcome": evidence["outcome"],
            "blocked_reason": evidence["blocked_reason"],
            "execution_id": evidence["execution_id"],
            "log_path": evidence["log_path"],
            "stdout_path": evidence["stdout_path"],
            "stderr_path": evidence["stderr_path"],
            "guardian_path": evidence["guardian_path"],
            "sandbox_profile_path": evidence["sandbox_profile_path"],
            "network_mode": evidence["network_mode"],
            "sandbox_mode": evidence["sandbox_mode"],
            "created_at": evidence["created_at"],
        },
    )
    return evidence


def command_version(command: str) -> Optional[str]:
    path = shutil.which(command)
    if not path:
        return None
    completed = run_capture([path, "--version"], check=False)
    text = f"{completed.stdout}\n{completed.stderr}"
    match = re.search(r"\d+(?:\.\d+){1,3}", text)
    return match.group(0) if match else text.strip().splitlines()[0]


def runtime_preflight(repo: Path) -> Dict[str, Any]:
    """Fail before worktree/model work when exclusive runtime ownership is blocked."""

    script = HARNESS_ROOT.parent.parent / "scripts" / "harness-runtime-smoke"
    completed = run_capture(
        [str(script), "--preflight"],
        repo,
        check=False,
    )
    lines = [line for line in completed.stdout.splitlines() if line.strip()]
    try:
        payload = json.loads(lines[-1]) if lines else {}
    except json.JSONDecodeError as exc:
        raise HarnessError(
            "runtime preflight returned malformed evidence: "
            + (completed.stderr.strip() or completed.stdout.strip())
        ) from exc
    if completed.returncode != 0 or payload.get("verdict") != "PASS":
        reason = payload.get("reason") if isinstance(payload, dict) else None
        raise HarnessError(
            "runtime preflight blocked before task setup: "
            + str(reason or completed.stderr.strip() or f"exit {completed.returncode}"),
            exit_code=3 if completed.returncode == 3 else 1,
        )
    return payload


def sanitized_model_environment(instructions_sha256: str, vendor: str) -> Tuple[Dict[str, str], List[str]]:
    """Return the minimal host environment needed by the CLI, never ambient secrets."""

    allowed = {
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "TMPDIR",
        "LANG",
        "SHELL",
        "TERM",
        "NO_COLOR",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
    }
    environment = {
        key: value
        for key, value in os.environ.items()
        if key in allowed or key.startswith("LC_")
    }
    # A usable executable search path is required even under aggressively
    # scrubbed launch environments (e.g. GUI shells with no exported PATH).
    environment.setdefault("PATH", os.defpath)
    environment["MURMUR_HARNESS_INSTRUCTIONS_SHA256"] = instructions_sha256
    if vendor == "claude":
        environment["CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC"] = "1"
    removed_names = sorted(set(os.environ) - set(environment))
    return environment, removed_names


def _extract_claude_result(log_path: Path) -> Any:
    candidate: Any = None
    with log_path.open("r", encoding="utf-8", errors="replace") as handle:
        for line in handle:
            line = line.strip()
            if not line:
                continue
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            if not isinstance(event, dict):
                continue
            if isinstance(event.get("structured_output"), dict):
                candidate = event["structured_output"]
            elif event.get("type") == "result":
                raw = event.get("result")
                if isinstance(raw, dict):
                    candidate = raw
                elif isinstance(raw, str):
                    try:
                        candidate = json.loads(raw)
                    except json.JSONDecodeError:
                        pass
    if candidate is None:
        raise HarnessError(f"Claude returned no structured result; inspect {log_path}")
    return candidate


def _claude_terminal_subtype(log_path: Path) -> Optional[str]:
    """The `subtype` of the last Claude-CLI `{"type":"result"}` event, or None."""

    subtype: Optional[str] = None
    try:
        with log_path.open("r", encoding="utf-8", errors="replace") as handle:
            for line in handle:
                line = line.strip()
                if not line:
                    continue
                try:
                    event = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if (
                    isinstance(event, dict)
                    and event.get("type") == "result"
                    and isinstance(event.get("subtype"), str)
                ):
                    subtype = event["subtype"]
    except OSError:
        return None
    return subtype


def extract_model_metadata(
    log_path: Path, vendor: str, fallback_session: str
) -> Dict[str, Any]:
    """Best-effort identity and usage extraction from vendor JSONL.

    Model CLIs do not expose one stable cross-vendor envelope, so keep this
    deliberately tolerant and retain the last typed value encountered.  The
    runner records the result for successful *and failed* invocations; metrics
    therefore do not need to re-parse multi-megabyte raw logs later.
    """

    session_id = fallback_session
    model = "unspecified"
    total_cost_usd: Optional[float] = None
    num_turns: Optional[int] = None
    usage: Optional[Mapping[str, Any]] = None
    terminal_reason: Optional[str] = None
    http_status: Optional[int] = None
    retry_after_seconds: Optional[float] = None

    def walk(value: Any) -> Iterable[Tuple[str, Any]]:
        if isinstance(value, Mapping):
            for key, child in value.items():
                yield str(key), child
                yield from walk(child)
        elif isinstance(value, list):
            for child in value:
                yield from walk(child)

    try:
        with log_path.open("r", encoding="utf-8", errors="replace") as handle:
            for line in handle:
                try:
                    event = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if not isinstance(event, dict):
                    continue
                if vendor == "codex":
                    if event.get("type") == "thread.started" and isinstance(event.get("thread_id"), str):
                        session_id = event["thread_id"]
                    payload = event.get("item")
                    if isinstance(payload, dict) and isinstance(payload.get("model"), str):
                        model = payload["model"]
                elif vendor == "claude":
                    if isinstance(event.get("session_id"), str):
                        session_id = event["session_id"]
                    if isinstance(event.get("model"), str):
                        model = event["model"]
                    message = event.get("message")
                    if isinstance(message, dict) and isinstance(message.get("model"), str):
                        model = message["model"]
                for key, value in walk(event):
                    lowered = key.lower()
                    if lowered in {"total_cost_usd", "cost_usd"}:
                        try:
                            total_cost_usd = float(value)
                        except (TypeError, ValueError):
                            pass
                    elif lowered in {"num_turns", "turns"}:
                        try:
                            num_turns = int(value)
                        except (TypeError, ValueError):
                            pass
                    elif lowered == "usage" and isinstance(value, Mapping):
                        usage = dict(value)
                    elif lowered in {
                        "terminal_reason",
                        "stop_reason",
                        "subtype",
                    } and isinstance(value, str):
                        terminal_reason = value
                    elif lowered in {
                        "api_error_status",
                        "status_code",
                        "http_status",
                    }:
                        try:
                            http_status = int(value)
                        except (TypeError, ValueError):
                            pass
                    elif lowered in {"retry_after", "retry_after_seconds"}:
                        try:
                            retry_after_seconds = max(0.0, float(value))
                        except (TypeError, ValueError):
                            pass
    except OSError:
        pass
    return {
        "session_id": session_id,
        "model": model,
        "total_cost_usd": total_cost_usd,
        "num_turns": num_turns,
        "usage": usage,
        "terminal_reason": terminal_reason,
        "http_status": http_status,
        "retry_after_seconds": retry_after_seconds,
    }


def _record_model_process_exit(
    task_dir: Path,
    *,
    label: str,
    role: str,
    vendor: str,
    timeout_seconds: int,
    process_result: Mapping[str, Any],
    result_path: Path,
    invocation_path: Path,
    terminal_subtype: Optional[str],
) -> None:
    """Persist the exact per-execution process artifacts before any error raises."""

    log_path = Path(str(process_result.get("log", "")))
    telemetry = extract_model_metadata(
        log_path,
        vendor,
        str(process_result.get("execution_id") or label),
    )
    append_jsonl(
        task_dir / "events.jsonl",
        {
            "at": utc_now(),
            "event": "model-process-exit",
            "label": label,
            "role": role,
            "vendor": vendor,
            "exit_code": process_result.get("exit_code"),
            "leader_exit_code": process_result.get("leader_exit_code"),
            "leader_exited_with_live_group": bool(
                process_result.get("leader_exited_with_live_group")
            ),
            "termination_reason": process_result.get("termination_reason"),
            "timed_out": bool(process_result.get("timed_out")),
            "duration_ms": process_result.get("duration_ms"),
            "wall_timeout_seconds": timeout_seconds,
            "terminal_subtype": terminal_subtype,
            "execution_id": process_result.get("execution_id"),
            "log_path": process_result.get("log"),
            "log_sha256": process_result.get("log_sha256"),
            "guardian_path": process_result.get("guardian_path"),
            "guardian_sha256": process_result.get("guardian_sha256"),
            "result_path": str(result_path),
            "invocation_path": str(invocation_path),
            "total_cost_usd": telemetry.get("total_cost_usd"),
            "num_turns": telemetry.get("num_turns"),
            "usage": telemetry.get("usage"),
            "http_status": telemetry.get("http_status"),
            "retry_after_seconds": telemetry.get("retry_after_seconds"),
        },
    )


def invoke_model(
    vendor: str,
    *,
    role: str,
    prompt: str,
    schema_name: str,
    worktree: Path,
    task_dir: Path,
    label: str,
    timeout_seconds: int,
    instructions_sha256: str,
) -> Dict[str, Any]:
    """Run one fresh, tool-denied reviewer process."""

    if role != "reviewer":
        raise HarnessError("the verifier-only Harness supports reviewer invocations only")
    schema_path = SCHEMAS_DIR / f"{schema_name}.schema.json"
    schema = load_schema(schema_name)
    invocation_id = _new_execution_id()
    log_base_path = task_dir / "logs" / f"{label}-{vendor}.jsonl"
    log_path = _execution_artifact_path(log_base_path, invocation_id)
    result_path = _execution_artifact_path(
        task_dir / "results" / f"{label}-{vendor}.json",
        invocation_id,
    )
    invocation_path = _execution_artifact_path(
        task_dir / "results" / f"{label}-{vendor}-invocation.json",
        invocation_id,
    )
    result_path.parent.mkdir(parents=True, exist_ok=True)
    recorded_argv: List[str] = [vendor, "fake"]
    budget: Optional[str] = None
    removed_env_names: List[str] = []
    model_cwd = reviewer_execution_cwd()

    if vendor == "fake":
        verdict_override = os.environ.get(f"MURMUR_HARNESS_FAKE_{label.upper().replace('-', '_')}_VERDICT")
        verdict = verdict_override or os.environ.get(
            "MURMUR_HARNESS_FAKE_REVIEW_VERDICT", "PASS"
        )
        findings: List[Dict[str, str]] = []
        if verdict != "PASS" and not (
            schema_name == "v2-review" and verdict == "BLOCKED"
        ):
            findings.append(
                {
                    "severity": "MAJOR" if verdict == "FAIL" else "BLOCKER",
                    "file": "",
                    "evidence": "synthetic fake-adapter finding",
                    "required_fix": "resolve the synthetic finding",
                }
            )
        document = {
            "verdict": verdict,
            "summary": f"fake {label} reviewer returned {verdict}",
            "requirements_covered": ["selftest"],
            "findings": findings,
        }
        if schema_name == "v2-review":
            fake_proof_gaps = os.environ.get(
                "MURMUR_HARNESS_FAKE_REVIEW_PROOF_GAPS_JSON"
            )
            document["proof_gaps"] = (
                json.loads(fake_proof_gaps)
                if fake_proof_gaps is not None
                else (
                    [
                        {
                            "claim": "synthetic selftest evidence",
                            "evidence_missing": "synthetic proof",
                            "how_to_prove": "resume with a fresh reviewer result",
                        }
                    ]
                    if verdict == "BLOCKED"
                    else []
                )
            )
            fake_probe_id = os.environ.get(
                "MURMUR_HARNESS_FAKE_REVIEW_PROBE_ID", ""
            ).strip()
            document["probe_requests"] = (
                [
                    {
                        "probe_id": fake_probe_id,
                        "rationale": os.environ.get(
                            "MURMUR_HARNESS_FAKE_REVIEW_PROBE_RATIONALE",
                            "selftest-only typed probe state transition",
                        ),
                    }
                ]
                if fake_probe_id
                else []
            )
        atomic_create_json(result_path, document)
        log_path.parent.mkdir(parents=True, exist_ok=True)
        with log_path.open("x", encoding="utf-8") as handle:
            handle.write(json.dumps({"type": "fake", "result": document}) + "\n")
            handle.flush()
            os.fsync(handle.fileno())
        process_result = {
            "exit_code": 0,
            "timed_out": False,
            "duration_ms": 0,
            "execution_id": invocation_id,
            "log": str(log_path),
            "log_sha256": sha256_file(log_path),
        }
    elif vendor == "codex":
        executable = shutil.which("codex")
        if not executable:
            raise HarnessError("codex executable not found")
        permission_profile = "murmur_harness_reviewer"
        workspace_access = "read"
        denied_host_paths = [
            "~/Desktop",
            "~/Documents",
            "~/Downloads",
            "~/Library",
            "~/.aws",
            "~/.claude",
            "~/.codex",
            "~/.config",
            "~/.cargo/credentials",
            "~/.cargo/credentials.toml",
            "~/.docker",
            "~/.git-credentials",
            "~/.gnupg",
            "~/.kube",
            "~/.netrc",
            "~/.npmrc",
            "~/.ssh",
        ]
        filesystem_entries = [
            '":root"="read"',
            '":tmpdir"="write"',
            '":slash_tmp"="write"',
            '":workspace_roots"={'
            + f'"."="{workspace_access}",'
            + '"**/.env"="deny","**/.env.*"="deny","**/*.p12"="deny",'
            + '"**/*.pem"="deny","**/id_ed25519"="deny","**/id_rsa"="deny"}',
            *(json.dumps(path) + '="deny"' for path in denied_host_paths),
        ]
        filesystem_profile = "{" + ",".join(filesystem_entries) + "}"
        guard_command = reviewer_tool_guard_command()
        reviewer_guard_config = (
            '[{matcher="*",hooks=[{type="command",command='
            + json.dumps(guard_command)
            + ',timeout=5,statusMessage="Blocking reviewer tool access"}]}]'
        )
        argv = [
            executable,
            "exec",
            "--ephemeral",
            "--ignore-user-config",
            "--strict-config",
            "--config",
            f"permissions.{permission_profile}.filesystem={filesystem_profile}",
            "--config",
            f"permissions.{permission_profile}.network.enabled=false",
            "--config",
            f'default_permissions="{permission_profile}"',
            "--cd",
            str(model_cwd),
            "--output-schema",
            str(schema_path),
            "--output-last-message",
            str(result_path),
            "--json",
        ]
        argv.extend(
            [
                "--skip-git-repo-check",
                "--ignore-rules",
                "--dangerously-bypass-hook-trust",
                "--enable",
                "hooks",
                "--config",
                f"hooks.PreToolUse={reviewer_guard_config}",
                "--config",
                'approval_policy="never"',
                "--config",
                'web_search="disabled"',
                "--config",
                "features.apps=false",
                "--config",
                "features.plugins=false",
                "--config",
                "features.multi_agent=false",
            ]
        )
        model_override = os.environ.get("MURMUR_HARNESS_CODEX_MODEL")
        if model_override:
            argv.extend(["--model", model_override])
        argv.append("-")
        environment, removed_env_names = sanitized_model_environment(instructions_sha256, vendor)
        environment = reviewer_model_environment(
            environment,
            vendor=vendor,
            cwd=model_cwd,
        )
        recorded_argv = list(argv)
        process_result = run_logged_process(
            argv,
            cwd=model_cwd,
            timeout_seconds=timeout_seconds,
            log_path=log_base_path,
            stdin_bytes=prompt.encode("utf-8"),
            env=environment,
            execution_id=invocation_id,
        )
        log_path = Path(str(process_result["log"]))
        _record_model_process_exit(
            task_dir,
            label=label,
            role=role,
            vendor=vendor,
            timeout_seconds=timeout_seconds,
            process_result=process_result,
            result_path=result_path,
            invocation_path=invocation_path,
            terminal_subtype=None,
        )
        if process_result["timed_out"]:
            raise ManagedProcessTimeout(
                f"Codex {label}",
                timeout_seconds,
                process_result,
            )
        if process_result["exit_code"] != 0:
            raise HarnessError(f"Codex {label} failed; inspect {log_path}")
        document = load_json(result_path)
    elif vendor == "claude":
        executable = shutil.which("claude")
        if not executable:
            raise HarnessError("claude executable not found")
        schema_text = json.dumps(schema_for_model_cli(schema, vendor), separators=(",", ":"))
        tools = ""
        permission_mode = "dontAsk"
        denied_read_paths = [
            "~/.ssh/**",
            "~/.aws/**",
            "~/.gnupg/**",
            "~/.codex/**",
            "~/.claude/**",
            "~/.config/gh/**",
            "~/.npmrc",
            "~/.cargo/credentials*",
            "~/Library/Keychains/**",
            "~/Library/Application Support/MeetNotes/**",
        ]
        sandbox_settings = json.dumps(
            {
                "permissions": {
                    "deny": [f"Read({path})" for path in denied_read_paths]
                    + ["Read(**/*.pem)", "Read(**/*.p12)", "Read(**/id_rsa)", "Read(**/id_ed25519)"]
                },
                "sandbox": {
                    "enabled": True,
                    "failIfUnavailable": True,
                    # Project settings enable sandboxed Bash for ordinary
                    # development sessions. A reviewer must not inherit that
                    # scalar: --allowedTools controls approval, not tool
                    # availability. The explicit --tools list below is the
                    # capability boundary; this setting closes the project
                    # auto-approval path as defense in depth.
                    "autoAllowBashIfSandboxed": False,
                    "allowUnsandboxedCommands": False,
                    "network": {"deniedDomains": ["*"]},
                    "filesystem": {"denyRead": denied_read_paths},
                }
            },
            separators=(",", ":"),
        )
        argv = [
            executable,
            "--print",
            "--output-format",
            "stream-json",
            "--verbose",
            "--no-session-persistence",
            "--strict-mcp-config",
            "--setting-sources",
            "project",
            "--settings",
            sandbox_settings,
            "--mcp-config",
            '{"mcpServers":{}}',
            "--tools",
            tools,
            "--allowedTools",
            tools,
            "--permission-mode",
            permission_mode,
            "--json-schema",
            schema_text,
        ]
        model_override = os.environ.get("MURMUR_HARNESS_CLAUDE_MODEL")
        if model_override:
            argv.extend(["--model", model_override])
        budget = os.environ.get("MURMUR_HARNESS_MAX_BUDGET_USD")
        if budget:
            argv.extend(["--max-budget-usd", budget])
        argv.append("-")
        environment, removed_env_names = sanitized_model_environment(instructions_sha256, vendor)
        environment = reviewer_model_environment(
            environment,
            vendor=vendor,
            cwd=model_cwd,
        )
        recorded_argv = list(argv)
        process_result = run_logged_process(
            argv,
            cwd=model_cwd,
            timeout_seconds=timeout_seconds,
            log_path=log_base_path,
            stdin_bytes=prompt.encode("utf-8"),
            env=environment,
            execution_id=invocation_id,
        )
        log_path = Path(str(process_result["log"]))
        # RECORD THE PROCESS OUTCOME BEFORE ANY BRANCH CAN RAISE.
        #
        # The `model-invocation` event below is appended only on the SUCCESS path, so every
        # failure was invisible in the event stream — 271 raw logs against 257 events, i.e.
        # 5.2% of invocations unaccounted for. That blind spot is why a timeout was
        # misdiagnosed as a permission rejection: with no recorded exit reason, the only
        # evidence left was the position of the last tool call in the log. Emitting here
        # makes every future failure self-diagnosing.
        terminal_subtype = _claude_terminal_subtype(log_path)
        _record_model_process_exit(
            task_dir,
            label=label,
            role=role,
            vendor=vendor,
            timeout_seconds=timeout_seconds,
            process_result=process_result,
            result_path=result_path,
            invocation_path=invocation_path,
            terminal_subtype=terminal_subtype,
        )
        if process_result["exit_code"] != 0 or process_result["timed_out"]:
            if process_result["timed_out"]:
                raise ManagedProcessTimeout(
                    f"Claude {label}",
                    timeout_seconds,
                    process_result,
                )
            else:
                raise HarnessError(f"Claude {label} failed; inspect {log_path}")
        else:
            document = _extract_claude_result(log_path)
        atomic_create_json(result_path, document)
    else:
        raise HarnessError(f"unsupported model adapter: {vendor}")

    if vendor != "fake":
        assert_reviewer_used_no_tools(log_path, vendor)
    validate_schema(document, schema, label=f"{label} result")
    metadata = extract_model_metadata(log_path, vendor, invocation_id)
    resolved_session = f"fake-{invocation_id}" if vendor == "fake" else metadata["session_id"]
    resolved_model = "fake" if vendor == "fake" else metadata["model"]
    resolved_cli_version = "fake" if vendor == "fake" else (command_version(vendor) or "unknown")
    invocation_created_at = utc_now()
    prompt_sha256 = sha256_bytes(prompt.encode("utf-8"))
    telemetry = {
        "total_cost_usd": metadata.get("total_cost_usd"),
        "num_turns": metadata.get("num_turns"),
        "usage": metadata.get("usage"),
        "terminal_reason": metadata.get("terminal_reason"),
        "http_status": metadata.get("http_status"),
        "retry_after_seconds": metadata.get("retry_after_seconds"),
    }
    atomic_create_json(
        invocation_path,
        {
            "invocation_id": invocation_id,
            "vendor": vendor,
            "role": role,
            "label": label,
            "argv": recorded_argv,
            "cwd": str(model_cwd),
            "wall_timeout_seconds": timeout_seconds,
            "process_duration_ms": process_result["duration_ms"],
            "process_timed_out": bool(process_result["timed_out"]),
            "budget_usd": budget,
            "removed_env_names": removed_env_names,
            "instructions_sha256": instructions_sha256,
            "prompt_sha256": prompt_sha256,
            "session_id": resolved_session,
            "model": resolved_model,
            "cli_version": resolved_cli_version,
            "telemetry": telemetry,
            "created_at": invocation_created_at,
        },
    )
    append_jsonl(
        task_dir / "events.jsonl",
        {
            "at": utc_now(),
            "event": "model-invocation",
            "label": label,
            "role": role,
            "vendor": vendor,
            "session_id": resolved_session,
            "exit_code": process_result["exit_code"],
            "timed_out": process_result["timed_out"],
            "duration_ms": process_result["duration_ms"],
            "total_cost_usd": telemetry["total_cost_usd"],
            "num_turns": telemetry["num_turns"],
            "usage": telemetry["usage"],
            "result_path": str(result_path),
        },
    )
    return {
        "vendor": vendor,
        "cli_version": resolved_cli_version,
        "model": resolved_model,
        "session_id": resolved_session,
        "role": role,
        "label": label,
        "result": document,
        "result_path": str(result_path),
        "artifact_sha256": sha256_file(result_path),
        "invocation_path": str(invocation_path),
        "invocation_sha256": sha256_file(invocation_path),
        "prompt_sha256": prompt_sha256,
        "telemetry": telemetry,
        "created_at": invocation_created_at,
        **process_result,
    }


def _publish_run_lock(lock: Path) -> TaskRunLock:
    """Publish a fully initialized inode with create-only hard-link semantics."""

    temporary = lock.parent / f".run.lock.{os.getpid()}.{uuid.uuid4().hex}.tmp"
    flags = os.O_RDWR | os.O_CREAT | os.O_EXCL
    if hasattr(os, "O_CLOEXEC"):
        flags |= os.O_CLOEXEC
    fd = os.open(temporary, flags, 0o600)
    handle = os.fdopen(fd, "r+b", buffering=0)
    try:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        owner = canonical_json(
            {
                "pid": os.getpid(),
                "protocol": 2,
                "started_at": utc_now(),
            }
        ) + b"\n"
        handle.write(owner)
        os.fsync(handle.fileno())
        os.link(temporary, lock, follow_symlinks=False)
        temporary.unlink()
        return TaskRunLock(lock, handle)
    except BaseException:
        handle.close()
        temporary.unlink(missing_ok=True)
        raise


def _remove_unowned_run_lock(lock: Path) -> bool:
    """Remove a stale inode only after acquiring its abandoned flock."""

    flags = os.O_RDWR
    if hasattr(os, "O_CLOEXEC"):
        flags |= os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        fd = os.open(lock, flags)
    except FileNotFoundError:
        return True
    except OSError as exc:
        raise HarnessError(f"refusing unsafe task lock: {lock}") from exc
    handle = os.fdopen(fd, "r+b", buffering=0)
    try:
        if not stat.S_ISREG(os.fstat(handle.fileno()).st_mode):
            raise HarnessError(f"task lock is not a regular file: {lock}")
        try:
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return False
        opened = os.fstat(handle.fileno())
        current = os.stat(lock, follow_symlinks=False)
        if (opened.st_dev, opened.st_ino) != (current.st_dev, current.st_ino):
            raise HarnessError("task lock inode changed during stale-owner recovery")
        lock.unlink()
        return True
    finally:
        handle.close()


def acquire_run_lock(task_dir: Path) -> TaskRunLock:
    lock = task_dir / "run.lock"
    for _attempt in range(3):
        try:
            return _publish_run_lock(lock)
        except FileExistsError:
            pass

        if lock.is_symlink():
            raise HarnessError(f"refusing symlink task lock: {lock}")
        if lock.is_dir():
            raise HarnessError(f"refusing non-file task lock: {lock}")
        if lock.exists():
            if _remove_unowned_run_lock(lock):
                continue
            raise HarnessError("task is already running (lock owner holds protocol-2 flock)")
        # The incumbent disappeared between the failed publish and inspection.
        continue
    raise HarnessError(f"could not acquire task lock: {lock}")


def release_run_lock(lock: TaskRunLock) -> None:
    try:
        lock.handle.seek(0)
        try:
            owner = json.loads(lock.handle.read().decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise HarnessError("refusing to release malformed task lock") from exc
        if int(owner.get("pid", -1)) != os.getpid():
            raise HarnessError(
                f"refusing to release task lock owned by another process: {lock.path}"
            )
        opened = os.fstat(lock.handle.fileno())
        current = os.stat(lock.path, follow_symlinks=False)
        if (opened.st_dev, opened.st_ino) != (current.st_dev, current.st_ino):
            raise HarnessError("refusing to release a replaced task lock inode")
        lock.path.unlink()
    except FileNotFoundError:
        pass
    finally:
        try:
            fcntl.flock(lock.handle.fileno(), fcntl.LOCK_UN)
        finally:
            lock.handle.close()


def prune_task_runtime(task_dir: Path) -> List[str]:
    """Remove disposable task caches while preserving attested sandbox profiles."""

    runtime_root = task_dir / "runtime"
    checks_root = runtime_root / "checks"
    removed: List[str] = []
    if not checks_root.is_dir() or checks_root.is_symlink():
        return removed
    for child in checks_root.iterdir():
        if child.name == "profiles":
            continue
        if child.is_symlink() or not child.is_dir():
            child.unlink(missing_ok=True)
        else:
            shutil.rmtree(child)
        removed.append(child.name)
    if removed:
        atomic_write_json(
            task_dir / "runtime-pruned.json",
            {
                "schema_version": 1,
                "removed": sorted(removed),
                "preserved": ["runtime/checks/profiles"],
                "pruned_at": utc_now(),
            },
        )
    return sorted(removed)


def parse_timestamp(raw: Any, label: str) -> dt.datetime:
    if not isinstance(raw, str):
        raise HarnessError(f"{label} is not a timestamp")
    try:
        parsed = dt.datetime.fromisoformat(raw.replace("Z", "+00:00"))
    except ValueError as exc:
        raise HarnessError(f"{label} is not an ISO date-time") from exc
    if parsed.tzinfo is None:
        raise HarnessError(f"{label} must include a timezone")
    return parsed.astimezone(dt.timezone.utc)


def _artifact_execution_id(path: Path, expected: Path) -> Optional[str]:
    """Return a UUID suffix for a dynamic artifact, or ``None`` for legacy exact paths."""

    if path == expected:
        return None
    if path.parent != expected.parent or path.suffix != expected.suffix:
        return None
    prefix = expected.stem + "-"
    if not path.stem.startswith(prefix):
        return None
    candidate = path.stem[len(prefix) :]
    try:
        parsed = uuid.UUID(candidate)
    except (AttributeError, TypeError, ValueError):
        return None
    if str(parsed) != candidate:
        return None
    return candidate


def evidence_file(task_dir: Path, raw: Any, expected: Path, label: str) -> Path:
    if not isinstance(raw, str) or not raw or "\x00" in raw:
        raise HarnessError(f"{label} path is malformed")
    path = Path(raw)
    execution_id = _artifact_execution_id(path, expected)
    canonical_expected = (
        _execution_artifact_path(expected, execution_id)
        if execution_id is not None
        else expected
    )
    if (
        not path.is_absolute()
        or (path != expected and execution_id is None)
        or path != canonical_expected
    ):
        raise HarnessError(f"{label} path is not the exact runner-owned artifact")
    try:
        resolved = path.resolve(strict=True)
        resolved.relative_to(task_dir.resolve())
        metadata = path.lstat()
    except (FileNotFoundError, OSError, ValueError) as exc:
        raise HarnessError(f"{label} is missing or outside the task evidence store") from exc
    if (
        resolved != canonical_expected.resolve()
        or not stat.S_ISREG(metadata.st_mode)
        or metadata.st_nlink != 1
    ):
        raise HarnessError(f"{label} must be a single-link regular file inside the task evidence store")
    return path


def verify_model_invocation(
    task_dir: Path,
    *,
    vendor: str,
    role: str,
    label: str,
    session_id: str,
    model: str,
    cli_version: str,
    invocation_path_raw: Any,
    invocation_sha256: Any,
    expected_path: Path,
    instructions_sha256: str,
    prompt_sha256: Optional[str] = None,
    require_cwd_binding: bool,
    expected_process_duration_ms: Optional[int] = None,
    expected_process_timed_out: Optional[bool] = None,
) -> dt.datetime:
    if role != "reviewer":
        raise HarnessError(f"{label} invocation role is not reviewer")
    invocation_path = evidence_file(task_dir, invocation_path_raw, expected_path, f"{label} invocation")
    if sha256_file(invocation_path) != invocation_sha256:
        raise HarnessError(f"{label} invocation metadata changed")
    invocation = load_json(invocation_path)
    for key, expected in (
        ("vendor", vendor),
        ("role", role),
        ("label", label),
        ("instructions_sha256", instructions_sha256),
        ("session_id", session_id),
        ("model", model),
        ("cli_version", cli_version),
    ):
        if invocation.get(key) != expected:
            raise HarnessError(f"{label} invocation {key} is not bound to the attested run")
    if prompt_sha256 is not None and invocation.get("prompt_sha256") != prompt_sha256:
        raise HarnessError(
            f"{label} invocation prompt_sha256 is not bound to the attested run"
        )
    invocation_id = invocation.get("invocation_id")
    if not isinstance(invocation_id, str) or not invocation_id:
        raise HarnessError(f"{label} invocation id is missing")
    path_execution_id = _artifact_execution_id(invocation_path, expected_path)
    if path_execution_id is not None and invocation_id != path_execution_id:
        raise HarnessError(
            f"{label} invocation id does not match its execution artifact path"
        )
    if vendor == "fake" and session_id != f"fake-{invocation_id}":
        raise HarnessError(f"{label} fake session is not bound to its invocation id")
    argv = invocation.get("argv")
    if not isinstance(argv, list) or not argv or not isinstance(argv[0], str):
        raise HarnessError(f"{label} invocation argv is missing")
    if Path(argv[0]).name != vendor:
        raise HarnessError(f"{label} invocation executable does not match vendor {vendor}")
    if "cwd" not in invocation:
        if require_cwd_binding:
            raise HarnessError(f"{label} invocation cwd is missing")
    else:
        invocation_cwd = invocation.get("cwd")
        if not isinstance(invocation_cwd, str) or not Path(invocation_cwd).is_absolute():
            raise HarnessError(f"{label} invocation cwd is not absolute")
        if Path(invocation_cwd) != reviewer_execution_cwd():
            raise HarnessError(f"{label} reviewer did not run from the isolated cwd")
    timeout = invocation.get("wall_timeout_seconds")
    if not isinstance(timeout, int) or isinstance(timeout, bool) or timeout < 1:
        raise HarnessError(f"{label} invocation timeout is invalid")
    removed = invocation.get("removed_env_names")
    if not isinstance(removed, list) or any(not isinstance(name, str) for name in removed):
        raise HarnessError(f"{label} invocation environment audit is malformed")
    if (
        expected_process_duration_ms is not None
        and invocation.get("process_duration_ms")
        != expected_process_duration_ms
    ):
        raise HarnessError(f"{label} invocation duration differs from process evidence")
    if (
        expected_process_timed_out is not None
        and invocation.get("process_timed_out")
        is not expected_process_timed_out
    ):
        raise HarnessError(f"{label} invocation timeout differs from process evidence")
    return parse_timestamp(invocation.get("created_at"), f"{label}.invocation.created_at")


def delete_local_task_branch(
    primary: Path,
    branch: str,
    expected_archive_sha: str,
    archive_ref: str,
) -> None:
    """Atomically delete only a branch whose current tip is preserved by its archive."""

    branch_ref = f"refs/heads/{branch}"
    current = git(primary, "show-ref", "--verify", "--hash", branch_ref, check=False)
    if not current:
        return
    archived = git(primary, "rev-parse", "--verify", archive_ref, check=False)
    if archived != expected_archive_sha:
        raise HarnessError("refusing to delete a task branch without its exact hidden archive")
    if (
        run_capture(
            ["git", "merge-base", "--is-ancestor", current, expected_archive_sha],
            primary,
            check=False,
        ).returncode
        != 0
    ):
        raise HarnessError(
            "refusing to delete a task branch that moved after its archive was created"
        )
    checked_out_marker = f"branch {branch_ref}"
    if checked_out_marker in git(primary, "worktree", "list", "--porcelain").splitlines():
        raise HarnessError("refusing to delete a task branch checked out by a worktree")
    # Supplying the observed old value closes the archive-check/delete race:
    # update-ref fails if another process advances the branch after our checks.
    git(primary, "update-ref", "-d", branch_ref, current)


def version_tuple(raw: str) -> Tuple[int, ...]:
    match = re.search(r"\d+(?:\.\d+)+", raw)
    if not match:
        return ()
    return tuple(int(part) for part in match.group(0).split("."))
