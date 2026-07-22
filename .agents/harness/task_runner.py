#!/usr/bin/env python3
"""Vendor-neutral, dependency-free development task loop for Murmur.

The runner deliberately keeps authority outside the model: writers may edit an
isolated worktree, while this process owns scope checks, deterministic commands,
fresh reviews, bounded repair, and the final hash-bound attestation.
"""

from __future__ import annotations

import argparse
import copy
import datetime as dt
import fnmatch
import hashlib
import json
import os
import pwd
import re
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


HARNESS_ROOT = Path(__file__).resolve().parent
SCHEMAS_DIR = HARNESS_ROOT / "schemas"
PROMPTS_DIR = HARNESS_ROOT / "prompts"
CONFIG_PATH = HARNESS_ROOT / "config.json"
TASK_ID_RE = re.compile(r"^[a-z0-9][a-z0-9._-]{1,63}$")
SHA1_RE = re.compile(r"^[0-9a-f]{40}$")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
TERMINAL_STATES = {"PASSED", "FAILED", "BLOCKED", "CLOSED"}


class HarnessError(RuntimeError):
    """An expected harness failure with a stable CLI exit code."""

    def __init__(self, message: str, exit_code: int = 2) -> None:
        super().__init__(message)
        self.exit_code = exit_code


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


def harness_store(common_dir: Path) -> Path:
    return common_dir / "agent-harness"


def task_dir_for(common_dir: Path, task_id: str) -> Path:
    return harness_store(common_dir) / "tasks" / task_id


def load_config() -> Dict[str, Any]:
    config = load_json(CONFIG_PATH)
    if not isinstance(config, dict) or config.get("schema_version") != 1:
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


def contract_hash(contract: Mapping[str, Any]) -> str:
    unsigned = copy.deepcopy(dict(contract))
    # The empty-field convention is shared with the commit guard.  Keeping the
    # key in the preimage also makes accidental omission a different contract.
    unsigned["contract_sha256"] = ""
    return sha256_bytes(canonical_json(unsigned))


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


def _owned_matches_pattern(owned: str, pattern: str) -> bool:
    if fnmatch.fnmatchcase(owned, pattern):
        return True
    wildcard_positions = [index for index in (pattern.find("*"), pattern.find("?"), pattern.find("[")) if index >= 0]
    static = pattern[: min(wildcard_positions)] if wildcard_positions else pattern
    static = static.rstrip("/")
    return bool(static) and path_overlaps(owned, static)


def classify_risks(
    owned_paths: Sequence[str], explicit: Sequence[str], config: Optional[Mapping[str, Any]] = None
) -> List[str]:
    risks = set(explicit)
    classification = (config or {}).get("risk_classification", {})
    for risk, patterns in classification.items():
        if any(_owned_matches_pattern(path, pattern) for path in owned_paths for pattern in patterns):
            risks.add(risk)
    order = ["lock", "egress", "protocol", "runtime", "ui", "release"]
    unknown = risks - set(order)
    if unknown:
        raise HarnessError(f"unknown risk flags: {', '.join(sorted(unknown))}")
    return [risk for risk in order if risk in risks]


def required_risk_evidence(risk_flags: Sequence[str], config: Mapping[str, Any]) -> List[str]:
    mapping = config.get("risk_required_evidence", {})
    result: List[str] = []
    for risk in risk_flags:
        for check_id in mapping.get(risk, []):
            if check_id not in result:
                result.append(check_id)
    return result


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
    for name in ("hook_guard.py", "config_audit.py", "eval_runner.py"):
        candidate = HARNESS_ROOT / name
        if candidate.is_file():
            harness_files.append(candidate)
    harness_files.extend(sorted(path for path in PROMPTS_DIR.rglob("*") if path.is_file()))
    harness_files.extend(sorted(path for path in SCHEMAS_DIR.rglob("*") if path.is_file()))
    for name in ("agent-harness", "agent-config-audit"):
        wrapper = source_repo / "scripts" / name
        if wrapper.is_file():
            harness_files.append(wrapper)
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


def dependency_revisions(repo_root: Path) -> Dict[str, str]:
    if not has_murmur_server_path_dependency(repo_root):
        return {}
    sibling = (repo_root / ".." / "murmur-server").resolve()
    revision_path = repo_root / ".murmur-server-revision"
    expected = revision_path.read_text(encoding="utf-8").strip() if revision_path.is_file() else "missing"
    if not sibling.is_dir():
        return {
            "murmur-server.expected": expected,
            "murmur-server.head": "missing",
            "murmur-server.dirty": "unknown",
        }
    head = git(sibling, "rev-parse", "HEAD", check=False) or "missing"
    dirty = "true" if git_bytes(sibling, "status", "--porcelain", check=False).strip() else "false"
    return {"murmur-server.expected": expected, "murmur-server.head": head, "murmur-server.dirty": dirty}


def validate_protocol_dependency(revisions: Mapping[str, str]) -> None:
    expected = revisions.get("murmur-server.expected", "")
    head = revisions.get("murmur-server.head", "")
    dirty = revisions.get("murmur-server.dirty", "unknown")
    if not SHA1_RE.fullmatch(expected) or head != expected or dirty != "false":
        raise HarnessError(
            "tasks with the murmur-server path dependency require a present, clean checkout at the exact pin; "
            f"expected={expected or 'missing'} observed={head or 'missing'} dirty={dirty}"
        )


def parse_check(raw: str, default_timeout: int) -> Dict[str, Any]:
    if "::" not in raw:
        raise HarnessError("checks must use id::command syntax")
    check_id, command = raw.split("::", 1)
    check_id = check_id.strip()
    command = command.strip()
    if not re.fullmatch(r"[a-z0-9][a-z0-9._-]*", check_id):
        raise HarnessError(f"invalid check id: {check_id!r}")
    if not command:
        raise HarnessError(f"check {check_id!r} has an empty command")
    return {"id": check_id, "command": command, "timeout_seconds": default_timeout}


def read_prompt(name: str) -> str:
    path = PROMPTS_DIR / f"{name}.md"
    try:
        return path.read_text(encoding="utf-8")
    except FileNotFoundError as exc:
        raise HarnessError(f"missing prompt template: {path}") from exc


def set_state(task_dir: Path, status: str, **details: Any) -> Dict[str, Any]:
    state_path = task_dir / "state.json"
    previous: Dict[str, Any] = {}
    if state_path.exists():
        previous = load_json(state_path)
    state = {
        "task_id": task_dir.name,
        "status": status,
        "updated_at": utc_now(),
        "round": details.pop("round", previous.get("round", 0)),
        **details,
    }
    atomic_write_json(state_path, state)
    append_jsonl(
        task_dir / "events.jsonl",
        {
            "at": state["updated_at"],
            "event": "state",
            "status": status,
            "round": state.get("round", 0),
            **{key: value for key, value in details.items() if key in {"phase", "reason"}},
        },
    )
    return state


def load_task_from_current_repo(task_id: str, cwd: Path) -> Tuple[Dict[str, Any], Path, Path]:
    if not TASK_ID_RE.fullmatch(task_id):
        raise HarnessError(f"invalid task id: {task_id!r}")
    _, common = repo_context(cwd)
    task_dir = task_dir_for(common, task_id)
    contract = load_json(task_dir / "task.json")
    validate_schema(contract, load_schema("task"), label="task contract")
    actual_hash = contract_hash(contract)
    if contract.get("contract_sha256") != actual_hash:
        raise HarnessError(
            f"task contract hash mismatch: expected {contract.get('contract_sha256')}, calculated {actual_hash}"
        )
    expected_common = Path(contract["git_common_dir"]).resolve()
    if expected_common != common.resolve():
        raise HarnessError("task belongs to a different Git common directory")
    return contract, task_dir, common


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


def stage_owned_paths(worktree: Path, contract: Mapping[str, Any]) -> Tuple[List[str], bytes]:
    if git(worktree, "rev-parse", "HEAD") != contract["base_sha"]:
        raise HarnessError("writer changed HEAD or created a commit; tasks must leave HEAD at base_sha")
    paths = changed_paths(worktree)
    violations = [path for path in paths if not path_is_owned(path, contract["owned_paths"])]
    if violations:
        raise HarnessError(f"out-of-scope changes: {', '.join(violations)}")
    unsafe_nodes = unsafe_changed_nodes(worktree, paths)
    if unsafe_nodes:
        raise HarnessError(
            "present changed paths must be single-link regular files and may not traverse symlinks: "
            + ", ".join(unsafe_nodes)
        )

    # Reset only the isolated worktree index, preserve every working-tree byte,
    # then stage the contract's paths explicitly (including deletions).
    git(worktree, "reset", "--quiet", "HEAD", "--", ".")
    if paths:
        git(worktree, "add", "-A", "--", *contract["owned_paths"])
    diff = staged_diff(worktree)
    if contract["expected_change"] and not diff:
        raise HarnessError("task requires a change, but the staged diff is empty")
    if not contract["expected_change"] and diff:
        raise HarnessError("task is declared no-change, but the writer modified files")
    return paths, diff


def _terminate_process(process: subprocess.Popen) -> None:
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except (ProcessLookupError, PermissionError):
        process.terminate()
    try:
        process.wait(timeout=3)
        return
    except subprocess.TimeoutExpired:
        pass
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        process.kill()
    process.wait()


def command_uses_cargo(command: str) -> bool:
    if re.search(r"(?:^|[\s;&|()])(?:[^\s;&|()]*/)?cargo(?:[\s;&|()]|$)", command):
        return True
    # These checked-in entrypoints invoke Cargo internally. They must share the
    # same lock even though the literal command line does not contain `cargo`.
    return any(
        marker in command
        for marker in (
            "scripts/ci.sh",
            "scripts/harness-runtime-smoke",
            "npm run dev",
            "npx tauri",
            "npm run tauri",
        )
    )


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


def command_needs_loopback(command: str) -> bool:
    """Return whether a deterministic check may use local TCP only."""

    return command_uses_playwright(command) or any(
        marker in command
        for marker in (
            "scripts/harness-runtime-smoke",
            "npm run dev",
            "npx tauri",
            "npm run tauri",
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
    }
    for path in paths.values():
        path.mkdir(parents=True, exist_ok=True)
    return paths


def build_check_environment(
    worktree: Path,
    task_dir: Path,
    *,
    cargo_target: Path,
    playwright_port: Optional[int],
) -> Tuple[Dict[str, str], Dict[str, Path]]:
    """Build a fixed allowlist environment for untrusted check commands."""

    runtime = _check_runtime_paths(task_dir)
    real_home = _real_user_home()
    cargo_home = (real_home / ".cargo").resolve()
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
        "TMPDIR": str(runtime["tmp"]) + os.sep,
        "XDG_CACHE_HOME": str(runtime["xdg_cache"]),
        "NPM_CONFIG_CACHE": str(runtime["npm_cache"]),
        "npm_config_cache": str(runtime["npm_cache"]),
        "CLANG_MODULE_CACHE_PATH": str(runtime["clang_cache"]),
        "CARGO_HOME": str(cargo_home),
        "RUSTUP_HOME": str(rustup_home),
        "CARGO_TARGET_DIR": str(cargo_target.resolve()),
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_CONFIG_NOSYSTEM": "1",
        "MISTRALRS_METAL_PRECOMPILE": "0",
        # Local verification must stay subordinate to the app the operator is
        # actively using.  These caps cover both Cargo's compiler fan-out and
        # the Rust/ML test process itself; the latter caused the observed
        # 1,100% CPU incident while a packaged Murmur recording was running.
        "CARGO_BUILD_JOBS": "2",
        "RUST_TEST_THREADS": "2",
        "RAYON_NUM_THREADS": "2",
        "TOKIO_WORKER_THREADS": "2",
        "NG_BUILD_MAX_WORKERS": "2",
        "MURMUR_HARNESS": "1",
        "MURMUR_HARNESS_TASK": task_dir.name,
        "MURMUR_HARNESS_INSTRUCTIONS_SHA256": instructions_hash(
            Path(load_json(task_dir / "task.json")["worktree_path"])
        ),
    }
    playwright_cache = real_home / "Library" / "Caches" / "ms-playwright"
    environment["PLAYWRIGHT_BROWSERS_PATH"] = str(playwright_cache.resolve())
    if playwright_port is not None:
        environment["MURMUR_E2E_PORT"] = str(playwright_port)
    return environment, runtime


def _seatbelt_literal(path: Path) -> str:
    return json.dumps(str(path.resolve()))


def build_check_seatbelt_profile(
    worktree: Path,
    task_dir: Path,
    *,
    cargo_target: Path,
    runtime: Mapping[str, Path],
    network_mode: str,
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
        Path("/sbin"),
        Path("/Library"),
        Path("/opt/homebrew"),
        Path("/usr/local"),
        Path("/private/etc"),
        Path("/private/var/db"),
        Path("/private/var/run"),
        # xcrun/xcode-select resolves the active Command Line Tools symlink
        # here before rustc can link a macOS target.
        Path("/private/var/select"),
        worktree.resolve(),
        common.resolve(),
        task_dir.resolve(),
        cargo_target.resolve(),
        (real_home / ".cargo").resolve(),
        (real_home / ".rustup").resolve(),
    }
    for optional in (
        real_home / ".bun" / "bin",
        real_home / ".local" / "bin",
        real_home / "Library" / "Caches" / "ms-playwright",
        worktree.parent / "murmur-server",
    ):
        read_paths.add(optional.resolve())
    modules = worktree / "node_modules"
    if modules.is_symlink():
        try:
            read_paths.add(modules.resolve(strict=True))
        except (FileNotFoundError, OSError) as exc:
            raise HarnessError("shared node_modules link became invalid before a check") from exc

    write_paths = {
        worktree.resolve(),
        task_dir.resolve(),
        cargo_target.resolve(),
        runtime["root"].resolve(),
        (task_dir.parent.parent / "runtime").resolve(),
        (real_home / ".cargo" / "registry").resolve(),
        (real_home / ".cargo" / "git").resolve(),
    }
    cargo_cache_files = [
        real_home / ".cargo" / ".package-cache",
        real_home / ".cargo" / ".package-cache-mutate",
        real_home / ".cargo" / ".global-cache",
    ]

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

    lines = [
        '(version 1)',
        '(deny default)',
        '(import "system.sb")',
        '(allow process-exec)',
        '(allow process-fork)',
        '(allow signal (target same-sandbox))',
        # canonicalize(3), git and rustc must be able to traverse the parents
        # of specifically allowed subpaths.  Grant metadata only for those
        # ancestors; file contents remain governed by the narrow rules below.
        '(allow file-read-metadata '
        + " ".join(
            f'(literal {_seatbelt_literal(path)})'
            for path in sorted(
                {
                    ancestor
                    for allowed in read_paths | write_paths
                    for ancestor in (allowed, *allowed.parents)
                }
            )
        )
        + ')',
        '(allow file-read* ' + " ".join(f'(subpath {_seatbelt_literal(path)})' for path in sorted(read_paths)) + ')',
        '(allow file-write* ' + " ".join(f'(subpath {_seatbelt_literal(path)})' for path in sorted(write_paths)) + ')',
    ]
    for cache_file in cargo_cache_files:
        lines.append(f'(allow file-write* (literal {_seatbelt_literal(cache_file)}))')
    for path in sensitive_paths:
        literal = _seatbelt_literal(path)
        lines.append(f'(deny file-read* (literal {literal}) (subpath {literal}))')
        lines.append(f'(deny file-write* (literal {literal}) (subpath {literal}))')
    if network_mode == "loopback":
        lines.extend(
            [
                '(allow network-bind (local ip "localhost:*"))',
                '(allow network-inbound (local ip "localhost:*"))',
                '(allow network-outbound (remote ip "localhost:*"))',
            ]
        )
    return "\n".join(lines) + "\n"


def sandboxed_check_argv(profile: str, command: str) -> List[str]:
    sandbox = Path("/usr/bin/sandbox-exec")
    if sys.platform != "darwin" or not sandbox.is_file() or not os.access(sandbox, os.X_OK):
        raise HarnessError("deterministic checks require executable macOS /usr/bin/sandbox-exec")
    return [str(sandbox), "-p", profile, "/bin/zsh", "-f", "-c", command]


def _pid_is_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
        return True
    except PermissionError:
        return True
    except ProcessLookupError:
        return False


def acquire_cargo_lock(task_dir: Path, timeout_seconds: int) -> Optional[Path]:
    """Serialize Cargo across worktrees, recovering only provably dead owners."""

    lock = task_dir.parent.parent / "cargo-lock"
    deadline = time.monotonic() + timeout_seconds
    while True:
        try:
            lock.mkdir()
            atomic_write_json(
                lock / "owner.json",
                {"pid": os.getpid(), "task_id": task_dir.name, "acquired_at": utc_now()},
            )
            return lock
        except FileExistsError:
            owner_path = lock / "owner.json"
            try:
                owner = load_json(owner_path)
                owner_pid = int(owner["pid"])
            except (HarnessError, KeyError, TypeError, ValueError):
                owner_pid = 0
            # Missing/corrupt metadata is not enough evidence to delete a lock.
            if owner_pid > 0 and not _pid_is_alive(owner_pid):
                try:
                    owner_path.unlink()
                    lock.rmdir()
                    continue
                except (FileNotFoundError, OSError):
                    pass
            if time.monotonic() >= deadline:
                raise HarnessError(
                    f"timed out waiting for machine-wide Cargo lock {lock}; owner pid={owner_pid or 'unknown'}"
                )
            time.sleep(0.25)


def release_cargo_lock(lock: Optional[Path]) -> None:
    if lock is None:
        return
    owner_path = lock / "owner.json"
    try:
        owner = load_json(owner_path)
        if int(owner.get("pid", -1)) != os.getpid():
            raise HarnessError(f"refusing to release Cargo lock owned by another process: {lock}")
        owner_path.unlink()
        lock.rmdir()
    except FileNotFoundError:
        pass


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


def acquire_playwright_port(task_dir: Path, timeout_seconds: int) -> Tuple[Path, int]:
    """Reserve a task-private loopback port across concurrent harness runs."""

    locks_root = task_dir.parent.parent / "playwright-ports"
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


def run_logged_process(
    argv: Sequence[str],
    *,
    cwd: Path,
    timeout_seconds: float,
    log_path: Path,
    stdin_bytes: Optional[bytes] = None,
    env: Optional[Mapping[str, str]] = None,
) -> Dict[str, Any]:
    log_path.parent.mkdir(parents=True, exist_ok=True)
    started = time.monotonic()
    timed_out = False
    with log_path.open("wb") as log_handle:
        process = subprocess.Popen(
            list(argv),
            cwd=str(cwd),
            env=dict(env) if env else None,
            stdin=subprocess.PIPE if stdin_bytes is not None else subprocess.DEVNULL,
            stdout=log_handle,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
        try:
            if stdin_bytes is not None:
                process.communicate(input=stdin_bytes, timeout=timeout_seconds)
            else:
                process.wait(timeout=timeout_seconds)
        except subprocess.TimeoutExpired:
            timed_out = True
            _terminate_process(process)
    duration_ms = int((time.monotonic() - started) * 1000)
    return {
        "exit_code": process.returncode,
        "timed_out": timed_out,
        "duration_ms": duration_ms,
        "log": str(log_path),
        "log_sha256": sha256_file(log_path),
    }


def run_check(worktree: Path, task_dir: Path, check: Mapping[str, Any], phase: str) -> Dict[str, Any]:
    safe_phase = re.sub(r"[^a-z0-9._-]+", "-", phase.lower())
    log_path = task_dir / "logs" / f"{safe_phase}-{check['id']}.log"
    stdout_path = task_dir / "logs" / f"{safe_phase}-{check['id']}.stdout.log"
    stderr_path = task_dir / "logs" / f"{safe_phase}-{check['id']}.stderr.log"
    primary, _ = repo_context(worktree)
    uses_shared_cargo_cache = command_uses_cargo(str(check["command"]))
    uses_playwright = command_uses_playwright(str(check["command"]))
    needs_loopback = command_needs_loopback(str(check["command"]))
    cargo_target = ((primary if uses_shared_cargo_cache else worktree) / "target").resolve()
    started = time.monotonic()
    started_at = utc_now()
    timed_out = False
    cargo_lock: Optional[Path] = None
    playwright_lock: Optional[Path] = None
    playwright_port: Optional[int] = None
    process: Optional[subprocess.Popen] = None
    try:
        if uses_shared_cargo_cache:
            cargo_lock = acquire_cargo_lock(task_dir, int(check["timeout_seconds"]))
        if uses_playwright:
            elapsed = time.monotonic() - started
            remaining_for_port = max(1, int(check["timeout_seconds"] - elapsed))
            playwright_lock, playwright_port = acquire_playwright_port(task_dir, remaining_for_port)
        environment, runtime = build_check_environment(
            worktree,
            task_dir,
            cargo_target=cargo_target,
            playwright_port=playwright_port,
        )
        network_mode = "loopback" if needs_loopback else "none"
        profile = build_check_seatbelt_profile(
            worktree,
            task_dir,
            cargo_target=cargo_target,
            runtime=runtime,
            network_mode=network_mode,
        )
        profile_path = runtime["profiles"] / f"{safe_phase}-{check['id']}.sb"
        atomic_write_bytes(profile_path, profile.encode("utf-8"))
        elapsed = time.monotonic() - started
        remaining = max(1, int(check["timeout_seconds"] - elapsed))
        stdout_path.parent.mkdir(parents=True, exist_ok=True)
        with stdout_path.open("wb") as stdout_handle, stderr_path.open("wb") as stderr_handle:
            process = subprocess.Popen(
                sandboxed_check_argv(profile, str(check["command"])),
                cwd=str(worktree),
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=stdout_handle,
                stderr=stderr_handle,
                start_new_session=True,
            )
            try:
                process.wait(timeout=remaining)
            except subprocess.TimeoutExpired:
                timed_out = True
                _terminate_process(process)
    finally:
        try:
            _release_owned_directory_lock(playwright_lock)
        finally:
            release_cargo_lock(cargo_lock)
    if process is None:
        raise HarnessError(f"check {check['id']} did not start")
    duration_ms = int((time.monotonic() - started) * 1000)
    with log_path.open("wb") as combined:
        combined.write(b"=== stdout ===\n")
        combined.write(stdout_path.read_bytes())
        combined.write(b"\n=== stderr ===\n")
        combined.write(stderr_path.read_bytes())
    result = {
        "exit_code": process.returncode,
        "timed_out": timed_out,
        "duration_ms": duration_ms,
        "log_path": str(log_path),
        "log_sha256": sha256_file(log_path),
        "stdout_sha256": sha256_file(stdout_path),
        "stderr_sha256": sha256_file(stderr_path),
        "sandbox_profile_path": str(profile_path),
        "sandbox_profile_sha256": sha256_file(profile_path),
        "environment_keys_sha256": sha256_bytes(canonical_json(sorted(environment))),
        "network_mode": network_mode,
        "started_at": started_at,
        "created_at": utc_now(),
    }
    evidence = {
        "id": check["id"],
        "command": check["command"],
        "phase": phase,
        **result,
        "passed": result["exit_code"] == 0 and not result["timed_out"],
    }
    append_jsonl(
        task_dir / "events.jsonl",
        {
            "at": utc_now(),
            "event": "check",
            "id": evidence["id"],
            "phase": phase,
            "exit_code": evidence["exit_code"],
            "timed_out": evidence["timed_out"],
            "duration_ms": evidence["duration_ms"],
            "passed": evidence["passed"],
            "log_path": evidence["log_path"],
            "network_mode": evidence["network_mode"],
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


def extract_model_metadata(log_path: Path, vendor: str, fallback_session: str) -> Dict[str, str]:
    """Best-effort extraction from vendor JSONL, with explicit non-empty fallbacks."""

    session_id = fallback_session
    model = "unspecified"
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
    except OSError:
        pass
    return {"session_id": session_id, "model": model}


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
    """Run one fresh model process. `role` is writer or reviewer."""

    schema_path = SCHEMAS_DIR / f"{schema_name}.schema.json"
    schema = load_schema(schema_name)
    log_path = task_dir / "logs" / f"{label}-{vendor}.jsonl"
    result_path = task_dir / "results" / f"{label}-{vendor}.json"
    invocation_path = task_dir / "results" / f"{label}-{vendor}-invocation.json"
    result_path.parent.mkdir(parents=True, exist_ok=True)
    invocation_id = str(uuid.uuid4())
    recorded_argv: List[str] = [vendor, "fake"]
    budget: Optional[str] = None
    removed_env_names: List[str] = []

    if vendor == "fake":
        verdict_override = os.environ.get(f"MURMUR_HARNESS_FAKE_{label.upper().replace('-', '_')}_VERDICT")
        if role == "writer":
            document: Dict[str, Any] = {
                "status": os.environ.get("MURMUR_HARNESS_FAKE_WRITER_STATUS", "completed"),
                "summary": "fake writer completed without invoking a model",
                "tests_run": [],
                "remaining_risks": [],
            }
        else:
            verdict = verdict_override or os.environ.get("MURMUR_HARNESS_FAKE_REVIEW_VERDICT", "PASS")
            findings: List[Dict[str, str]] = []
            if verdict != "PASS":
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
        atomic_write_json(result_path, document)
        log_path.parent.mkdir(parents=True, exist_ok=True)
        log_path.write_text(json.dumps({"type": "fake", "result": document}) + "\n", encoding="utf-8")
        process_result = {
            "exit_code": 0,
            "timed_out": False,
            "duration_ms": 0,
            "log": str(log_path),
            "log_sha256": sha256_file(log_path),
        }
    elif vendor == "codex":
        executable = shutil.which("codex")
        if not executable:
            raise HarnessError("codex executable not found")
        permission_profile = (
            "murmur_harness_writer" if role == "writer" else "murmur_harness_reviewer"
        )
        workspace_access = "write" if role == "writer" else "read"
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
            str(worktree),
            "--output-schema",
            str(schema_path),
            "--output-last-message",
            str(result_path),
            "--json",
        ]
        model_override = os.environ.get("MURMUR_HARNESS_CODEX_MODEL")
        if model_override:
            argv.extend(["--model", model_override])
        argv.append("-")
        environment, removed_env_names = sanitized_model_environment(instructions_sha256, vendor)
        recorded_argv = list(argv)
        process_result = run_logged_process(
            argv,
            cwd=worktree,
            timeout_seconds=timeout_seconds,
            log_path=log_path,
            stdin_bytes=prompt.encode("utf-8"),
            env=environment,
        )
        if process_result["exit_code"] != 0 or process_result["timed_out"]:
            raise HarnessError(f"Codex {label} failed; inspect {log_path}")
        document = load_json(result_path)
    elif vendor == "claude":
        executable = shutil.which("claude")
        if not executable:
            raise HarnessError("claude executable not found")
        schema_text = json.dumps(schema_for_model_cli(schema, vendor), separators=(",", ":"))
        if role == "writer":
            tools = "Read,Grep,Glob,Edit,Write,Bash"
        else:
            tools = "Read,Grep,Glob"
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
            "--allowedTools",
            tools,
            "--permission-mode",
            permission_mode,
            "--json-schema",
            schema_text,
        ]
        sibling_server = worktree.parent / "murmur-server"
        if role == "reviewer" and sibling_server.is_dir():
            argv.extend(["--add-dir", str(sibling_server)])
        model_override = os.environ.get("MURMUR_HARNESS_CLAUDE_MODEL")
        if model_override:
            argv.extend(["--model", model_override])
        budget = os.environ.get("MURMUR_HARNESS_MAX_BUDGET_USD")
        if budget:
            argv.extend(["--max-budget-usd", budget])
        argv.append("-")
        environment, removed_env_names = sanitized_model_environment(instructions_sha256, vendor)
        recorded_argv = list(argv)
        process_result = run_logged_process(
            argv,
            cwd=worktree,
            timeout_seconds=timeout_seconds,
            log_path=log_path,
            stdin_bytes=prompt.encode("utf-8"),
            env=environment,
        )
        if process_result["exit_code"] != 0 or process_result["timed_out"]:
            raise HarnessError(f"Claude {label} failed; inspect {log_path}")
        document = _extract_claude_result(log_path)
        atomic_write_json(result_path, document)
    else:
        raise HarnessError(f"unsupported model adapter: {vendor}")

    validate_schema(document, schema, label=f"{label} result")
    metadata = extract_model_metadata(log_path, vendor, invocation_id)
    invocation_created_at = utc_now()
    atomic_write_json(
        invocation_path,
        {
            "invocation_id": invocation_id,
            "vendor": vendor,
            "role": role,
            "label": label,
            "argv": recorded_argv,
            "wall_timeout_seconds": timeout_seconds,
            "budget_usd": budget,
            "removed_env_names": removed_env_names,
            "instructions_sha256": instructions_sha256,
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
            "session_id": f"fake-{invocation_id}" if vendor == "fake" else metadata["session_id"],
            "exit_code": process_result["exit_code"],
            "timed_out": process_result["timed_out"],
            "duration_ms": process_result["duration_ms"],
            "result_path": str(result_path),
        },
    )
    return {
        "vendor": vendor,
        "cli_version": "fake" if vendor == "fake" else (command_version(vendor) or "unknown"),
        "model": "fake" if vendor == "fake" else metadata["model"],
        "session_id": f"fake-{invocation_id}" if vendor == "fake" else metadata["session_id"],
        "role": role,
        "label": label,
        "result": document,
        "result_path": str(result_path),
        "invocation_path": str(invocation_path),
        "created_at": invocation_created_at,
        **process_result,
    }


def writer_prompt(contract: Mapping[str, Any], feedback: Sequence[Mapping[str, Any]]) -> str:
    sections = [
        read_prompt("implementer"),
        "\n## Task contract\n```json\n" + json.dumps(contract, indent=2, sort_keys=True) + "\n```",
    ]
    if feedback:
        sections.append("\n## Authoritative feedback from the previous round\n```json\n" + json.dumps(feedback, indent=2) + "\n```")
    return "\n".join(sections)


def review_prompt(
    review_name: str,
    contract: Mapping[str, Any],
    diff: bytes,
    checks: Sequence[Mapping[str, Any]],
) -> str:
    return "\n".join(
        [
            read_prompt(f"{review_name}-reviewer"),
            "\n## Immutable task contract\n```json\n" + json.dumps(contract, indent=2, sort_keys=True) + "\n```",
            "\n## Exact staged binary diff\n```diff\n" + diff.decode("utf-8", "replace") + "\n```",
            "\n## Deterministic check evidence\n```json\n" + json.dumps(list(checks), indent=2, sort_keys=True) + "\n```",
            "\nReturn only the review JSON. A PASS is invalid if any required evidence is missing.",
        ]
    )


def required_review_names(contract: Mapping[str, Any], config: Mapping[str, Any]) -> List[str]:
    names = list(config.get("required_reviews", ["spec", "adversarial"]))
    mapping = config.get("risk_reviews", {})
    for risk in contract["risk_flags"]:
        review = mapping.get(risk)
        if review and review not in names:
            names.append(review)
    return names


def assert_provenance(contract: Mapping[str, Any], task_dir: Path) -> None:
    disk_contract = load_json(task_dir / "task.json")
    if canonical_json(disk_contract) != canonical_json(contract):
        raise HarnessError("task contract changed while the loop was running")
    if contract_hash(disk_contract) != contract["contract_sha256"]:
        raise HarnessError("task contract hash changed while the loop was running")
    current_instructions = instructions_hash(Path(contract["worktree_path"]))
    if current_instructions != contract["instructions_sha256"]:
        raise HarnessError(
            "active agent instructions changed after init; create a new task contract "
            f"(expected {contract['instructions_sha256']}, current {current_instructions})"
        )
    current_dependencies = dependency_revisions(Path(contract["worktree_path"]))
    if current_dependencies != contract["dependency_revisions"]:
        raise HarnessError("dependency revisions changed after init; create a new task contract")
    if current_dependencies:
        validate_protocol_dependency(current_dependencies)


def acquire_run_lock(task_dir: Path) -> Path:
    lock = task_dir / "run.lock"
    for _attempt in range(2):
        try:
            lock.mkdir()
            atomic_write_json(lock / "owner.json", {"pid": os.getpid(), "started_at": utc_now()})
            return lock
        except FileExistsError as exc:
            try:
                owner = load_json(lock / "owner.json")
                owner_pid = int(owner["pid"])
            except (HarnessError, KeyError, TypeError, ValueError):
                owner_pid = 0
                owner = {"pid": "unknown"}
            if owner_pid > 0 and not _pid_is_alive(owner_pid):
                try:
                    (lock / "owner.json").unlink()
                    lock.rmdir()
                    continue
                except OSError:
                    pass
            raise HarnessError(f"task is already running (lock owner: {owner})") from exc
    raise HarnessError(f"could not acquire task lock: {lock}")


def release_run_lock(lock: Path) -> None:
    try:
        owner_path = lock / "owner.json"
        owner = load_json(owner_path)
        if int(owner.get("pid", -1)) != os.getpid():
            raise HarnessError(f"refusing to release task lock owned by another process: {lock}")
        owner_path.unlink()
        lock.rmdir()
    except FileNotFoundError:
        pass


def create_attestation(
    contract: Mapping[str, Any],
    worktree: Path,
    rounds: int,
    checks: Sequence[Mapping[str, Any]],
    reviews: Sequence[Mapping[str, Any]],
    writer_runs: Sequence[Mapping[str, Any]],
) -> Dict[str, Any]:
    diff = staged_diff(worktree)
    diff_sha = sha256_bytes(diff)
    head_sha = git(worktree, "rev-parse", "HEAD")
    tree_sha = git(worktree, "write-tree")
    latest_writer = writer_runs[-1]
    attested_checks = [
        {
            "id": check["id"],
            "command": check["command"],
            "phase": check["phase"],
            "exit_code": check["exit_code"],
            "duration_ms": check["duration_ms"],
            "log_sha256": check["log_sha256"],
            "stdout_sha256": check["stdout_sha256"],
            "stderr_sha256": check["stderr_sha256"],
            "log_path": check["log_path"],
            "sandbox_profile_path": check["sandbox_profile_path"],
            "sandbox_profile_sha256": check["sandbox_profile_sha256"],
            "environment_keys_sha256": check["environment_keys_sha256"],
            "network_mode": check["network_mode"],
            "started_at": check["started_at"],
            "created_at": check["created_at"],
        }
        for check in checks
    ]
    attested_reviews = [
        {
            "kind": review["kind"],
            "verdict": review["result"]["verdict"],
            "reviewer": {
                "vendor": review["vendor"],
                "cli_version": review["cli_version"],
                "model": review["model"],
                "session_id": review["session_id"],
            },
            "result_path": review["result_path"],
            "artifact_sha256": review["artifact_sha256"],
            "invocation_path": review["invocation_path"],
            "invocation_sha256": review["invocation_sha256"],
            "log_path": review["log"],
            "log_sha256": review["log_sha256"],
            "staged_diff_sha256": review["staged_diff_sha256"],
            "created_at": review["created_at"],
        }
        for review in reviews
    ]
    attestation = {
        "schema_version": 2,
        "task_id": contract["task_id"],
        "contract_sha256": contract["contract_sha256"],
        "instructions_sha256": contract["instructions_sha256"],
        "dependency_revisions": contract["dependency_revisions"],
        "base_sha": contract["base_sha"],
        "head_sha": head_sha,
        "tree_sha": tree_sha,
        "staged_diff_sha256": diff_sha,
        "repo_realpath": contract["repo_realpath"],
        "worktree_path": str(worktree.resolve()),
        "risk_flags": contract["risk_flags"],
        "writer": {
            "vendor": contract["writer"],
            "cli_version": latest_writer["cli_version"],
            "model": latest_writer["model"],
            "session_id": latest_writer["session_id"],
            "round": rounds,
            "label": latest_writer["label"],
            "result_path": latest_writer["result_path"],
            "artifact_sha256": sha256_file(Path(latest_writer["result_path"])),
            "invocation_path": latest_writer["invocation_path"],
            "invocation_sha256": sha256_file(Path(latest_writer["invocation_path"])),
            "log_path": latest_writer["log"],
            "log_sha256": latest_writer["log_sha256"],
            "created_at": latest_writer["created_at"],
        },
        "reviewer": {
            "vendor": contract["reviewer"],
            "cli_version": reviews[-1]["cli_version"],
            "model": reviews[-1]["model"],
        },
        "rounds": rounds,
        "checks": attested_checks,
        "reviews": attested_reviews,
        "verdict": "PASS",
        "created_at": utc_now(),
    }
    validate_schema(attestation, load_schema("attestation"), label="attestation")
    return attestation


def _review_evidence(model_run: Mapping[str, Any]) -> Dict[str, Any]:
    return {
        "name": model_run["label"],
        "vendor": model_run["vendor"],
        "cli_version": model_run["cli_version"],
        "model": model_run["model"],
        "session_id": model_run["session_id"],
        "result": model_run["result"],
        "result_path": model_run["result_path"],
        "artifact_sha256": sha256_file(Path(model_run["result_path"])),
        "invocation_path": model_run["invocation_path"],
        "invocation_sha256": sha256_file(Path(model_run["invocation_path"])),
        "log": model_run["log"],
        "log_sha256": model_run["log_sha256"],
        "duration_ms": model_run["duration_ms"],
        "exit_code": model_run["exit_code"],
    }


def run_task(contract: Dict[str, Any], task_dir: Path) -> str:
    config = load_config()
    worktree = Path(contract["worktree_path"])
    if not worktree.is_dir():
        raise HarnessError(f"task worktree is missing: {worktree}")
    if Path(git(worktree, "rev-parse", "--show-toplevel")).resolve() != worktree.resolve():
        raise HarnessError("task worktree path no longer identifies its Git root")
    assert_provenance(contract, task_dir)

    state_path = task_dir / "state.json"
    if state_path.exists():
        prior = load_json(state_path)
        if prior.get("status") == "PASSED":
            raise HarnessError("task already PASSED; verify its attestation instead of rerunning")

    lock = acquire_run_lock(task_dir)
    all_checks: List[Dict[str, Any]] = []
    final_reviews: List[Dict[str, Any]] = []
    writer_runs: List[Dict[str, Any]] = []
    feedback: List[Dict[str, Any]] = []
    max_repairs = int(contract["max_repair_rounds"])
    max_writer_rounds = 1 + max_repairs
    try:
        set_state(task_dir, "RUNNING", round=0, phase="writer")
        for round_number in range(1, max_writer_rounds + 1):
            set_state(task_dir, "RUNNING", round=round_number, phase="writer")
            writer = invoke_model(
                contract["writer"],
                role="writer",
                prompt=writer_prompt(contract, feedback),
                schema_name="model-result",
                worktree=worktree,
                task_dir=task_dir,
                label=f"round-{round_number:02d}-writer",
                timeout_seconds=int(config["agent_timeout_seconds"]),
                instructions_sha256=contract["instructions_sha256"],
            )
            writer_runs.append(writer)
            assert_provenance(contract, task_dir)
            if writer["result"]["status"] == "blocked":
                reason = writer["result"]["summary"]
                set_state(task_dir, "BLOCKED", round=round_number, phase="writer", reason=reason)
                return "BLOCKED"

            try:
                _, diff = stage_owned_paths(worktree, contract)
            except HarnessError as exc:
                set_state(task_dir, "FAILED", round=round_number, phase="scope", reason=str(exc))
                return "FAILED"
            atomic_write_bytes(task_dir / "diffs" / f"round-{round_number:02d}-writer.diff", diff)

            max_diff = int(config.get("max_diff_bytes_for_review", 500000))
            if len(diff) > max_diff:
                reason = f"staged diff is {len(diff)} bytes; review limit is {max_diff}"
                set_state(task_dir, "BLOCKED", round=round_number, phase="review", reason=reason)
                return "BLOCKED"

            set_state(task_dir, "CHECKING", round=round_number, phase="checks")
            round_checks = [run_check(worktree, task_dir, check, f"round-{round_number:02d}") for check in contract["checks"]]
            assert_provenance(contract, task_dir)
            all_checks.extend(round_checks)
            failed_checks = [check for check in round_checks if not check["passed"]]
            successful_ids = {check["id"] for check in round_checks if check["passed"]}
            missing_risk_evidence = [
                check_id for check_id in required_risk_evidence(contract["risk_flags"], config) if check_id not in successful_ids
            ]
            if missing_risk_evidence:
                failed_checks.append(
                    {
                        "id": "risk-evidence",
                        "passed": False,
                        "reason": "missing successful risk evidence: " + ", ".join(missing_risk_evidence),
                    }
                )
            try:
                _, after_check_diff = stage_owned_paths(worktree, contract)
            except HarnessError as exc:
                set_state(task_dir, "FAILED", round=round_number, phase="checks", reason=str(exc))
                return "FAILED"
            if after_check_diff != diff:
                failed_checks.append(
                    {
                        "id": "check-mutated-tree",
                        "passed": False,
                        "reason": "a deterministic check changed the staged diff",
                    }
                )
                diff = after_check_diff

            if failed_checks:
                feedback = [{"source": "deterministic-checks", "failures": failed_checks}]
                if round_number < max_writer_rounds:
                    set_state(task_dir, "REPAIRING", round=round_number, phase="checks", reason="required check failed")
                    continue
                set_state(task_dir, "FAILED", round=round_number, phase="checks", reason="required check failed")
                return "FAILED"

            atomic_write_bytes(task_dir / "diffs" / f"round-{round_number:02d}-reviewed.diff", diff)
            set_state(task_dir, "REVIEWING", round=round_number, phase="reviews")
            reviews_this_round: List[Dict[str, Any]] = []
            review_failed = False
            review_blocked = False
            for review_name in required_review_names(contract, config):
                before_review = workspace_fingerprint(worktree)
                model_review = invoke_model(
                    contract["reviewer"],
                    role="reviewer",
                    prompt=review_prompt(review_name, contract, diff, round_checks),
                    schema_name="review",
                    worktree=worktree,
                    task_dir=task_dir,
                    label=f"round-{round_number:02d}-{review_name}",
                    timeout_seconds=int(config["agent_timeout_seconds"]),
                    instructions_sha256=contract["instructions_sha256"],
                )
                evidence = _review_evidence(model_review)
                assert_provenance(contract, task_dir)
                evidence.update(
                    {
                        "kind": review_name,
                        "staged_diff_sha256": sha256_bytes(diff),
                        "created_at": utc_now(),
                    }
                )
                reviews_this_round.append(evidence)
                if staged_diff(worktree) != diff or workspace_fingerprint(worktree) != before_review:
                    raise HarnessError(f"{review_name} review changed the worktree; read-only review violated")
                verdict = model_review["result"]["verdict"]
                if verdict == "BLOCKED":
                    review_blocked = True
                    break
                if verdict == "FAIL":
                    review_failed = True
                    break

            if review_blocked:
                final_reviews = reviews_this_round
                set_state(task_dir, "BLOCKED", round=round_number, phase="reviews", reason="reviewer returned BLOCKED")
                return "BLOCKED"
            if review_failed:
                final_reviews = reviews_this_round
                feedback = [
                    {
                        "source": review["name"],
                        "verdict": review["result"]["verdict"],
                        "findings": review["result"]["findings"],
                    }
                    for review in reviews_this_round
                    if review["result"]["verdict"] != "PASS"
                ]
                if round_number < max_writer_rounds:
                    set_state(task_dir, "REPAIRING", round=round_number, phase="reviews", reason="review returned FAIL")
                    continue
                set_state(task_dir, "FAILED", round=round_number, phase="reviews", reason="review returned FAIL")
                return "FAILED"

            final_reviews = reviews_this_round
            reviewed_diff = diff
            set_state(task_dir, "CHECKING", round=round_number, phase="final-checks")
            final_checks = [run_check(worktree, task_dir, check, "final") for check in contract["final_checks"]]
            assert_provenance(contract, task_dir)
            all_checks.extend(final_checks)
            if any(not check["passed"] for check in final_checks):
                set_state(task_dir, "FAILED", round=round_number, phase="final-checks", reason="final check failed")
                return "FAILED"
            try:
                _, final_diff = stage_owned_paths(worktree, contract)
            except HarnessError as exc:
                set_state(task_dir, "FAILED", round=round_number, phase="final-checks", reason=str(exc))
                return "FAILED"
            if final_diff != reviewed_diff:
                reason = "final checks changed the reviewed staged diff; reviews are stale"
                set_state(task_dir, "FAILED", round=round_number, phase="final-checks", reason=reason)
                return "FAILED"

            assert_provenance(contract, task_dir)
            atomic_write_bytes(task_dir / "diffs" / "attested.diff", final_diff)

            attestation = create_attestation(
                contract,
                worktree,
                round_number,
                [*round_checks, *final_checks],
                final_reviews,
                writer_runs,
            )
            attestation_path = task_dir / "attestation.json"
            atomic_write_json(attestation_path, attestation)
            set_state(
                task_dir,
                "PASSED",
                round=round_number,
                phase="complete",
                attestation=str(attestation_path),
                staged_diff_sha256=attestation["staged_diff_sha256"],
                tree_sha=attestation["tree_sha"],
            )
            return "PASSED"

        raise HarnessError("internal error: task loop exhausted without a terminal result")
    except HarnessError as exc:
        current = load_json(task_dir / "state.json") if (task_dir / "state.json").exists() else {}
        if current.get("status") not in TERMINAL_STATES:
            set_state(task_dir, "BLOCKED", round=current.get("round", 0), phase="harness", reason=str(exc))
        raise
    finally:
        release_run_lock(lock)


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


def evidence_file(task_dir: Path, raw: Any, expected: Path, label: str) -> Path:
    if not isinstance(raw, str) or not raw or "\x00" in raw:
        raise HarnessError(f"{label} path is malformed")
    path = Path(raw)
    if not path.is_absolute() or path != expected:
        raise HarnessError(f"{label} path is not the exact runner-owned artifact")
    try:
        resolved = path.resolve(strict=True)
        resolved.relative_to(task_dir.resolve())
        metadata = path.lstat()
    except (FileNotFoundError, OSError, ValueError) as exc:
        raise HarnessError(f"{label} is missing or outside the task evidence store") from exc
    if resolved != expected.resolve() or not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
        raise HarnessError(f"{label} must be a single-link regular file inside the task evidence store")
    return path


def verify_model_invocation(
    task_dir: Path,
    *,
    vendor: str,
    role: str,
    label: str,
    session_id: str,
    invocation_path_raw: Any,
    invocation_sha256: Any,
    expected_path: Path,
    instructions_sha256: str,
) -> dt.datetime:
    invocation_path = evidence_file(task_dir, invocation_path_raw, expected_path, f"{label} invocation")
    if sha256_file(invocation_path) != invocation_sha256:
        raise HarnessError(f"{label} invocation metadata changed")
    invocation = load_json(invocation_path)
    for key, expected in (
        ("vendor", vendor),
        ("role", role),
        ("label", label),
        ("instructions_sha256", instructions_sha256),
    ):
        if invocation.get(key) != expected:
            raise HarnessError(f"{label} invocation {key} is not bound to the attested run")
    invocation_id = invocation.get("invocation_id")
    if not isinstance(invocation_id, str) or not invocation_id:
        raise HarnessError(f"{label} invocation id is missing")
    if vendor == "fake" and session_id != f"fake-{invocation_id}":
        raise HarnessError(f"{label} fake session is not bound to its invocation id")
    argv = invocation.get("argv")
    if not isinstance(argv, list) or not argv or not isinstance(argv[0], str):
        raise HarnessError(f"{label} invocation argv is missing")
    if Path(argv[0]).name != vendor:
        raise HarnessError(f"{label} invocation executable does not match vendor {vendor}")
    timeout = invocation.get("wall_timeout_seconds")
    if not isinstance(timeout, int) or isinstance(timeout, bool) or timeout < 1:
        raise HarnessError(f"{label} invocation timeout is invalid")
    removed = invocation.get("removed_env_names")
    if not isinstance(removed, list) or any(not isinstance(name, str) for name in removed):
        raise HarnessError(f"{label} invocation environment audit is malformed")
    return parse_timestamp(invocation.get("created_at"), f"{label}.invocation.created_at")


def verify_attestation(contract: Mapping[str, Any], task_dir: Path) -> Dict[str, Any]:
    """Canonical fail-closed verifier used by verify, guard, and commit."""

    validate_schema(dict(contract), load_schema("task"), label="task contract")
    attestation_path = task_dir / "attestation.json"
    attestation = load_json(attestation_path)
    validate_schema(attestation, load_schema("attestation"), label="attestation")
    worktree = Path(contract["worktree_path"])
    config = load_config()
    failures: List[str] = []

    def require(condition: bool, message: str) -> None:
        if not condition:
            failures.append(message)

    if contract_hash(contract) != contract.get("contract_sha256"):
        failures.append("task contract hash is stale")
    disk_contract = load_json(task_dir / "task.json")
    require(canonical_json(disk_contract) == canonical_json(contract), "task contract artifact changed")
    for key in (
        "task_id",
        "contract_sha256",
        "instructions_sha256",
        "dependency_revisions",
        "base_sha",
        "repo_realpath",
        "worktree_path",
        "risk_flags",
    ):
        expected = str(worktree.resolve()) if key == "worktree_path" else contract[key]
        require(attestation.get(key) == expected, f"{key} does not match the task contract")
    require(attestation.get("verdict") == "PASS", "attestation verdict is not PASS")
    rounds = attestation.get("rounds")
    require(
        isinstance(rounds, int)
        and not isinstance(rounds, bool)
        and 1 <= rounds <= int(contract["max_repair_rounds"]) + 1,
        "attestation rounds exceed the bounded repair loop",
    )

    try:
        task_created = parse_timestamp(contract.get("created_at"), "task.created_at")
        receipt_created = parse_timestamp(attestation.get("created_at"), "attestation.created_at")
        now = dt.datetime.now(dt.timezone.utc)
        require(task_created <= receipt_created <= now + dt.timedelta(minutes=5), "attestation timestamp is stale or future")
    except HarnessError as exc:
        failures.append(str(exc))
        task_created = dt.datetime.min.replace(tzinfo=dt.timezone.utc)
        receipt_created = dt.datetime.max.replace(tzinfo=dt.timezone.utc)

    if not worktree.is_dir() or worktree.is_symlink():
        failures.append("task worktree is missing or unsafe")
    else:
        try:
            primary, common = repo_context(worktree)
            require(primary == Path(contract["repo_realpath"]).resolve(), "primary repository provenance changed")
            require(common == Path(contract["git_common_dir"]).resolve(), "Git common directory provenance changed")
            require(Path(git(worktree, "rev-parse", "--show-toplevel")).resolve() == worktree.resolve(), "worktree Git root changed")
            require(git(worktree, "branch", "--show-current") == contract["branch"], "task branch changed")
            current_head = git(worktree, "rev-parse", "HEAD")
            require(current_head == contract["base_sha"], "HEAD no longer equals task base before commit")
            require(current_head == attestation["head_sha"], "HEAD changed after attestation")
            current_tree = git(worktree, "write-tree")
            current_diff = staged_diff(worktree)
            require(current_tree == attestation["tree_sha"], "index tree changed after attestation")
            require(sha256_bytes(current_diff) == attestation["staged_diff_sha256"], "staged diff changed after attestation")
            current_paths = changed_paths(worktree)
            violations = [path for path in current_paths if not path_is_owned(path, contract["owned_paths"])]
            require(not violations, f"out-of-scope paths now changed: {', '.join(violations)}")
            unsafe_nodes = unsafe_changed_nodes(worktree, current_paths)
            require(not unsafe_nodes, f"unsafe changed nodes exist: {', '.join(unsafe_nodes)}")
            require(bool(current_diff) == bool(contract["expected_change"]), "staged change presence differs from task contract")
            require(
                not git_bytes(worktree, "diff", "--binary", "--no-ext-diff", "--").strip(),
                "unstaged tracked changes exist after attestation",
            )
            require(not untracked_paths(worktree), "untracked files exist after attestation")
        except HarnessError as exc:
            failures.append(str(exc))

    require(instructions_hash(worktree) == contract["instructions_sha256"], "active instructions changed after attestation")
    require(dependency_revisions(worktree) == contract["dependency_revisions"], "dependency revisions changed after attestation")
    inferred_risks = classify_risks(contract["owned_paths"], [], config)
    require(set(inferred_risks).issubset(set(contract["risk_flags"])), "task omitted automatically classified risk flags")

    try:
        diff_path = evidence_file(
            task_dir,
            str(task_dir / "diffs" / "attested.diff"),
            task_dir / "diffs" / "attested.diff",
            "attested diff",
        )
        require(sha256_file(diff_path) == attestation["staged_diff_sha256"], "attested diff artifact changed")
    except HarnessError as exc:
        failures.append(str(exc))

    writer = attestation.get("writer", {})
    writer_time = task_created
    writer_session = writer.get("session_id") if isinstance(writer, dict) else None
    writer_vendor = writer.get("vendor") if isinstance(writer, dict) else None
    writer_label = f"round-{int(rounds or 0):02d}-writer"
    require(writer_vendor == contract["writer"], "writer vendor does not match task contract")
    require(writer.get("round") == rounds, "writer round does not match attestation rounds")
    require(writer.get("label") == writer_label, "writer label does not match the final round")
    require(isinstance(writer_session, str) and bool(writer_session), "writer session provenance is empty")
    for field in ("cli_version", "model"):
        require(isinstance(writer.get(field), str) and bool(writer.get(field)), f"writer {field} provenance is empty")
    if isinstance(writer_session, str) and isinstance(writer_vendor, str):
        writer_result_expected = task_dir / "results" / f"{writer_label}-{writer_vendor}.json"
        writer_invocation_expected = task_dir / "results" / f"{writer_label}-{writer_vendor}-invocation.json"
        writer_log_expected = task_dir / "logs" / f"{writer_label}-{writer_vendor}.jsonl"
        try:
            writer_result_path = evidence_file(task_dir, writer.get("result_path"), writer_result_expected, "writer result")
            require(sha256_file(writer_result_path) == writer.get("artifact_sha256"), "writer result artifact changed")
            writer_result = load_json(writer_result_path)
            validate_schema(writer_result, load_schema("model-result"), label="writer result")
            require(writer_result.get("status") == "completed", "attested writer did not complete")
            invocation_time = verify_model_invocation(
                task_dir,
                vendor=writer_vendor,
                role="writer",
                label=writer_label,
                session_id=writer_session,
                invocation_path_raw=writer.get("invocation_path"),
                invocation_sha256=writer.get("invocation_sha256"),
                expected_path=writer_invocation_expected,
                instructions_sha256=contract["instructions_sha256"],
            )
            writer_time = parse_timestamp(writer.get("created_at"), "writer.created_at")
            require(writer_time == invocation_time, "writer timestamp differs from invocation metadata")
            writer_log = evidence_file(task_dir, writer.get("log_path"), writer_log_expected, "writer log")
            require(sha256_file(writer_log) == writer.get("log_sha256"), "writer log changed")
            require(task_created <= writer_time <= receipt_created, "writer timestamp is outside the task lifetime")
        except HarnessError as exc:
            failures.append(str(exc))

    declared_checks = list(contract.get("checks", [])) + list(contract.get("final_checks", []))
    recorded_checks = attestation.get("checks", [])
    require(len(recorded_checks) == len(declared_checks), "attestation check set differs from the exact contract")
    successful_ids: List[str] = []
    for index, declared in enumerate(declared_checks):
        if index >= len(recorded_checks):
            break
        check = recorded_checks[index]
        check_id = declared["id"]
        phase = f"round-{int(rounds or 0):02d}" if index < len(contract.get("checks", [])) else "final"
        require(check.get("id") == check_id, f"check order/id differs from contract at {check_id}")
        require(check.get("command") == declared["command"], f"attested command differs for check {check_id}")
        require(check.get("phase") == phase, f"attested phase differs for check {check_id}")
        require(check.get("exit_code") == 0, f"attested check is not green: {check_id}")
        safe_phase = re.sub(r"[^a-z0-9._-]+", "-", phase.lower())
        expected_log = task_dir / "logs" / f"{safe_phase}-{check_id}.log"
        expected_profile = task_dir / "runtime" / "checks" / "profiles" / f"{safe_phase}-{check_id}.sb"
        try:
            log_path = evidence_file(task_dir, check.get("log_path"), expected_log, f"check {check_id} log")
            stdout_path = evidence_file(
                task_dir,
                str(expected_log.with_suffix(".stdout.log")),
                expected_log.with_suffix(".stdout.log"),
                f"check {check_id} stdout",
            )
            stderr_path = evidence_file(
                task_dir,
                str(expected_log.with_suffix(".stderr.log")),
                expected_log.with_suffix(".stderr.log"),
                f"check {check_id} stderr",
            )
            profile_path = evidence_file(
                task_dir,
                check.get("sandbox_profile_path"),
                expected_profile,
                f"check {check_id} sandbox profile",
            )
            require(sha256_file(log_path) == check.get("log_sha256"), f"check {check_id} combined log changed")
            require(sha256_file(stdout_path) == check.get("stdout_sha256"), f"check {check_id} stdout changed")
            require(sha256_file(stderr_path) == check.get("stderr_sha256"), f"check {check_id} stderr changed")
            combined = b"=== stdout ===\n" + stdout_path.read_bytes() + b"\n=== stderr ===\n" + stderr_path.read_bytes()
            require(log_path.read_bytes() == combined, f"check {check_id} combined log is not canonical")
            require(sha256_file(profile_path) == check.get("sandbox_profile_sha256"), f"check {check_id} sandbox profile changed")
            uses_cargo = command_uses_cargo(declared["command"])
            cargo_target = (((Path(contract["repo_realpath"]) if uses_cargo else worktree) / "target").resolve())
            expected_network = "loopback" if command_needs_loopback(declared["command"]) else "none"
            require(check.get("network_mode") == expected_network, f"check {check_id} network policy changed")
            env, runtime = build_check_environment(
                worktree,
                task_dir,
                cargo_target=cargo_target,
                playwright_port=42000 if command_uses_playwright(declared["command"]) else None,
            )
            require(
                check.get("environment_keys_sha256") == sha256_bytes(canonical_json(sorted(env))),
                f"check {check_id} environment allowlist changed",
            )
            expected_profile_text = build_check_seatbelt_profile(
                worktree,
                task_dir,
                cargo_target=cargo_target,
                runtime=runtime,
                network_mode=expected_network,
            )
            require(
                sha256_bytes(expected_profile_text.encode("utf-8")) == check.get("sandbox_profile_sha256"),
                f"check {check_id} was not run under the current canonical sandbox",
            )
            started = parse_timestamp(check.get("started_at"), f"check[{check_id}].started_at")
            finished = parse_timestamp(check.get("created_at"), f"check[{check_id}].created_at")
            require(task_created <= started <= finished <= receipt_created, f"check {check_id} timestamp is outside task lifetime")
        except HarnessError as exc:
            failures.append(str(exc))
        successful_ids.append(check_id)
    require(len(set(successful_ids)) == len(successful_ids), "attestation check ids are not unique")
    for check_id in required_risk_evidence(contract["risk_flags"], config):
        require(check_id in successful_ids, f"required risk evidence absent: {check_id}")

    expected_reviews = required_review_names(contract, config)
    reviews = attestation.get("reviews", [])
    require([review.get("kind") for review in reviews] == expected_reviews, "attestation review set/order differs from requirements")
    reviewer_sessions: set = set()
    for review in reviews:
        kind = review.get("kind", "unknown")
        reviewer = review.get("reviewer", {})
        vendor = reviewer.get("vendor") if isinstance(reviewer, dict) else None
        session = reviewer.get("session_id") if isinstance(reviewer, dict) else None
        label = f"round-{int(rounds or 0):02d}-{kind}"
        require(review.get("verdict") == "PASS", f"review is not PASS: {kind}")
        require(review.get("staged_diff_sha256") == attestation["staged_diff_sha256"], f"review is bound to another diff: {kind}")
        require(vendor == contract["reviewer"], f"review {kind} vendor does not match contract")
        require(isinstance(session, str) and bool(session), f"review {kind} session provenance is empty")
        require(session != writer_session, f"review {kind} reused the writer session")
        require(session not in reviewer_sessions, f"review {kind} reused another reviewer session")
        if isinstance(session, str):
            reviewer_sessions.add(session)
        for field in ("cli_version", "model"):
            require(isinstance(reviewer.get(field), str) and bool(reviewer.get(field)), f"review {kind} {field} provenance is empty")
        if isinstance(vendor, str) and isinstance(session, str):
            result_expected = task_dir / "results" / f"{label}-{vendor}.json"
            invocation_expected = task_dir / "results" / f"{label}-{vendor}-invocation.json"
            log_expected = task_dir / "logs" / f"{label}-{vendor}.jsonl"
            try:
                result_path = evidence_file(task_dir, review.get("result_path"), result_expected, f"review {kind} result")
                require(sha256_file(result_path) == review.get("artifact_sha256"), f"review {kind} artifact changed")
                result = load_json(result_path)
                validate_schema(result, load_schema("review"), label=f"review {kind} result")
                require(result.get("verdict") == "PASS", f"review {kind} result artifact is not PASS")
                invocation_time = verify_model_invocation(
                    task_dir,
                    vendor=vendor,
                    role="reviewer",
                    label=label,
                    session_id=session,
                    invocation_path_raw=review.get("invocation_path"),
                    invocation_sha256=review.get("invocation_sha256"),
                    expected_path=invocation_expected,
                    instructions_sha256=contract["instructions_sha256"],
                )
                log_path = evidence_file(task_dir, review.get("log_path"), log_expected, f"review {kind} log")
                require(sha256_file(log_path) == review.get("log_sha256"), f"review {kind} log changed")
                review_time = parse_timestamp(review.get("created_at"), f"review[{kind}].created_at")
                require(task_created <= invocation_time <= review_time <= receipt_created, f"review {kind} timestamp is outside task lifetime")
                require(writer_time <= review_time, f"review {kind} predates the final writer")
            except HarnessError as exc:
                failures.append(str(exc))
    top_reviewer = attestation.get("reviewer", {})
    require(top_reviewer.get("vendor") == contract["reviewer"], "summary reviewer vendor differs from contract")
    if reviews:
        last_reviewer = reviews[-1].get("reviewer", {})
        require(top_reviewer.get("cli_version") == last_reviewer.get("cli_version"), "summary reviewer cli version is stale")
        require(top_reviewer.get("model") == last_reviewer.get("model"), "summary reviewer model is stale")

    try:
        state = load_json(task_dir / "state.json")
        require(state.get("status") == "PASSED", "task state is not PASSED")
        require(state.get("task_id") == contract["task_id"], "task state id differs from contract")
        require(state.get("staged_diff_sha256") == attestation["staged_diff_sha256"], "task state diff hash is stale")
        require(state.get("tree_sha") == attestation["tree_sha"], "task state tree hash is stale")
    except HarnessError as exc:
        failures.append(str(exc))

    if failures:
        raise HarnessError("attestation verification failed:\n- " + "\n- ".join(failures), exit_code=1)
    return attestation


def cmd_init(args: argparse.Namespace) -> int:
    cwd = Path.cwd()
    primary, common = repo_context(cwd)
    if not TASK_ID_RE.fullmatch(args.task_id):
        raise HarnessError("task id must match [a-z0-9][a-z0-9._-]{1,63}")
    config = load_config()
    task_dir = task_dir_for(common, args.task_id)
    if task_dir.exists():
        raise HarnessError(f"task already exists: {args.task_id}")

    owned = sorted(set(normalize_owned_path(path) for path in args.owned))
    protected = [normalize_owned_path(path) for path in config.get("protected_paths", [])]
    overlaps = sorted({path for path in owned if any(path_overlaps(path, guard) for guard in protected)})
    if overlaps:
        raise HarnessError(f"owned paths overlap protected harness/guardrail paths: {', '.join(overlaps)}")

    base_sha = git(cwd, "rev-parse", "--verify", "--end-of-options", f"{args.base or 'HEAD'}^{{commit}}")
    if not SHA1_RE.fullmatch(base_sha):
        raise HarnessError(f"invalid base commit: {base_sha}")
    branch = args.branch or f"agent/{args.task_id}"
    comparable_branch = branch[len("refs/heads/") :] if branch.startswith("refs/heads/") else branch
    if comparable_branch in {"murmur", "main", "master"}:
        raise HarnessError(f"task worktrees may not use a protected branch: {branch}")
    if run_capture(["git", "check-ref-format", "--branch", branch], cwd, check=False).returncode != 0:
        raise HarnessError(f"invalid task branch: {branch!r}")
    exists = run_capture(["git", "show-ref", "--verify", "--quiet", f"refs/heads/{branch}"], cwd, check=False)
    if exists.returncode == 0:
        raise HarnessError(f"branch already exists: {branch}")
    # Mirror the production sibling layout inside a task-specific root. The app
    # path dependency `src-tauri/../../murmur-server` then resolves to a clean,
    # pinned server worktree rather than the operator's mutable sibling checkout.
    task_root = primary.parent / ".murmur-agent-tasks" / args.task_id
    worktree = task_root / primary.name
    server_worktree = task_root / "murmur-server"
    if task_root.exists():
        raise HarnessError(f"task worktree root already exists: {task_root}")

    writer = args.agent
    reviewer = args.reviewer or ({"codex": "claude", "claude": "codex", "fake": "fake"}[writer])
    timeout = int(config.get("check_timeout_seconds", 1800))
    checks = [parse_check(raw, timeout) for raw in args.check]
    final_checks = [parse_check(raw, timeout) for raw in args.final_check]
    risks = classify_risks(owned, args.risk, config)
    check_ids = {check["id"] for check in checks}
    if "ui" in risks and "playwright" not in check_ids:
        checks.append(parse_check("playwright::npm run test:e2e -- --workers=1", timeout))
        check_ids.add("playwright")
    if "runtime" in risks and "tauri-boot" not in check_ids:
        runtime_smoke = primary / "scripts" / "harness-runtime-smoke"
        if runtime_smoke.is_file():
            checks.append(parse_check("tauri-boot::scripts/harness-runtime-smoke", timeout))
            check_ids.add("tauri-boot")
    missing_evidence = [check_id for check_id in required_risk_evidence(risks, config) if check_id not in check_ids]
    if missing_evidence:
        raise HarnessError(
            "risk classification requires explicit deterministic evidence checks: "
            + ", ".join(missing_evidence)
            + ". Add each with --check 'id::command'."
        )
    if not checks:
        raise HarnessError(
            "every task requires at least one deterministic pre-review check; "
            "add --check 'id::command'"
        )
    all_check_ids = [check["id"] for check in [*checks, *final_checks]]
    duplicates = sorted({check_id for check_id in all_check_ids if all_check_ids.count(check_id) > 1})
    if duplicates:
        raise HarnessError(f"check ids must be unique across checks and final-checks: {', '.join(duplicates)}")
    created_at = utc_now()
    contract: Dict[str, Any] = {
        "schema_version": 1,
        "task_id": args.task_id,
        "description": args.prompt,
        "kind": args.kind,
        "base_sha": base_sha,
        "contract_sha256": "",
        "instructions_sha256": "0" * 64,
        "dependency_revisions": {},
        "repo_realpath": str(primary.resolve()),
        "git_common_dir": str(common.resolve()),
        "worktree_path": str(worktree.resolve()),
        "branch": branch,
        "owned_paths": owned,
        "risk_flags": risks,
        "writer": writer,
        "reviewer": reviewer,
        "max_repair_rounds": args.max_repair_rounds,
        "checks": checks,
        "final_checks": final_checks,
        "expected_change": args.expected_change,
        "created_at": created_at,
    }
    task_dir.mkdir(parents=True)
    task_root.mkdir(parents=True)
    server_source: Optional[Path] = None
    try:
        run_capture(["git", "worktree", "add", "-b", branch, str(worktree), base_sha], primary)
        if has_murmur_server_path_dependency(worktree):
            revision_path = worktree / ".murmur-server-revision"
            try:
                pinned_server_sha = revision_path.read_text(encoding="utf-8").strip()
            except FileNotFoundError as exc:
                raise HarnessError(
                    "the committed base uses murmur-server but has no .murmur-server-revision pin"
                ) from exc
            if not SHA1_RE.fullmatch(pinned_server_sha):
                raise HarnessError(".murmur-server-revision must contain exactly one 40-character commit SHA")
            server_source = primary.parent / "murmur-server"
            if not server_source.is_dir():
                raise HarnessError(f"pinned murmur-server checkout is unavailable: {server_source}")
            resolved_server_sha = git(
                server_source,
                "rev-parse",
                "--verify",
                "--end-of-options",
                f"{pinned_server_sha}^{{commit}}",
            )
            if resolved_server_sha != pinned_server_sha:
                raise HarnessError("murmur-server revision pin did not resolve to the exact requested commit")
            run_capture(
                ["git", "worktree", "add", "--detach", str(server_worktree), pinned_server_sha],
                server_source,
            )
        unsafe_owned = [path for path in owned if path_has_symlink_component(worktree, path)]
        if unsafe_owned:
            raise HarnessError(
                "owned paths may not traverse symlinks in the base tree: " + ", ".join(unsafe_owned)
            )
        node_modules_source = primary / "node_modules"
        node_modules_link = worktree / "node_modules"
        shared_node_modules: Optional[str] = None
        if node_modules_source.is_dir() and not node_modules_source.is_symlink():
            ignored = run_capture(
                ["git", "check-ignore", "--quiet", "--no-index", "--", "node_modules/"], worktree, check=False
            )
            if ignored.returncode != 0:
                raise HarnessError("refusing to share node_modules because the worktree does not ignore it")
            if node_modules_link.exists() or node_modules_link.is_symlink():
                raise HarnessError(f"refusing to replace existing worktree path: {node_modules_link}")
            os.symlink(str(node_modules_source.resolve()), str(node_modules_link), target_is_directory=True)
            shared_node_modules = str(node_modules_source.resolve())
        contract["instructions_sha256"] = instructions_hash(worktree)
        contract["dependency_revisions"] = dependency_revisions(worktree)
        if contract["dependency_revisions"]:
            validate_protocol_dependency(contract["dependency_revisions"])
        contract["contract_sha256"] = contract_hash(contract)
        validate_schema(contract, load_schema("task"), label="task contract")
        atomic_write_json(task_dir / "task.json", contract)
        atomic_write_json(
            task_dir / "runtime.json",
            {
                "shared_node_modules": shared_node_modules,
                "cargo_target_dir": str((primary / "target").resolve()),
                "worktree_layout": "mirrored-siblings",
                "task_root": str(task_root),
                "server_worktree": str(server_worktree) if server_worktree.is_dir() else None,
                "server_source": str(server_source) if server_source else None,
            },
        )
        set_state(task_dir, "INITIALIZED", round=0, phase="init")
    except Exception:
        if server_worktree.exists() and server_source is not None:
            run_capture(["git", "worktree", "remove", "--force", str(server_worktree)], server_source, check=False)
        if worktree.exists():
            run_capture(["git", "worktree", "remove", "--force", str(worktree)], primary, check=False)
        run_capture(["git", "branch", "-D", branch], primary, check=False)
        try:
            task_root.rmdir()
            task_root.parent.rmdir()
        except OSError:
            pass
        shutil.rmtree(task_dir, ignore_errors=True)
        raise

    if not getattr(args, "quiet", False):
        print(json.dumps({"task_id": args.task_id, "status": "INITIALIZED", "worktree": str(worktree), "base_sha": base_sha, "risk_flags": risks}, indent=2))
    return 0


def cmd_run(args: argparse.Namespace) -> int:
    contract, task_dir, _ = load_task_from_current_repo(args.task_id, Path.cwd())
    result = run_task(contract, task_dir)
    state = load_json(task_dir / "state.json")
    print(json.dumps(state, indent=2, sort_keys=True))
    return 0 if result == "PASSED" else 1


def cmd_status(args: argparse.Namespace) -> int:
    contract, task_dir, _ = load_task_from_current_repo(args.task_id, Path.cwd())
    state = load_json(task_dir / "state.json")
    result = {"contract": contract, "state": state}
    if (task_dir / "attestation.json").exists():
        result["attestation"] = load_json(task_dir / "attestation.json")
    if args.json:
        print(json.dumps(result, indent=2, sort_keys=True))
    else:
        print(f"{contract['task_id']}: {state['status']} (round {state.get('round', 0)})")
        print(f"worktree: {contract['worktree_path']}")
        if state.get("reason"):
            print(f"reason: {state['reason']}")
        if state.get("attestation"):
            print(f"attestation: {state['attestation']}")
    return 0


def cmd_verify(args: argparse.Namespace) -> int:
    contract, task_dir, _ = load_task_from_current_repo(args.task_id, Path.cwd())
    attestation = verify_attestation(contract, task_dir)
    print(
        json.dumps(
            {
                "task_id": contract["task_id"],
                "verdict": "PASS",
                "tree_sha": attestation["tree_sha"],
                "staged_diff_sha256": attestation["staged_diff_sha256"],
            },
            indent=2,
        )
    )
    return 0


def cmd_guard_commit(args: argparse.Namespace) -> int:
    current_top = Path(git(Path.cwd(), "rev-parse", "--show-toplevel")).resolve()
    _, common = repo_context(Path.cwd())
    if args.task_id:
        contract, task_dir, _ = load_task_from_current_repo(args.task_id, Path.cwd())
    else:
        matches: List[Tuple[Dict[str, Any], Path]] = []
        tasks_root = task_dir_for(common, "placeholder").parent
        if tasks_root.is_dir():
            for candidate in sorted(tasks_root.iterdir()):
                task_path = candidate / "task.json"
                if not task_path.is_file():
                    continue
                try:
                    candidate_contract = load_json(task_path)
                except HarnessError:
                    continue
                if Path(str(candidate_contract.get("worktree_path", ""))).resolve() == current_top:
                    matches.append((candidate_contract, candidate))
        if len(matches) != 1:
            raise HarnessError(
                f"expected exactly one task contract for {current_top}, found {len(matches)}; pass TASK_ID explicitly"
            )
        contract, task_dir = matches[0]
        validate_schema(contract, load_schema("task"), label="task contract")
        if contract_hash(contract) != contract.get("contract_sha256"):
            raise HarnessError("task contract hash mismatch")
    verify_attestation(contract, task_dir)
    if not getattr(args, "quiet", False):
        print(f"agent-harness: commit gate PASS for {contract['task_id']}")
    return 0


def verify_committed_task(contract: Mapping[str, Any], task_dir: Path) -> Dict[str, Any]:
    """Verify the exact one-commit lifecycle receipt before destructive close."""

    state = load_json(task_dir / "state.json")
    if state.get("status") != "COMMITTED":
        raise HarnessError(f"close requires COMMITTED state; current status is {state.get('status')}")
    worktree = Path(contract["worktree_path"])
    if not worktree.is_dir() or worktree.is_symlink():
        raise HarnessError(f"recorded task worktree is missing or unsafe: {worktree}")
    if Path(git(worktree, "rev-parse", "--show-toplevel")).resolve() != worktree.resolve():
        raise HarnessError("recorded task worktree does not match its Git root")
    if git(worktree, "branch", "--show-current") != contract["branch"]:
        raise HarnessError("task branch changed after commit")

    receipt_path = evidence_file(task_dir, str(task_dir / "commit.json"), task_dir / "commit.json", "commit receipt")
    receipt = load_json(receipt_path)
    validate_schema(receipt, load_schema("commit"), label="commit receipt")
    attestation_path = evidence_file(
        task_dir,
        str(task_dir / "attestation.json"),
        task_dir / "attestation.json",
        "attestation receipt",
    )
    attestation = load_json(attestation_path)
    validate_schema(attestation, load_schema("attestation"), label="attestation")

    head = git(worktree, "rev-parse", "HEAD")
    parents = git(worktree, "rev-list", "--parents", "-n", "1", "HEAD").split()
    if len(parents) != 2 or parents[0] != head or parents[1] != contract["base_sha"]:
        raise HarnessError("close requires exactly one task commit whose sole parent is base_sha")
    if int(git(worktree, "rev-list", "--count", f"{contract['base_sha']}..HEAD")) != 1:
        raise HarnessError("close requires exactly one commit after base_sha")
    tree = git(worktree, "rev-parse", "HEAD^{tree}")
    author = {"name": git(worktree, "log", "-1", "--format=%an"), "email": git(worktree, "log", "-1", "--format=%ae")}
    committer = {"name": git(worktree, "log", "-1", "--format=%cn"), "email": git(worktree, "log", "-1", "--format=%ce")}
    message = git(worktree, "log", "-1", "--format=%B").rstrip("\n")
    authored_at = git(worktree, "log", "-1", "--format=%aI")
    committed_at = git(worktree, "log", "-1", "--format=%cI")
    identity = load_config().get("commit_identity", {})
    expected_identity = {"name": identity.get("name"), "email": identity.get("email")}

    expected = {
        "task_id": contract["task_id"],
        "contract_sha256": contract["contract_sha256"],
        "attestation_sha256": sha256_file(attestation_path),
        "commit_sha": head,
        "parent_sha": contract["base_sha"],
        "tree_sha": tree,
        "author": author,
        "committer": committer,
        "message": message,
        "authored_at": authored_at,
        "committed_at": committed_at,
    }
    for key, value in expected.items():
        if receipt.get(key) != value:
            raise HarnessError(f"commit receipt {key} does not match the exact Git commit")
    if author != expected_identity or committer != expected_identity:
        raise HarnessError(
            f"task commit must be authored and committed by {expected_identity['name']} <{expected_identity['email']}>"
        )
    if tree != attestation.get("tree_sha"):
        raise HarnessError("committed tree does not match the exact attested index tree")
    if attestation.get("contract_sha256") != contract["contract_sha256"]:
        raise HarnessError("attestation no longer belongs to the task contract")
    if state.get("head_sha") != head or state.get("tree_sha") != tree or state.get("branch") != contract["branch"]:
        raise HarnessError("COMMITTED state is stale relative to the exact Git commit")
    attested_at = parse_timestamp(attestation.get("created_at"), "attestation.created_at")
    commit_time = parse_timestamp(committed_at, "commit.committed_at")
    recorded_at = parse_timestamp(receipt.get("recorded_at"), "commit.recorded_at")
    if not attested_at <= commit_time <= recorded_at <= dt.datetime.now(dt.timezone.utc) + dt.timedelta(minutes=5):
        raise HarnessError("commit receipt timestamps are stale or out of order")
    return receipt


def cmd_commit(args: argparse.Namespace) -> int:
    contract, task_dir, _ = load_task_from_current_repo(args.task_id, Path.cwd())
    state = load_json(task_dir / "state.json")
    if state.get("status") != "PASSED":
        raise HarnessError(f"only a PASSED task can be committed; current status is {state.get('status')}")
    attestation = verify_attestation(contract, task_dir)
    worktree = Path(contract["worktree_path"])
    if Path(git(worktree, "rev-parse", "--show-toplevel")).resolve() != worktree.resolve():
        raise HarnessError("recorded task worktree does not match its Git root")
    if git(worktree, "branch", "--show-current") != contract["branch"]:
        raise HarnessError("task branch changed before commit")
    if git(worktree, "rev-parse", "HEAD") != contract["base_sha"]:
        raise HarnessError("task branch must still point at the recorded base before commit")

    message = args.message.strip()
    if not message or "\x00" in message:
        raise HarnessError("commit message must be non-empty and contain no NUL bytes")
    if re.search(r"(?im)^\s*co-authored-by:.*\b(codex|claude|openai|anthropic)\b", message):
        raise HarnessError("AI co-author trailers are forbidden")
    identity = load_config().get("commit_identity", {})
    expected_name = identity.get("name") if isinstance(identity, dict) else None
    expected_email = identity.get("email") if isinstance(identity, dict) else None
    actual_name = git(worktree, "config", "--get", "user.name", check=False)
    actual_email = git(worktree, "config", "--get", "user.email", check=False)
    if not expected_name or not expected_email:
        raise HarnessError("harness commit_identity is missing")
    if (actual_name, actual_email) != (expected_name, expected_email):
        raise HarnessError(
            f"commit identity must be {expected_name} <{expected_email}>; "
            f"found {actual_name or 'unset'} <{actual_email or 'unset'}>"
        )

    commit_argv = ["git", "commit"]
    if not contract["expected_change"]:
        commit_argv.append("--allow-empty")
    commit_argv.extend(["-m", message])
    run_capture(commit_argv, worktree)
    commit_sha = git(worktree, "rev-parse", "HEAD")
    parent_sha = git(worktree, "rev-parse", "HEAD^")
    committed_tree = git(worktree, "rev-parse", "HEAD^{tree}")
    author_name = git(worktree, "log", "-1", "--format=%an")
    author_email = git(worktree, "log", "-1", "--format=%ae")
    committer_name = git(worktree, "log", "-1", "--format=%cn")
    committer_email = git(worktree, "log", "-1", "--format=%ce")
    authored_at = git(worktree, "log", "-1", "--format=%aI")
    committed_at = git(worktree, "log", "-1", "--format=%cI")
    if parent_sha != contract["base_sha"]:
        raise HarnessError("created task commit does not have the recorded base as its sole parent")
    if committed_tree != attestation["tree_sha"]:
        raise HarnessError("created task commit tree differs from the attested tree")
    if (author_name, author_email, committer_name, committer_email) != (
        expected_name,
        expected_email,
        expected_name,
        expected_email,
    ):
        raise HarnessError("created task commit author/committer identity is invalid")
    commit_receipt = {
            "schema_version": 1,
            "task_id": contract["task_id"],
            "contract_sha256": contract["contract_sha256"],
            "attestation_sha256": sha256_file(task_dir / "attestation.json"),
            "commit_sha": commit_sha,
            "parent_sha": parent_sha,
            "tree_sha": committed_tree,
            "author": {"name": author_name, "email": author_email},
            "committer": {"name": committer_name, "email": committer_email},
            "message": message,
            "authored_at": authored_at,
            "committed_at": committed_at,
            "recorded_at": utc_now(),
        }
    validate_schema(commit_receipt, load_schema("commit"), label="commit receipt")
    atomic_write_json(task_dir / "commit.json", commit_receipt)
    set_state(
        task_dir,
        "COMMITTED",
        round=state.get("round", 0),
        phase="commit",
        branch=contract["branch"],
        head_sha=commit_sha,
        tree_sha=committed_tree,
    )
    verify_committed_task(contract, task_dir)
    if not getattr(args, "quiet", False):
        print(json.dumps({"task_id": contract["task_id"], "status": "COMMITTED", "commit_sha": commit_sha}, indent=2))
    return 0


def cmd_close(args: argparse.Namespace) -> int:
    contract, task_dir, _ = load_task_from_current_repo(args.task_id, Path.cwd())
    state = load_json(task_dir / "state.json")
    verify_committed_task(contract, task_dir)
    worktree = Path(contract["worktree_path"])
    if not worktree.is_dir() or worktree.is_symlink():
        raise HarnessError(f"recorded task worktree is missing or unsafe: {worktree}")
    actual_top = Path(git(worktree, "rev-parse", "--show-toplevel")).resolve()
    if actual_top != worktree.resolve():
        raise HarnessError("recorded task worktree does not match its Git root")
    current_branch = git(worktree, "branch", "--show-current")
    if current_branch != contract["branch"]:
        raise HarnessError(f"task branch changed: expected {contract['branch']}, found {current_branch}")
    if run_capture(["git", "merge-base", "--is-ancestor", contract["base_sha"], "HEAD"], worktree, check=False).returncode != 0:
        raise HarnessError("task base is no longer an ancestor of HEAD")
    commit_count_text = git(worktree, "rev-list", "--count", f"{contract['base_sha']}..HEAD")
    if int(commit_count_text) != 1 or git(worktree, "rev-parse", "HEAD^") != contract["base_sha"]:
        raise HarnessError("close requires exactly one task commit whose parent is base_sha")
    attestation = load_json(task_dir / "attestation.json")
    validate_schema(attestation, load_schema("attestation"), label="attestation")
    committed_tree = git(worktree, "rev-parse", "HEAD^{tree}")
    if committed_tree != attestation["tree_sha"]:
        raise HarnessError("committed tree does not match the exact attested index tree")
    if git_bytes(worktree, "diff", "--binary", "--no-ext-diff", "--").strip():
        raise HarnessError("close requires a clean worktree (unstaged changes remain)")
    if staged_diff(worktree).strip():
        raise HarnessError("close requires a clean worktree (staged changes remain)")
    if untracked_paths(worktree):
        raise HarnessError("close requires a clean worktree (untracked files remain)")

    runtime = load_json(task_dir / "runtime.json")
    server_path_raw = runtime.get("server_worktree")
    server_source_raw = runtime.get("server_source")
    server_worktree = Path(server_path_raw) if isinstance(server_path_raw, str) else None
    server_source = Path(server_source_raw) if isinstance(server_source_raw, str) else None
    expected_server_sha = contract.get("dependency_revisions", {}).get("murmur-server.expected")
    primary, _ = repo_context(worktree)
    if server_worktree is not None:
        if server_worktree.resolve() != (worktree.parent / "murmur-server").resolve():
            raise HarnessError("recorded server worktree is outside the exact task root")
        if server_source is None or server_source.resolve() != primary.parent.joinpath("murmur-server").resolve():
            raise HarnessError("recorded server source does not match the canonical sibling repository")
        if not server_worktree.is_dir() or Path(git(server_worktree, "rev-parse", "--show-toplevel")).resolve() != server_worktree.resolve():
            raise HarnessError("recorded server worktree is missing or invalid")
        if git(server_worktree, "rev-parse", "HEAD") != expected_server_sha:
            raise HarnessError("server worktree moved away from the pinned dependency revision")
        if git_bytes(server_worktree, "status", "--porcelain").strip():
            raise HarnessError("close requires the pinned server worktree to remain clean")
    shared_link = worktree / "node_modules"
    shared_target: Optional[Path] = None
    if managed_node_modules_link(worktree):
        shared_target = shared_link.resolve(strict=True)
        shared_link.unlink()
    server_removed = False
    try:
        if server_worktree is not None and server_source is not None:
            run_capture(["git", "worktree", "remove", str(server_worktree)], server_source)
            server_removed = True
        run_capture(["git", "worktree", "remove", str(worktree)], primary)
    except Exception:
        if server_removed and server_source is not None and server_worktree is not None and expected_server_sha:
            run_capture(
                ["git", "worktree", "add", "--detach", str(server_worktree), expected_server_sha],
                server_source,
                check=False,
            )
        if shared_target is not None and worktree.is_dir() and not shared_link.exists():
            os.symlink(str(shared_target), str(shared_link), target_is_directory=True)
        raise
    task_root = worktree.parent
    try:
        task_root.rmdir()
        task_root.parent.rmdir()
    except OSError:
        pass
    set_state(
        task_dir,
        "CLOSED",
        round=state.get("round", 0),
        phase="close",
        branch=contract["branch"],
        head_sha=git(primary, "rev-parse", contract["branch"]),
        worktree_removed=str(worktree),
    )
    if not getattr(args, "quiet", False):
        print(f"{contract['task_id']}: CLOSED (branch preserved: {contract['branch']})")
    return 0


def version_tuple(raw: str) -> Tuple[int, ...]:
    match = re.search(r"\d+(?:\.\d+)+", raw)
    if not match:
        return ()
    return tuple(int(part) for part in match.group(0).split("."))


def cmd_doctor(args: argparse.Namespace) -> int:
    checks: List[Dict[str, Any]] = []
    primary: Optional[Path] = None

    def record(name: str, ok: bool, detail: str, required: bool = True) -> None:
        checks.append({"name": name, "ok": ok, "required": required, "detail": detail})

    try:
        primary, common = repo_context(Path.cwd())
        record("git-repository", True, f"{primary} ({common})")
    except HarnessError as exc:
        record("git-repository", False, str(exc))
    record("python", sys.version_info >= (3, 9), sys.version.split()[0])
    sandbox = Path("/usr/bin/sandbox-exec")
    record(
        "check-sandbox",
        sys.platform == "darwin" and sandbox.is_file() and os.access(sandbox, os.X_OK),
        f"{sys.platform}: {sandbox}",
    )
    try:
        config = load_config()
        record("config", True, str(CONFIG_PATH))
    except HarnessError as exc:
        config = {}
        record("config", False, str(exc))
    for name in ("task", "model-result", "review", "attestation", "commit"):
        try:
            load_schema(name)
            record(f"schema:{name}", True, str(SCHEMAS_DIR / f"{name}.schema.json"))
        except HarnessError as exc:
            record(f"schema:{name}", False, str(exc))
    prompt_names = {"implementer", "spec-reviewer", "adversarial-reviewer"}
    if isinstance(config, dict):
        prompt_names.update(f"{name}-reviewer" for name in config.get("risk_reviews", {}).values())
    for name in sorted(prompt_names):
        path = PROMPTS_DIR / f"{name}.md"
        record(f"prompt:{name}", path.is_file(), str(path))
    wrapper = HARNESS_ROOT.parent.parent / "scripts" / "agent-harness"
    record("wrapper:agent-harness", wrapper.is_file() and os.access(wrapper, os.X_OK), str(wrapper))
    if primary is not None and has_murmur_server_path_dependency(primary):
        pin_path = primary / ".murmur-server-revision"
        pin = pin_path.read_text(encoding="utf-8").strip() if pin_path.is_file() else ""
        record("dependency-pin", bool(SHA1_RE.fullmatch(pin)), f"{pin_path}: {pin or 'missing'}")
        server_source = primary.parent / "murmur-server"
        server_has_pin = False
        if server_source.is_dir() and SHA1_RE.fullmatch(pin):
            server_has_pin = run_capture(
                ["git", "cat-file", "-e", f"{pin}^{{commit}}"], server_source, check=False
            ).returncode == 0
        record("dependency-object", server_has_pin, f"{server_source}: {pin or 'missing'}")
    configured_cli = config.get("cli", {}) if isinstance(config, dict) else {}
    available_vendors = 0
    for vendor in ("codex", "claude"):
        version = command_version(vendor)
        minimum = str(configured_cli.get(vendor, {}).get("minimum_version", "0.0.0"))
        ok = version is not None and version_tuple(version) >= version_tuple(minimum)
        if ok:
            available_vendors += 1
        detail = f"{version or 'missing'} (minimum {minimum})"
        record(f"cli:{vendor}", ok, detail, required=False)
        if vendor == "claude" and version is not None:
            recommended = str(configured_cli.get(vendor, {}).get("recommended_version", minimum))
            recommended_ok = version_tuple(version) >= version_tuple(recommended)
            record(
                "cli:claude-recommended",
                recommended_ok,
                f"{version} (recommended {recommended}; older stream-json builds may be less reliable)",
                required=False,
            )
    record("cli:any-real-adapter", available_vendors > 0, f"{available_vendors} available")
    ok = all(item["ok"] for item in checks if item["required"])
    payload = {"ok": ok, "checks": checks}
    if args.json:
        print(json.dumps(payload, indent=2, sort_keys=True))
    else:
        for item in checks:
            marker = "OK" if item["ok"] else ("WARN" if not item["required"] else "FAIL")
            print(f"[{marker}] {item['name']}: {item['detail']}")
    return 0 if ok else 1


def _selftest_init_args(task_id: str, prompt: str, expected_change: bool) -> argparse.Namespace:
    return argparse.Namespace(
        task_id=task_id,
        kind="harness",
        agent="fake",
        reviewer="fake",
        prompt=prompt,
        owned=["owned.txt"],
        risk=[],
        check=["deterministic::test -f owned.txt"],
        final_check=["final::test -f owned.txt"],
        max_repair_rounds=2,
        base=None,
        branch=None,
        expected_change=expected_change,
        quiet=True,
    )


def cmd_selftest(_args: argparse.Namespace) -> int:
    failures: List[str] = []
    claude_schema = schema_for_model_cli(load_schema("review"), "claude")
    if "$schema" in claude_schema or "$id" in claude_schema:
        failures.append("Claude CLI schema retained unsupported draft metadata")
    codex_schema = schema_for_model_cli(load_schema("review"), "codex")
    if "$schema" not in codex_schema or "$id" not in codex_schema:
        failures.append("Codex CLI schema unexpectedly lost canonical draft metadata")
    with tempfile.TemporaryDirectory(prefix="murmur-agent-harness-") as temp_name:
        server_repo = Path(temp_name) / "murmur-server"
        server_repo.mkdir()
        run_capture(["git", "init", "-q", "-b", "main"], server_repo)
        run_capture(["git", "config", "user.name", "Harness Selftest"], server_repo)
        run_capture(["git", "config", "user.email", "harness@example.invalid"], server_repo)
        protocol_dir = server_repo / "crates" / "murmur-protocol" / "src"
        protocol_dir.mkdir(parents=True)
        (protocol_dir / "lib.rs").write_text("pub const SELFTEST: bool = true;\n", encoding="utf-8")
        (protocol_dir.parent / "Cargo.toml").write_text(
            '[package]\nname="murmur-protocol"\nversion="0.0.0"\nedition="2021"\n',
            encoding="utf-8",
        )
        run_capture(["git", "add", "."], server_repo)
        run_capture(["git", "commit", "-qm", "pinned server"], server_repo)
        pinned_server_sha = git(server_repo, "rev-parse", "HEAD")

        repo = Path(temp_name) / "repo"
        repo.mkdir()
        run_capture(["git", "init", "-q", "-b", "murmur"], repo)
        run_capture(["git", "config", "user.name", "QueaT"], repo)
        run_capture(["git", "config", "user.email", "kgm004a@gmail.com"], repo)
        (repo / "owned.txt").write_text("base\n", encoding="utf-8")
        (repo / "other.txt").write_text("base\n", encoding="utf-8")
        (repo / "AGENTS.md").write_text("committed instructions\n", encoding="utf-8")
        (repo / ".gitignore").write_text("/node_modules/\n", encoding="utf-8")
        (repo / ".murmur-server-revision").write_text(pinned_server_sha + "\n", encoding="utf-8")
        cargo_dir = repo / "src-tauri"
        cargo_dir.mkdir()
        (cargo_dir / "Cargo.toml").write_text(
            '[package]\nname="selftest-app"\nversion="0.0.0"\nedition="2021"\n'
            '[dependencies]\nmurmur-protocol={path="../../murmur-server/crates/murmur-protocol"}\n',
            encoding="utf-8",
        )
        run_capture(
            [
                "git",
                "add",
                "owned.txt",
                "other.txt",
                "AGENTS.md",
                ".gitignore",
                ".murmur-server-revision",
                "src-tauri/Cargo.toml",
            ],
            repo,
        )
        run_capture(["git", "commit", "-qm", "base"], repo)
        (repo / "node_modules").mkdir()
        # The contract must describe the committed worktree instructions, not
        # ambient dirty instructions from the primary checkout.
        (repo / "AGENTS.md").write_text("dirty primary instructions\n", encoding="utf-8")
        original_cwd = Path.cwd()
        try:
            os.chdir(repo)
            args = _selftest_init_args("selftest-pass", "exercise the passing loop", True)
            cmd_init(args)
            contract, task_dir, _ = load_task_from_current_repo("selftest-pass", repo)
            worktree = Path(contract["worktree_path"])
            shared_modules = worktree / "node_modules"
            if not shared_modules.is_symlink() or shared_modules.resolve() != (repo / "node_modules").resolve():
                failures.append("isolated worktree did not safely share ignored node_modules")
            if contract["instructions_sha256"] != instructions_hash(worktree):
                failures.append("contract did not fingerprint isolated worktree instructions")
            if contract["instructions_sha256"] == instructions_hash(repo):
                failures.append("contract accidentally fingerprinted dirty primary instructions")
            expected_dependency = contract["dependency_revisions"].get("murmur-server.expected")
            actual_dependency = contract["dependency_revisions"].get("murmur-server.head")
            task_server = worktree.parent / "murmur-server"
            if expected_dependency != pinned_server_sha or actual_dependency != pinned_server_sha:
                failures.append("contract did not bind the exact pinned server dependency")
            if not task_server.is_dir() or git_bytes(task_server, "status", "--porcelain").strip():
                failures.append("init did not create a clean detached server worktree")
            (worktree / "owned.txt").write_text("base\nchanged\n", encoding="utf-8")
            poisoned_zdotdir = Path(temp_name) / "poisoned-zdotdir"
            poisoned_zdotdir.mkdir()
            (poisoned_zdotdir / ".zshrc").write_text("exit 77\n", encoding="utf-8")
            previous_zdotdir = os.environ.get("ZDOTDIR")
            os.environ["ZDOTDIR"] = str(poisoned_zdotdir)
            try:
                if run_task(contract, task_dir) != "PASSED":
                    failures.append("passing fake task did not reach PASSED")
            finally:
                if previous_zdotdir is None:
                    os.environ.pop("ZDOTDIR", None)
                else:
                    os.environ["ZDOTDIR"] = previous_zdotdir
            verify_attestation(contract, task_dir)
            original_attestation = load_json(task_dir / "attestation.json")
            reused_session_attestation = copy.deepcopy(original_attestation)
            for review in reused_session_attestation["reviews"]:
                review["reviewer"]["session_id"] = reused_session_attestation["writer"]["session_id"]
            atomic_write_json(task_dir / "attestation.json", reused_session_attestation)
            try:
                verify_attestation(contract, task_dir)
                failures.append("canonical verifier accepted reviewer sessions reused from the writer")
            except HarnessError:
                pass
            finally:
                atomic_write_json(task_dir / "attestation.json", original_attestation)
            previous_cwd = Path.cwd()
            try:
                os.chdir(worktree)
                cmd_guard_commit(argparse.Namespace(task_id=None, quiet=True))
            finally:
                os.chdir(previous_cwd)
            (worktree / "owned.txt").write_text("base\nchanged\nstale\n", encoding="utf-8")
            try:
                verify_attestation(contract, task_dir)
                failures.append("post-attestation edit was not rejected")
            except HarnessError:
                pass
            run_capture(["git", "add", "owned.txt"], worktree)
            run_capture(
                [
                    "git",
                    "-c",
                    "user.name=Wrong Identity",
                    "-c",
                    "user.email=wrong@example.invalid",
                    "commit",
                    "-qm",
                    "tampered after receipt",
                ],
                worktree,
            )
            try:
                cmd_close(argparse.Namespace(task_id="selftest-pass", quiet=True))
                failures.append("close accepted a manually-created wrong-identity commit from PASSED state")
            except HarnessError as exc:
                if "COMMITTED state" not in str(exc):
                    failures.append("close rejected a manual PASSED-state commit for an unexpected reason")

            args = _selftest_init_args("selftest-scope", "reject out-of-scope edits", False)
            cmd_init(args)
            scope_contract, scope_dir, _ = load_task_from_current_repo("selftest-scope", repo)
            scope_worktree = Path(scope_contract["worktree_path"])
            (scope_worktree / "other.txt").write_text("out of scope\n", encoding="utf-8")
            if run_task(scope_contract, scope_dir) != "FAILED":
                failures.append("out-of-scope edit was not rejected")

            args = _selftest_init_args("selftest-repair", "exercise one bounded repair", True)
            cmd_init(args)
            repair_contract, repair_dir, _ = load_task_from_current_repo("selftest-repair", repo)
            repair_worktree = Path(repair_contract["worktree_path"])
            (repair_worktree / "owned.txt").write_text("base\nrepair target\n", encoding="utf-8")
            os.environ["MURMUR_HARNESS_FAKE_ROUND_01_SPEC_VERDICT"] = "FAIL"
            try:
                if run_task(repair_contract, repair_dir) != "PASSED":
                    failures.append("repair task did not recover from first-round review failure")
            finally:
                os.environ.pop("MURMUR_HARNESS_FAKE_ROUND_01_SPEC_VERDICT", None)
            repair_state = load_json(repair_dir / "state.json")
            if repair_state.get("round") != 2:
                failures.append("repair task did not record exactly two writer rounds")
            verify_attestation(repair_contract, repair_dir)
            cmd_commit(argparse.Namespace(task_id="selftest-repair", message="test: selftest repair task", quiet=True))
            if load_json(repair_dir / "state.json").get("status") != "COMMITTED":
                failures.append("runner commit did not record COMMITTED state")
            original_commit_receipt = load_json(repair_dir / "commit.json")
            tampered_commit_receipt = copy.deepcopy(original_commit_receipt)
            tampered_commit_receipt["author"]["name"] = "Wrong Identity"
            atomic_write_json(repair_dir / "commit.json", tampered_commit_receipt)
            try:
                cmd_close(argparse.Namespace(task_id="selftest-repair", quiet=True))
                failures.append("close accepted a commit receipt with a forged author")
            except HarnessError:
                pass
            finally:
                atomic_write_json(repair_dir / "commit.json", original_commit_receipt)
            cmd_close(argparse.Namespace(task_id="selftest-repair", quiet=True))
            if repair_worktree.exists() or load_json(repair_dir / "state.json").get("status") != "CLOSED":
                failures.append("close did not remove the exact committed worktree and preserve CLOSED state")
            if run_capture(
                ["git", "show-ref", "--verify", "--quiet", f"refs/heads/{repair_contract['branch']}"],
                repo,
                check=False,
            ).returncode != 0:
                failures.append("close removed the preserved task branch")

            # Prove dead-owner recovery without weakening the live-owner rule.
            cargo_lock = repair_dir.parent.parent / "cargo-lock"
            cargo_lock.mkdir()
            stale_pid = 900000
            while _pid_is_alive(stale_pid):
                stale_pid += 1
            atomic_write_json(cargo_lock / "owner.json", {"pid": stale_pid, "task_id": "dead"})
            recovered = acquire_cargo_lock(repair_dir, 1)
            release_cargo_lock(recovered)

            # Playwright workers reload playwright.config.ts in fresh PIDs, so
            # the runner must assign and hold a stable task-private port.
            port_probe = (
                "import os,sys; "
                "port=int(os.environ.get('MURMUR_E2E_PORT','0')); "
                "sys.exit(0 if 42000 <= port < 62000 else 1)"
            )
            port_check = run_check(
                repo,
                repair_dir,
                {
                    "id": "playwright-port",
                    "command": f"{json.dumps(sys.executable)} -c {json.dumps(port_probe)} playwright",
                    "timeout_seconds": 5,
                },
                "selftest",
            )
            if not port_check["passed"]:
                failures.append("Playwright check did not receive a task-private port")
            locks_root = repair_dir.parent.parent / "playwright-ports"
            if any(locks_root.glob("*.lock")):
                failures.append("Playwright port lock was not released after the check")

            os.environ["MURMUR_CHECK_SECRET_TOKEN"] = "must-not-cross-check-boundary"
            try:
                secret_probe = "import os,sys; sys.exit(1 if 'MURMUR_CHECK_SECRET_TOKEN' in os.environ else 0)"
                secret_check = run_check(
                    repo,
                    repair_dir,
                    {
                        "id": "ambient-secret",
                        "command": f"{json.dumps(sys.executable)} -c {json.dumps(secret_probe)}",
                        "timeout_seconds": 5,
                    },
                    "selftest",
                )
                if not secret_check["passed"]:
                    failures.append("deterministic check inherited a synthetic ambient secret")
            finally:
                os.environ.pop("MURMUR_CHECK_SECRET_TOKEN", None)

            outside_probe = Path(temp_name) / "outside-check-sandbox.txt"
            outside_probe.write_text("unchanged\n", encoding="utf-8")
            outside_write = f"from pathlib import Path; Path({str(outside_probe)!r}).write_text('mutated')"
            write_check = run_check(
                repo,
                repair_dir,
                {
                    "id": "outside-write",
                    "command": f"{json.dumps(sys.executable)} -c {json.dumps(outside_write)}",
                    "timeout_seconds": 5,
                },
                "selftest",
            )
            if write_check["passed"] or outside_probe.read_text(encoding="utf-8") != "unchanged\n":
                failures.append("check sandbox allowed a write outside task-owned paths")
            outside_read = f"from pathlib import Path; Path({str(outside_probe)!r}).read_text()"
            read_check = run_check(
                repo,
                repair_dir,
                {
                    "id": "outside-read",
                    "command": f"{json.dumps(sys.executable)} -c {json.dumps(outside_read)}",
                    "timeout_seconds": 5,
                },
                "selftest",
            )
            if read_check["passed"]:
                failures.append("check sandbox allowed an arbitrary outside read")

            outbound_probe = (
                "import socket,sys; s=socket.socket(); s.settimeout(0.2); "
                "sys.exit(0 if s.connect_ex(('1.1.1.1',443)) == 0 else 1)"
            )
            outbound_check = run_check(
                repo,
                repair_dir,
                {
                    "id": "outbound-network",
                    "command": f"{json.dumps(sys.executable)} -c {json.dumps(outbound_probe)}",
                    "timeout_seconds": 5,
                },
                "selftest",
            )
            if outbound_check["passed"]:
                failures.append("check sandbox allowed outbound Internet access")
            loopback_probe = (
                "import socket; s=socket.socket(); s.bind(('127.0.0.1',0)); s.listen(1); "
                "c=socket.socket(); c.connect(s.getsockname()); conn,_=s.accept(); conn.close(); c.close(); s.close()"
            )
            loopback_check = run_check(
                repo,
                repair_dir,
                {
                    "id": "loopback-network",
                    "command": f"{json.dumps(sys.executable)} -c {json.dumps(loopback_probe)} playwright",
                    "timeout_seconds": 5,
                },
                "selftest",
            )
            if not loopback_check["passed"]:
                failures.append("loopback-only sandbox blocked a local runtime check")

            timeout_result = run_logged_process(
                [sys.executable, "-c", "import time; time.sleep(5)"],
                cwd=repo,
                timeout_seconds=0.1,  # deliberate fast selftest
                log_path=Path(temp_name) / "timeout.log",
            )
            if not timeout_result["timed_out"] or timeout_result["exit_code"] == 0:
                failures.append("wall timeout did not terminate the synthetic process group")
            os.environ["MURMUR_SELFTEST_SECRET_TOKEN"] = "must-not-cross-model-boundary"
            try:
                clean_env, removed_names = sanitized_model_environment("0" * 64, "codex")
                if "MURMUR_SELFTEST_SECRET_TOKEN" in clean_env:
                    failures.append("model environment retained a synthetic ambient secret")
                if "MURMUR_SELFTEST_SECRET_TOKEN" not in removed_names:
                    failures.append("model environment did not audit the removed secret variable name")
                if "PATH" not in clean_env or clean_env.get("MURMUR_HARNESS_INSTRUCTIONS_SHA256") != "0" * 64:
                    failures.append("sanitized model environment dropped required non-secret controls")
            finally:
                os.environ.pop("MURMUR_SELFTEST_SECRET_TOKEN", None)

            risk_args = _selftest_init_args("selftest-risk", "require lock evidence", True)
            risk_args.owned = ["src-tauri/src/crypto.rs"]
            risk_args.check = []
            risk_args.final_check = []
            try:
                cmd_init(risk_args)
                failures.append("lock-risk init accepted missing deterministic evidence")
            except HarnessError as exc:
                if "lock-negative" not in str(exc) or "lock-roundtrip" not in str(exc):
                    failures.append("lock-risk init returned an unclear missing-evidence error")

            no_check_args = _selftest_init_args("selftest-no-check", "reject zero-check receipts", False)
            no_check_args.check = []
            no_check_args.final_check = []
            try:
                cmd_init(no_check_args)
                failures.append("zero-check task was accepted")
            except HarnessError as exc:
                if "at least one deterministic" not in str(exc):
                    failures.append("zero-check task returned an unclear error")

            for unsafe_root in (".", "./"):
                try:
                    normalize_owned_path(unsafe_root)
                    failures.append(f"repository-root owned path was accepted: {unsafe_root!r}")
                except HarnessError:
                    pass

            symlink_args = _selftest_init_args("selftest-symlink", "reject external symlink scope", True)
            symlink_args.owned = ["owned-link"]
            symlink_args.check = ["symlink::test -L owned-link"]
            cmd_init(symlink_args)
            symlink_contract, symlink_dir, _ = load_task_from_current_repo("selftest-symlink", repo)
            symlink_worktree = Path(symlink_contract["worktree_path"])
            os.symlink(str((repo / "AGENTS.md").resolve()), str(symlink_worktree / "owned-link"))
            if run_task(symlink_contract, symlink_dir) != "FAILED":
                failures.append("changed external symlink was not rejected as a scope violation")

            hardlink_args = _selftest_init_args("selftest-hardlink", "reject hardlink scope escape", True)
            hardlink_args.owned = ["owned-hardlink"]
            hardlink_args.check = ["hardlink::test -f owned-hardlink"]
            cmd_init(hardlink_args)
            hardlink_contract, hardlink_dir, _ = load_task_from_current_repo("selftest-hardlink", repo)
            hardlink_worktree = Path(hardlink_contract["worktree_path"])
            outside_hardlink = Path(temp_name) / "outside-hardlink.txt"
            outside_hardlink.write_text("must remain outside\n", encoding="utf-8")
            os.link(outside_hardlink, hardlink_worktree / "owned-hardlink")
            if run_task(hardlink_contract, hardlink_dir) != "FAILED":
                failures.append("changed external hardlink was not rejected as a scope violation")
            if outside_hardlink.read_text(encoding="utf-8") != "must remain outside\n":
                failures.append("hardlink scope probe mutated the outside inode")

            protected_args = _selftest_init_args("selftest-protected-branch", "reject protected branch", False)
            protected_args.branch = "refs/heads/main"
            try:
                cmd_init(protected_args)
                failures.append("protected task branch was accepted")
            except HarnessError as exc:
                if "protected branch" not in str(exc):
                    failures.append("protected branch returned an unclear error")
        except HarnessError as exc:
            failures.append(str(exc))
        finally:
            os.chdir(original_cwd)

    if failures:
        print("agent harness selftest: FAIL", file=sys.stderr)
        for failure in failures:
            print(f"- {failure}", file=sys.stderr)
        return 1
    print("agent harness selftest: PASS")
    return 0


def dispatch_eval(argv: Sequence[str]) -> int:
    candidate = HARNESS_ROOT / "eval_runner.py"
    if not candidate.is_file():
        raise HarnessError("eval runner is not installed yet; expected .agents/harness/eval_runner.py")
    import importlib.util

    spec = importlib.util.spec_from_file_location("murmur_agent_eval_runner", candidate)
    if spec is None or spec.loader is None:
        raise HarnessError(f"cannot load eval runner: {candidate}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    entrypoint = getattr(module, "main", None)
    if not callable(entrypoint):
        raise HarnessError("eval runner must export main(argv)")
    return int(entrypoint(list(argv)) or 0)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="agent-harness", description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    init_parser = subparsers.add_parser("init", help="create a task contract and isolated worktree")
    init_parser.add_argument("task_id")
    init_parser.add_argument("--kind", choices=["bug", "feature", "refactor", "docs", "harness"], default="feature")
    init_parser.add_argument("--agent", choices=["codex", "claude", "fake"], default="codex")
    init_parser.add_argument("--reviewer", choices=["codex", "claude", "fake"])
    init_parser.add_argument("--prompt", required=True)
    init_parser.add_argument("--owned", action="append", required=True, metavar="PATH")
    init_parser.add_argument("--risk", action="append", default=[], choices=["lock", "egress", "protocol", "runtime", "ui", "release"])
    init_parser.add_argument("--check", action="append", default=[], metavar="ID::COMMAND")
    init_parser.add_argument("--final-check", action="append", default=[], metavar="ID::COMMAND")
    init_parser.add_argument("--max-repair-rounds", type=int, default=2, choices=range(0, 6))
    init_parser.add_argument("--base", help="committed base ref (default: HEAD)")
    init_parser.add_argument("--branch", help="task branch (default: agent/<task-id>)")
    expected = init_parser.add_mutually_exclusive_group()
    expected.add_argument("--expected-change", dest="expected_change", action="store_true", default=True)
    expected.add_argument("--no-expected-change", dest="expected_change", action="store_false")
    init_parser.set_defaults(handler=cmd_init)

    run_parser = subparsers.add_parser("run", help="execute the bounded task loop")
    run_parser.add_argument("task_id")
    run_parser.set_defaults(handler=cmd_run)

    status_parser = subparsers.add_parser("status", help="show task state and evidence location")
    status_parser.add_argument("task_id")
    status_parser.add_argument("--json", action="store_true")
    status_parser.set_defaults(handler=cmd_status)

    verify_parser = subparsers.add_parser("verify-attestation", help="verify PASS against the current exact tree")
    verify_parser.add_argument("task_id")
    verify_parser.set_defaults(handler=cmd_verify)

    guard_parser = subparsers.add_parser("guard-commit", help="fail closed unless the current staged diff has a fresh PASS")
    guard_parser.add_argument("task_id", nargs="?")
    guard_parser.set_defaults(handler=cmd_guard_commit)

    commit_parser = subparsers.add_parser("commit", help="commit the exact attested index on the task branch")
    commit_parser.add_argument("task_id")
    commit_parser.add_argument("-m", "--message", required=True)
    commit_parser.set_defaults(handler=cmd_commit)

    close_parser = subparsers.add_parser("close", help="remove a clean committed task worktree and preserve evidence")
    close_parser.add_argument("task_id")
    close_parser.set_defaults(handler=cmd_close)

    doctor_parser = subparsers.add_parser("doctor", help="check local harness dependencies without invoking a model")
    doctor_parser.add_argument("--json", action="store_true")
    doctor_parser.set_defaults(handler=cmd_doctor)

    selftest_parser = subparsers.add_parser("selftest", help="exercise isolation, scope, and freshness with fake adapters")
    selftest_parser.add_argument("--ci", action="store_true", help="CI-compatible concise mode (never invokes a model)")
    selftest_parser.set_defaults(handler=cmd_selftest)
    # `main` delegates this command before argparse; registering it here keeps
    # top-level help truthful without coupling this runner to eval internals.
    subparsers.add_parser("eval", help="delegate to the optional development-agent eval runner")
    return parser


def main(argv: Optional[Sequence[str]] = None) -> int:
    arguments = list(argv if argv is not None else sys.argv[1:])
    if arguments and arguments[0] == "eval":
        return dispatch_eval(arguments[1:])
    parser = build_parser()
    args = parser.parse_args(arguments)
    return int(args.handler(args))


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except HarnessError as exc:
        print(f"agent-harness: {exc}", file=sys.stderr)
        raise SystemExit(exc.exit_code)
