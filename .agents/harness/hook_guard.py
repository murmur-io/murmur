#!/usr/bin/env python3
"""Canonical Claude/Codex hook guard for Murmur's development harness.

The vendor hook files are deliberately tiny adapters.  All command parsing,
secret scanning, and hash-bound completion checks live here so the two agent
clients cannot silently drift.
"""

from __future__ import annotations

import argparse
import copy
import datetime as dt
import fnmatch
import hashlib
import json
import os
import re
import shlex
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path, PurePosixPath
from typing import Any, Dict, Iterable, List, Mapping, Optional, Sequence, Tuple

import resource_policy


SOURCE_ROOT = Path(__file__).resolve().parents[2]
HARNESS_ROOT = SOURCE_ROOT / ".agents" / "harness"
TASK_ID_RE = re.compile(r"^[a-z0-9][a-z0-9._-]{1,63}$")
SHA1_RE = re.compile(r"^[0-9a-f]{40}$")
SHA256_RE = re.compile(r"^[0-9a-f]{64}$")
PROTECTED_BRANCHES = {"murmur", "main", "master"}
SEPARATORS = {";", "&&", "||", "|", "&", "(", ")", "\n"}


class GuardFailure(RuntimeError):
    """A deterministic reason the attempted operation must be refused."""


@dataclass(frozen=True)
class SimpleCommand:
    tokens: Tuple[str, ...]


@dataclass(frozen=True)
class GitInvocation:
    tokens: Tuple[str, ...]
    subcommand: str
    args: Tuple[str, ...]
    cwd: Path


def _run(
    argv: Sequence[str], cwd: Path, *, check: bool = True, text: bool = False
) -> subprocess.CompletedProcess:
    completed = subprocess.run(
        list(argv),
        cwd=str(cwd),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=text,
        check=False,
    )
    if check and completed.returncode != 0:
        stderr = completed.stderr if text else completed.stderr.decode("utf-8", "replace")
        raise GuardFailure(f"{' '.join(argv)} failed: {stderr.strip()}")
    return completed


def _git_text(cwd: Path, *args: str, check: bool = True) -> str:
    return _run(["git", *args], cwd, check=check, text=True).stdout.strip()


def _git_bytes(cwd: Path, *args: str, check: bool = True) -> bytes:
    return _run(["git", *args], cwd, check=check).stdout


def _sha256(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def _canonical_json(value: Any) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


def _load_json(path: Path) -> Any:
    try:
        with path.open("r", encoding="utf-8") as handle:
            return json.load(handle)
    except FileNotFoundError as exc:
        raise GuardFailure(f"missing required file: {path}") from exc
    except (OSError, json.JSONDecodeError) as exc:
        raise GuardFailure(f"invalid JSON in {path}: {exc}") from exc


def _task_runner_module() -> Any:
    """Import the runner helpers so fingerprints/diffs have one exact definition."""

    harness_path = str(HARNESS_ROOT)
    if harness_path not in sys.path:
        sys.path.insert(0, harness_path)
    try:
        import task_runner  # type: ignore
    except Exception as exc:  # pragma: no cover - exercised as a fail-closed path
        raise GuardFailure(f"cannot load the canonical task runner: {exc}") from exc
    return task_runner


def _payload_command(payload: Any) -> str:
    if not isinstance(payload, dict):
        raise GuardFailure("hook payload must be a JSON object")
    candidates: List[Any] = []
    for key in ("tool_input", "input", "arguments"):
        nested = payload.get(key)
        if isinstance(nested, dict):
            candidates.append(nested.get("command"))
    tool = payload.get("tool")
    if isinstance(tool, dict):
        arguments = tool.get("arguments")
        if isinstance(arguments, dict):
            candidates.append(arguments.get("command"))
    candidates.append(payload.get("command"))
    for candidate in candidates:
        if isinstance(candidate, str):
            return candidate
    raise GuardFailure("hook payload is missing the Bash command")


def _payload_process_cwd(payload: Any) -> Path:
    """Resolve the directory in which the observed command will execute.

    Hook processes run in the session cwd, while unified ``exec_command`` may
    carry a per-call ``workdir``. Ignoring the latter makes a valid linked
    worktree receipt look unrelated to the commit it is guarding.
    """

    if not isinstance(payload, dict):
        raise GuardFailure("hook payload must be a JSON object")
    session_raw = payload.get("cwd")
    if session_raw is None:
        session = Path.cwd()
    elif isinstance(session_raw, str) and session_raw and "\x00" not in session_raw:
        candidate = Path(session_raw)
        session = (candidate if candidate.is_absolute() else Path.cwd() / candidate).resolve()
    else:
        raise GuardFailure("hook payload cwd is malformed")

    candidates: List[Any] = []
    for key in ("tool_input", "input", "arguments"):
        nested = payload.get(key)
        if isinstance(nested, dict):
            candidates.extend((nested.get("workdir"), nested.get("cwd")))
    tool = payload.get("tool")
    if isinstance(tool, dict) and isinstance(tool.get("arguments"), dict):
        arguments = tool["arguments"]
        candidates.extend((arguments.get("workdir"), arguments.get("cwd")))
    for raw in candidates:
        if raw is None:
            continue
        if not isinstance(raw, str) or not raw or "\x00" in raw:
            raise GuardFailure("tool workdir is malformed")
        candidate = Path(raw)
        return (candidate if candidate.is_absolute() else session / candidate).resolve()
    return session.resolve()


def _read_payload() -> Tuple[str, Path]:
    try:
        payload = json.load(sys.stdin)
    except json.JSONDecodeError as exc:
        raise GuardFailure(f"malformed hook JSON: {exc}") from exc
    return _payload_command(payload), _payload_process_cwd(payload)


def _simple_commands(command: str) -> List[SimpleCommand]:
    """Tokenize executable shell clauses without matching quoted rg/search text.

    This is intentionally not a shell evaluator.  It recognizes the command
    positions needed by the guard, including compound and multiline commands,
    while leaving quoted strings as arguments to their real executable.
    """

    command = command.replace("\\\r\n", "").replace("\\\n", "")
    lexer = shlex.shlex(command, posix=True, punctuation_chars=";&|()\n")
    lexer.whitespace = " \t\r"
    lexer.whitespace_split = True
    lexer.commenters = ""
    try:
        tokens = list(lexer)
    except ValueError as exc:
        raise GuardFailure(f"cannot safely parse Bash command: {exc}") from exc

    result: List[SimpleCommand] = []
    current: List[str] = []
    for token in tokens:
        if token in SEPARATORS or (token and set(token) <= set(";&|()\n")):
            if current:
                result.append(SimpleCommand(tuple(current)))
                current = []
        else:
            current.append(token)
    if current:
        result.append(SimpleCommand(tuple(current)))

    # `bash -c '…'` is executable text, unlike a string passed to rg.  Recurse
    # only for actual shell interpreters to retain that distinction.
    nested: List[SimpleCommand] = []
    for simple in list(result):
        effective = _effective_tokens(simple.tokens)
        if not effective:
            continue
        if Path(effective[0]).name in {"bash", "sh", "zsh"}:
            for index, token in enumerate(effective[1:], 1):
                if token == "-c" and index + 1 < len(effective):
                    nested.extend(_simple_commands(effective[index + 1]))
                    break
    result.extend(nested)
    return result


_ASSIGNMENT_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=.*$", re.DOTALL)


def _effective_tokens(tokens: Sequence[str]) -> Tuple[str, ...]:
    """Strip common command wrappers and return executable-first tokens."""

    values = list(tokens)
    while values and _ASSIGNMENT_RE.match(values[0]):
        values.pop(0)
    changed = True
    while values and changed:
        changed = False
        executable = Path(values[0]).name
        if executable == "env":
            values.pop(0)
            while values:
                token = values[0]
                if token == "--":
                    values.pop(0)
                    break
                if token in {"-u", "--unset", "-C", "--chdir"}:
                    values = values[2:] if len(values) >= 2 else []
                    continue
                if token.startswith("-") or _ASSIGNMENT_RE.match(token):
                    values.pop(0)
                    continue
                break
            changed = True
        elif executable == "sudo":
            values.pop(0)
            while values:
                token = values[0]
                if token == "--":
                    values.pop(0)
                    break
                if token in {"-u", "-g", "-h", "-p", "-C", "-T", "-R", "-D"}:
                    values = values[2:] if len(values) >= 2 else []
                    continue
                if token.startswith("-"):
                    values.pop(0)
                    continue
                break
            changed = True
        elif executable in {"command", "builtin", "nohup"}:
            values.pop(0)
            while values and values[0].startswith("-"):
                values.pop(0)
            changed = True
        while values and _ASSIGNMENT_RE.match(values[0]):
            values.pop(0)
            changed = True
    return tuple(values)


def _unsupported_execution_indirection(command: str) -> Optional[str]:
    """Fail closed when the hook cannot see the executable command directly."""

    single_quoted = False
    double_quoted = False
    escaped = False
    active_shell_indirection = False
    index = 0
    while index < len(command):
        character = command[index]
        if escaped:
            escaped = False
            index += 1
            continue
        if character == "\\" and not single_quoted:
            escaped = True
            index += 1
            continue
        if character == "'" and not double_quoted:
            single_quoted = not single_quoted
            index += 1
            continue
        if character == '"' and not single_quoted:
            double_quoted = not double_quoted
            index += 1
            continue
        if not single_quoted and (
            character == "`"
            or command.startswith("$(", index)
            or command.startswith("<(", index)
            or command.startswith(">(", index)
        ):
            active_shell_indirection = True
            break
        index += 1
    if active_shell_indirection:
        return "shell substitution/process indirection is unsupported by the command guard"
    for simple in _simple_commands(command):
        raw = list(simple.tokens)
        while raw and _ASSIGNMENT_RE.match(raw[0]):
            raw.pop(0)
        if raw and Path(raw[0]).name == "env":
            if any(
                token == "--split-string" or token.startswith("--split-string=") or token.startswith("-S")
                for token in raw[1:]
            ):
                return "env --split-string/-S is unsupported by the command guard"
        effective = _effective_tokens(simple.tokens)
        if not effective:
            continue
        executable = effective[0] if effective[0] == "." else Path(effective[0]).name
        if executable in {"eval", "source", ".", "exec", "xargs"}:
            return f"execution indirection via {executable!r} is unsupported by the command guard"
        if executable == "find" and any(
            token in {"-exec", "-execdir", "-ok", "-okdir"} or token.startswith("-exec")
            for token in effective[1:]
        ):
            return "find -exec/-ok indirection is unsupported by the command guard"
    return None


def _git_invocation(simple: SimpleCommand, process_cwd: Path) -> Optional[GitInvocation]:
    values = list(_effective_tokens(simple.tokens))
    if not values or Path(values[0]).name != "git":
        return None
    values.pop(0)
    git_cwd = process_cwd.resolve()
    index = 0
    options_with_value = {"-c", "--config-env", "--git-dir", "--work-tree", "--namespace", "--exec-path"}
    while index < len(values):
        token = values[index]
        if token == "--":
            index += 1
            break
        if token == "-C":
            if index + 1 >= len(values):
                raise GuardFailure("git -C is missing its path")
            candidate = Path(values[index + 1])
            git_cwd = (candidate if candidate.is_absolute() else git_cwd / candidate).resolve()
            index += 2
            continue
        if token.startswith("-C") and token != "-C":
            candidate = Path(token[2:])
            git_cwd = (candidate if candidate.is_absolute() else git_cwd / candidate).resolve()
            index += 1
            continue
        if token in options_with_value:
            if index + 1 >= len(values):
                raise GuardFailure(f"git option {token} is missing its value")
            index += 2
            continue
        if token.startswith("-c") and token != "-c":
            index += 1
            continue
        if token.startswith("--git-dir=") or token.startswith("--work-tree=") or token.startswith("--namespace="):
            index += 1
            continue
        if token.startswith("-"):
            index += 1
            continue
        break
    if index >= len(values):
        return None
    return GitInvocation(tuple(_effective_tokens(simple.tokens)), values[index], tuple(values[index + 1 :]), git_cwd)


def _git_invocations(command: str, process_cwd: Path) -> List[GitInvocation]:
    result: List[GitInvocation] = []
    for simple in _simple_commands(command):
        invocation = _git_invocation(simple, process_cwd)
        if invocation is not None:
            result.append(invocation)
    return result


def _is_protected_ref(token: str) -> bool:
    value = token.lstrip("+")
    if value in PROTECTED_BRANCHES:
        return True
    if value.startswith("refs/heads/") and value.rsplit("/", 1)[-1] in PROTECTED_BRANCHES:
        return True
    if ":" in value:
        destination = value.rsplit(":", 1)[-1]
        return destination in PROTECTED_BRANCHES or (
            destination.startswith("refs/heads/") and destination.rsplit("/", 1)[-1] in PROTECTED_BRANCHES
        )
    return False


def _push_targets_protected(invocation: GitInvocation) -> bool:
    if invocation.subcommand != "push":
        return False
    args = list(invocation.args)
    if any(token in {"--all", "--mirror"} for token in args):
        return True
    if any(_is_protected_ref(token) for token in args if not token.startswith("--repo=")):
        return True

    # No explicit refspec means Git pushes the current/upstream branch.  A
    # protected current branch must never reach a remote directly.
    positional: List[str] = []
    index = 0
    options_with_value = {"--repo", "--receive-pack", "--exec", "-o", "--push-option"}
    while index < len(args):
        token = args[index]
        if token == "--":
            positional.extend(args[index + 1 :])
            break
        if token in options_with_value:
            index += 2
            continue
        if token.startswith("-"):
            index += 1
            continue
        positional.append(token)
        index += 1
    has_refspec = len(positional) >= 2
    if not has_refspec:
        branch = _git_text(invocation.cwd, "rev-parse", "--abbrev-ref", "HEAD", check=False)
        if not branch:
            raise GuardFailure("cannot prove the destination of a bare git push")
        return branch in PROTECTED_BRANCHES
    return False


def _has_option(tokens: Sequence[str], short: str, long: str) -> bool:
    for token in tokens:
        if token == long:
            return True
        if token.startswith("--"):
            continue
        if token.startswith("-") and short in token[1:]:
            return True
    return False


def _block_bash(command: str, process_cwd: Path) -> Optional[str]:
    simples = _simple_commands(command)
    for invocation in _git_invocations(command, process_cwd):
        if _push_targets_protected(invocation):
            return "direct push to protected trunk murmur/main/master; use a feature branch and PR merge"

    for simple in simples:
        values = _effective_tokens(simple.tokens)
        if not values:
            continue
        executable = Path(values[0]).name
        args = list(values[1:])

        if executable == "security":
            return "the macOS security/keychain CLI is forbidden in an agent shell; run it interactively yourself"
        if executable == "xcrun" and "notarytool" in args and "store-credentials" in args:
            return "notarytool store-credentials requires interactive keychain authorization"
        if executable == "notarytool" and "store-credentials" in args:
            return "notarytool store-credentials requires interactive keychain authorization"

        if executable == "cargo":
            cargo_args = list(args)
            if cargo_args and cargo_args[0].startswith("+"):
                cargo_args.pop(0)
            if "clippy" in cargo_args and "--all-targets" in cargo_args:
                return "cargo clippy --all-targets is forbidden in the inner loop; run cargo test --lib or scripts/ci.sh"

        if executable == "codesign" and "--deep" in args:
            return "codesign --deep skips Murmur's nested helpers; use the inside-out signing script"

        if executable == "rm":
            recursive = _has_option(args, "r", "--recursive") or _has_option(args, "R", "--recursive")
            targets = [token for token in args if token == "--" or not token.startswith("-")]
            dangerous = {"/", "//", "/*", "~", "~/", "~/*", "$HOME", "${HOME}", "$HOME/", "${HOME}/"}
            if recursive and any(token in dangerous for token in targets):
                return "recursive deletion of filesystem root or the home directory is forbidden"
    if resource_policy.command_is_heavy_in(command, process_cwd):
        return (
            "unwrapped resource-heavy build/test/dev command; run it through "
            "scripts/agent-resource-run so Murmur worktrees share one supervised lane"
        )
    return None


def _repo_for_invocation(invocation: GitInvocation) -> Path:
    top = _git_text(invocation.cwd, "rev-parse", "--show-toplevel", check=False)
    if not top:
        raise GuardFailure("cannot inspect the Git repository targeted by commit")
    return Path(top).resolve()


def _secret_hits(diff: str) -> List[str]:
    # Pattern spellings are intentionally split/character-classed so staging
    # this guard itself cannot match its own detector source.
    patterns = [
        (re.compile(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----"), "PEM private key"),
        (re.compile(r"sk[-]ant[-][A-Za-z0-9_-]{20,}"), "Anthropic API key"),
        (re.compile(r"sk[-]proj[-][A-Za-z0-9_-]{20,}"), "OpenAI project key"),
        (re.compile(r"gh[ps]_[A-Za-z0-9]{20,}"), "GitHub token"),
    ]
    hits: List[str] = []
    for pattern, label in patterns:
        if pattern.search(diff):
            hits.append(label)

    for line in diff.splitlines():
        if re.search(r"[0-9a-fA-F]{64}", line):
            placeholder = ("0123456789abcdef" * 4) in line
            documented = "MURMUR_DEV_DEK" in line or "MURMUR_DEV_KEK" in line
            if not (placeholder and documented):
                hits.append("64-hex DEK/KEK-shaped value")
                break
    return hits


def _secret_scan(command: str, process_cwd: Path) -> Optional[str]:
    commits = [item for item in _git_invocations(command, process_cwd) if item.subcommand == "commit"]
    if not commits:
        return None
    if os.environ.get("MURMUR_ALLOW_SECRET") == "1":
        return None
    for invocation in commits:
        repo = _repo_for_invocation(invocation)
        raw = _git_bytes(repo, "diff", "--cached", "--no-color", "--no-ext-diff", "--unified=0", "--")
        added = []
        for encoded in raw.splitlines():
            if encoded.startswith(b"+") and not encoded.startswith(b"+++"):
                added.append(encoded[1:].decode("utf-8", "replace"))
        hits = _secret_hits("\n".join(added))
        if hits:
            return "staged additions contain secret material: " + ", ".join(sorted(set(hits)))
    return None


def _repo_context(repo: Path) -> Tuple[Path, Path, str, str]:
    top_raw = _git_text(repo, "rev-parse", "--show-toplevel")
    top = Path(top_raw).resolve()
    common_raw = Path(_git_text(top, "rev-parse", "--git-common-dir"))
    common = common_raw.resolve() if common_raw.is_absolute() else (top / common_raw).resolve()
    head = _git_text(top, "rev-parse", "HEAD")
    branch = _git_text(top, "rev-parse", "--abbrev-ref", "HEAD")
    return top, common, head, branch


def _primary_worktree(repo: Path) -> Path:
    listing = _git_text(repo, "worktree", "list", "--porcelain").splitlines()
    first = next((line for line in listing if line.startswith("worktree ")), "")
    if not first:
        raise GuardFailure("cannot resolve the canonical primary worktree")
    return Path(first.split(" ", 1)[1]).resolve()


def _manifest_worktree(document: Any) -> Optional[Path]:
    if not isinstance(document, dict) or not isinstance(document.get("worktree_path"), str):
        return None
    return Path(document["worktree_path"]).resolve()


def _resolve_task(repo: Path, common: Path) -> Tuple[Dict[str, Any], Path]:
    tasks_root = common / "agent-harness" / "tasks"
    explicit = os.environ.get("MURMUR_AGENT_TASK_ID", "").strip()
    if explicit:
        if not TASK_ID_RE.fullmatch(explicit):
            raise GuardFailure("MURMUR_AGENT_TASK_ID is invalid")
        task_dir = tasks_root / explicit
        document = _load_json(task_dir / "task.json")
        if _manifest_worktree(document) != repo.resolve():
            raise GuardFailure("explicit task manifest belongs to a different worktree")
        return document, task_dir

    matches: List[Tuple[Dict[str, Any], Path]] = []
    if tasks_root.is_dir():
        for manifest in sorted(tasks_root.glob("*/task.json")):
            try:
                document = _load_json(manifest)
            except GuardFailure:
                continue
            if _manifest_worktree(document) == repo.resolve():
                matches.append((document, manifest.parent))
    if len(matches) > 1:
        raise GuardFailure("multiple task manifests claim this worktree")
    if len(matches) == 1:
        return matches[0]

    # Legacy pointer is only a fallback and can never redirect to another
    # worktree.  Concurrency-safe discovery above remains authoritative.
    pointer = common / "agent-harness" / "current-task"
    if pointer.is_file():
        try:
            raw = pointer.read_text(encoding="utf-8").strip()
            if raw.startswith("{"):
                parsed = json.loads(raw)
                raw = parsed.get("task_id", "") if isinstance(parsed, dict) else ""
        except (OSError, json.JSONDecodeError) as exc:
            raise GuardFailure(f"invalid legacy current-task pointer: {exc}") from exc
        if TASK_ID_RE.fullmatch(raw):
            task_dir = tasks_root / raw
            document = _load_json(task_dir / "task.json")
            if _manifest_worktree(document) == repo.resolve():
                return document, task_dir
            raise GuardFailure("legacy current-task pointer belongs to a different worktree")
    raise GuardFailure("no task manifest matches this worktree; use scripts/agent-harness init/run")


def _parse_time(raw: Any, label: str) -> dt.datetime:
    if not isinstance(raw, str):
        raise GuardFailure(f"{label} is not a timestamp")
    try:
        parsed = dt.datetime.fromisoformat(raw.replace("Z", "+00:00"))
    except ValueError as exc:
        raise GuardFailure(f"{label} is not an ISO date-time") from exc
    if parsed.tzinfo is None:
        raise GuardFailure(f"{label} must include a timezone")
    return parsed.astimezone(dt.timezone.utc)


def _staged_paths(repo: Path) -> List[str]:
    raw = _git_bytes(repo, "diff", "--cached", "--name-only", "-z", "--no-renames", "--")
    return sorted(item.decode("utf-8", "surrogateescape") for item in raw.split(b"\x00") if item)


def _path_is_owned(path: str, owned: Iterable[str]) -> bool:
    for entry in owned:
        if any(character in entry for character in "*?["):
            if fnmatch.fnmatchcase(path, entry):
                return True
        elif path == entry or path.startswith(entry.rstrip("/") + "/"):
            return True
    return False


def _classify_actual_risks(paths: Sequence[str], config: Mapping[str, Any]) -> List[str]:
    result: List[str] = []
    classification = config.get("risk_classification", {})
    if not isinstance(classification, dict):
        raise GuardFailure("harness config risk_classification is malformed")
    for risk, patterns in classification.items():
        if not isinstance(risk, str) or not isinstance(patterns, list):
            raise GuardFailure("harness config risk_classification is malformed")
        if any(
            isinstance(pattern, str) and fnmatch.fnmatchcase(path, pattern)
            for path in paths
            for pattern in patterns
        ):
            result.append(risk)
    return result


def _validate_provenance(
    attestation: Mapping[str, Any],
    task: Mapping[str, Any],
    *,
    allow_test_adapter: bool = False,
) -> None:
    writer = attestation["writer"]
    reviewer = attestation["reviewer"]
    writer_vendor = task.get("writer")
    reviewer_vendor = task.get("reviewer")
    if allow_test_adapter and writer_vendor == reviewer_vendor == "fake":
        pass
    elif writer_vendor not in {"codex", "claude"} or reviewer_vendor not in {"codex", "claude"}:
        raise GuardFailure("fake or unknown model vendors are forbidden outside harness selftests")
    elif writer_vendor == reviewer_vendor:
        raise GuardFailure("writer and reviewer must use different vendors")
    if writer.get("vendor") != task.get("writer"):
        raise GuardFailure("attestation writer does not match the task contract")
    if reviewer.get("vendor") != task.get("reviewer"):
        raise GuardFailure("attestation reviewer does not match the task contract")
    if writer.get("round") != attestation.get("rounds"):
        raise GuardFailure("writer round does not match attestation rounds")

    for label, identity in (("writer", writer), ("reviewer", reviewer)):
        for field in ("cli_version", "model"):
            value = identity.get(field)
            if not isinstance(value, str) or not value:
                raise GuardFailure(f"{label} provenance field {field} is empty")
    writer_session = writer.get("session_id")
    if not isinstance(writer_session, str) or not writer_session:
        raise GuardFailure("writer provenance session_id is empty")


def _validate_attestation(
    repo: Path,
    common: Path,
    task: Dict[str, Any],
    task_dir: Path,
    *,
    allow_test_adapter: bool = False,
) -> None:
    runner = _task_runner_module()
    try:
        runner.validate_schema(task, runner.load_schema("task"), label="task contract")
    except Exception as exc:
        raise GuardFailure(str(exc)) from exc
    if runner.contract_hash(task) != task.get("contract_sha256"):
        raise GuardFailure("task contract hash is malformed or stale")

    top, actual_common, head, branch = _repo_context(repo)
    if actual_common != common.resolve():
        raise GuardFailure("Git common directory changed while validating the task")
    expected_paths = {
        "repo_realpath": _primary_worktree(top),
        "worktree_path": top,
        "git_common_dir": common.resolve(),
    }
    for key, expected in expected_paths.items():
        if Path(str(task.get(key, ""))).resolve() != expected:
            raise GuardFailure(f"task {key} does not match the current repository")
    if task.get("branch") != branch:
        raise GuardFailure("task branch does not match the current worktree branch")
    if not SHA1_RE.fullmatch(str(task.get("base_sha", ""))):
        raise GuardFailure("task base_sha is malformed")
    ancestor = _run(["git", "merge-base", "--is-ancestor", str(task["base_sha"]), head], top, check=False)
    if ancestor.returncode != 0:
        raise GuardFailure("task base_sha is not an ancestor of current HEAD")

    current_instructions = runner.instructions_hash(top)
    current_dependencies = runner.dependency_revisions(top)
    if task.get("instructions_sha256") != current_instructions:
        raise GuardFailure("task instructions fingerprint is stale")
    if task.get("dependency_revisions") != current_dependencies:
        raise GuardFailure("task dependency revisions are stale")

    attestation = _load_json(task_dir / "attestation.json")
    try:
        runner.validate_schema(attestation, runner.load_schema("attestation"), label="attestation")
    except Exception as exc:
        raise GuardFailure(str(exc)) from exc
    if attestation.get("verdict") != "PASS":
        raise GuardFailure("attestation verdict is not PASS")

    for key in (
        "task_id",
        "contract_sha256",
        "instructions_sha256",
        "dependency_revisions",
        "base_sha",
        "repo_realpath",
        "worktree_path",
    ):
        if attestation.get(key) != task.get(key):
            raise GuardFailure(f"attestation {key} does not match the task contract")
    if attestation.get("head_sha") != head:
        raise GuardFailure("attestation HEAD is stale")

    current_diff = runner.staged_diff(top)
    current_diff_hash = _sha256(current_diff)
    current_tree = _git_text(top, "write-tree")
    if attestation.get("staged_diff_sha256") != current_diff_hash:
        raise GuardFailure("attestation staged diff hash is stale")
    if attestation.get("tree_sha") != current_tree:
        raise GuardFailure("attestation index tree is stale")
    if attestation.get("instructions_sha256") != current_instructions:
        raise GuardFailure("attestation instructions fingerprint is stale")
    if attestation.get("dependency_revisions") != current_dependencies:
        raise GuardFailure("attestation dependency revisions are stale")

    paths = _staged_paths(top)
    owned = task.get("owned_paths", [])
    violations = [path for path in paths if not _path_is_owned(path, owned)]
    if violations:
        raise GuardFailure("staged paths exceed task ownership: " + ", ".join(violations))
    if task.get("expected_change") and not current_diff:
        raise GuardFailure("task requires a change, but staged diff is empty")
    if not task.get("expected_change") and current_diff:
        raise GuardFailure("no-change task unexpectedly has a staged diff")

    if _git_bytes(top, "diff", "--binary", "--no-ext-diff", "--").strip():
        raise GuardFailure("unstaged tracked changes make the receipt incomplete")
    if _git_bytes(top, "ls-files", "--others", "--exclude-standard", "-z").strip(b"\x00"):
        raise GuardFailure("untracked files make the receipt incomplete")

    config = _load_json(top / ".agents" / "harness" / "config.json") if (top / ".agents" / "harness" / "config.json").is_file() else runner.load_config()
    actual_risks = _classify_actual_risks(paths, config)
    task_risks = task.get("risk_flags", [])
    attested_risks = attestation.get("risk_flags", [])
    if not isinstance(task_risks, list) or not isinstance(attested_risks, list):
        raise GuardFailure("risk flags are malformed")
    required_risks = set(task_risks) | set(actual_risks)
    if not required_risks.issubset(set(attested_risks)):
        missing = sorted(required_risks - set(attested_risks))
        raise GuardFailure("attestation is missing automatically classified risks: " + ", ".join(missing))

    _validate_provenance(attestation, task, allow_test_adapter=allow_test_adapter)
    rounds = attestation.get("rounds")
    if not isinstance(rounds, int) or rounds < 1 or rounds > int(task.get("max_repair_rounds", 0)) + 1:
        raise GuardFailure("attestation rounds exceed the bounded repair loop")

    task_created = _parse_time(task.get("created_at"), "task.created_at")
    receipt_created = _parse_time(attestation.get("created_at"), "attestation.created_at")
    if receipt_created < task_created or receipt_created > dt.datetime.now(dt.timezone.utc) + dt.timedelta(minutes=5):
        raise GuardFailure("attestation timestamp is stale or from the future")

    check_map: Dict[str, Mapping[str, Any]] = {}
    for check in attestation.get("checks", []):
        check_id = check.get("id")
        if not isinstance(check_id, str) or check_id in check_map:
            raise GuardFailure("attestation checks have a missing or duplicate id")
        if check.get("exit_code") != 0:
            raise GuardFailure(f"attested check {check_id} is not green")
        log_path = Path(str(check.get("log_path", "")))
        if not log_path.is_absolute():
            log_path = task_dir / log_path
        try:
            log_path.resolve().relative_to(task_dir.resolve())
        except (OSError, ValueError) as exc:
            raise GuardFailure(f"attested check {check_id} log is outside the task store") from exc
        if not log_path.is_file():
            raise GuardFailure(f"attested check {check_id} log is missing")
        stdout_path = log_path.with_suffix(".stdout.log")
        stderr_path = log_path.with_suffix(".stderr.log")
        for stream, stream_path, hash_field in (
            ("stdout", stdout_path, "stdout_sha256"),
            ("stderr", stderr_path, "stderr_sha256"),
        ):
            try:
                stream_path.resolve().relative_to(task_dir.resolve())
            except (OSError, ValueError) as exc:
                raise GuardFailure(f"attested check {check_id} {stream} is outside the task store") from exc
            if not stream_path.is_file() or _sha256(stream_path.read_bytes()) != check.get(hash_field):
                raise GuardFailure(f"attested check {check_id} {stream} is missing or changed")
        check_map[check_id] = check

    declared_checks: Dict[str, str] = {}
    for check in list(task.get("checks", [])) + list(task.get("final_checks", [])):
        check_id = check.get("id")
        command = check.get("command")
        if check_id in declared_checks and declared_checks[check_id] != command:
            raise GuardFailure(f"task reuses check id {check_id} with another command")
        declared_checks[check_id] = command
    for check_id, command in declared_checks.items():
        evidence = check_map.get(check_id)
        if evidence is None:
            raise GuardFailure(f"required check evidence is missing: {check_id}")
        if evidence.get("command") != command:
            raise GuardFailure(f"attested command differs for check {check_id}")

    risk_evidence = config.get("risk_required_evidence", {})
    for risk in required_risks:
        for check_id in risk_evidence.get(risk, []):
            if check_id not in check_map or check_map[check_id].get("exit_code") != 0:
                raise GuardFailure(f"risk {risk} requires green evidence check {check_id}")

    review_map: Dict[str, Mapping[str, Any]] = {}
    writer_session = attestation["writer"]["session_id"]
    reviewer_sessions: set = set()
    for review in attestation.get("reviews", []):
        kind = review.get("kind")
        if not isinstance(kind, str) or kind in review_map:
            raise GuardFailure("attestation reviews have a missing or duplicate kind")
        if review.get("verdict") != "PASS":
            raise GuardFailure(f"review {kind} is not PASS")
        if review.get("staged_diff_sha256") != current_diff_hash:
            raise GuardFailure(f"review {kind} was made against another staged diff")
        review_created = _parse_time(review.get("created_at"), f"review[{kind}].created_at")
        if review_created < task_created or review_created > receipt_created:
            raise GuardFailure(f"review {kind} timestamp is stale")
        reviewer_run = review.get("reviewer", {})
        session_id = reviewer_run.get("session_id") if isinstance(reviewer_run, dict) else None
        if not isinstance(session_id, str) or not session_id:
            raise GuardFailure(f"review {kind} has no reviewer session provenance")
        if session_id == writer_session:
            raise GuardFailure(f"review {kind} reused the writer session")
        if session_id in reviewer_sessions:
            raise GuardFailure(f"review {kind} reused another review session")
        reviewer_sessions.add(session_id)
        for field in ("vendor", "cli_version", "model"):
            value = reviewer_run.get(field) if isinstance(reviewer_run, dict) else None
            if not isinstance(value, str) or not value:
                raise GuardFailure(f"review {kind} provenance field {field} is empty")
        if reviewer_run.get("vendor") != task.get("reviewer"):
            raise GuardFailure(f"review {kind} vendor does not match the independent reviewer contract")
        if not SHA256_RE.fullmatch(str(review.get("artifact_sha256", ""))):
            raise GuardFailure(f"review {kind} artifact hash is malformed")
        artifact_hash = review["artifact_sha256"]
        artifacts = sorted((task_dir / "results").glob("*.json"))
        if not any(path.is_file() and _sha256(path.read_bytes()) == artifact_hash for path in artifacts):
            raise GuardFailure(f"review {kind} artifact is missing or changed")
        review_map[kind] = review

    required_reviews = list(config.get("required_reviews", []))
    risk_review_mapping = config.get("risk_reviews", {})
    for risk in required_risks:
        review_name = risk_review_mapping.get(risk)
        if isinstance(review_name, str) and review_name not in required_reviews:
            required_reviews.append(review_name)
    for kind in required_reviews:
        if kind not in review_map:
            raise GuardFailure(f"required automatic review is missing: {kind}")

    # The runner is the canonical verifier used by the explicit verify and
    # commit commands. Hooks may add contextual checks, but may never accept a
    # receipt that the canonical verifier rejects.
    try:
        runner.verify_attestation(task, task_dir, allow_test_adapter=allow_test_adapter)
    except Exception as exc:
        raise GuardFailure(str(exc)) from exc


def _finish_guard(
    command: str, process_cwd: Path, *, allow_test_adapter: bool = False
) -> Optional[str]:
    commits = [item for item in _git_invocations(command, process_cwd) if item.subcommand == "commit"]
    if not commits:
        return None
    mode = os.environ.get("MURMUR_FINISH_GUARD", "enforce").strip().lower()
    if mode == "off":
        return None
    if mode not in {"enforce", "advisory"}:
        return "MURMUR_FINISH_GUARD must be enforce, advisory, or off"
    if len(commits) != 1:
        reason = "one hook invocation may target only one git commit"
    else:
        try:
            repo = _repo_for_invocation(commits[0])
            _, common, _, _ = _repo_context(repo)
            task, task_dir = _resolve_task(repo, common)
            _validate_attestation(
                repo,
                common,
                task,
                task_dir,
                allow_test_adapter=allow_test_adapter,
            )
            return None
        except GuardFailure as exc:
            reason = str(exc)
    if mode == "advisory":
        print(f"finish-guard advisory: {reason}", file=sys.stderr)
        return None
    return "Definition-of-Done receipt rejected: " + reason


def _emit(vendor: str, reason: Optional[str], guard: str) -> int:
    if reason is None:
        return 0
    message = f"{guard} refused this command: {reason}"
    if vendor == "codex":
        print(json.dumps({"decision": "block", "reason": message}, separators=(",", ":")))
        return 0
    print(f"BLOCK: {message}", file=sys.stderr)
    return 2


class _Selftest:
    def __init__(self) -> None:
        self.failures: List[str] = []
        self.count = 0

    def result(self, label: str, got: str, want: str) -> None:
        self.count += 1
        marker = "PASS" if got == want else "FAIL"
        print(f"  [{marker}] {label}: {got}")
        if got != want:
            self.failures.append(f"{label}: got {got}, want {want}")

    @staticmethod
    def payload(vendor: str, command: str, session_cwd: Path, workdir: Optional[Path] = None) -> bytes:
        tool_input: Dict[str, Any] = {"command": command}
        if workdir is not None:
            tool_input["workdir"] = str(workdir)
        if vendor == "codex":
            document = {
                "hook_event_name": "PreToolUse",
                "tool_name": "Bash",
                "cwd": str(session_cwd),
                "tool_input": tool_input,
            }
        else:
            document = {
                "session_id": "selftest",
                "tool_name": "Bash",
                "cwd": str(session_cwd),
                "tool_input": tool_input,
            }
        return json.dumps(document).encode("utf-8")

    def invoke(
        self,
        vendor: str,
        action: str,
        command: str,
        cwd: Path,
        *,
        extra_env: Optional[Mapping[str, str]] = None,
        use_default_finish: bool = False,
        payload_workdir: Optional[Path] = None,
        allow_test_adapter: bool = False,
    ) -> Tuple[str, subprocess.CompletedProcess]:
        if allow_test_adapter:
            if action != "finish-guard":
                raise RuntimeError("the internal test adapter is valid only for finish-guard selftests")
            reason = _finish_guard(
                command,
                payload_workdir or cwd,
                allow_test_adapter=True,
            )
            completed = subprocess.CompletedProcess([], 2 if reason else 0, b"", b"")
            return ("BLOCK" if reason else "ALLOW"), completed
        wrapper = SOURCE_ROOT / f".{vendor}" / "hooks" / f"{action}.sh"
        env = os.environ.copy()
        env.pop("MURMUR_AGENT_TASK_ID", None)
        env.pop("MURMUR_ALLOW_SECRET", None)
        if use_default_finish:
            env.pop("MURMUR_FINISH_GUARD", None)
        else:
            env["MURMUR_FINISH_GUARD"] = "enforce"
        if extra_env:
            env.update(extra_env)
        completed = subprocess.run(
            [str(wrapper)],
            cwd=str(cwd),
            env=env,
            input=self.payload(vendor, command, cwd, payload_workdir),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        blocked = completed.returncode == 2
        if vendor == "codex":
            try:
                output = json.loads(completed.stdout.decode("utf-8")) if completed.stdout else {}
            except json.JSONDecodeError:
                output = {}
            blocked = blocked or output.get("decision") == "block"
        return ("BLOCK" if blocked else "ALLOW"), completed

    def expect(self, label: str, vendor: str, action: str, command: str, cwd: Path, want: str) -> None:
        got, _ = self.invoke(vendor, action, command, cwd)
        self.result(f"{vendor}: {label}", got, want)


def _init_repo(path: Path) -> None:
    path.mkdir(parents=True, exist_ok=True)
    _run(["git", "init", "-q", "-b", "feature/selftest"], path)
    _run(["git", "config", "user.name", "Harness Selftest"], path)
    _run(["git", "config", "user.email", "harness@example.invalid"], path)
    (path / "base.txt").write_text("base\n", encoding="utf-8")
    cargo_src = path / "src-tauri" / "src"
    cargo_src.mkdir(parents=True)
    (path / "src-tauri" / "Cargo.toml").write_text(
        '[package]\nname="hook-selftest"\nversion="0.0.0"\nedition="2021"\n',
        encoding="utf-8",
    )
    (cargo_src / "lib.rs").write_text(
        "#[cfg(test)] mod tests { #[test] fn smoke() { assert_eq!(2 + 2, 4); } }\n",
        encoding="utf-8",
    )
    _run(["git", "add", "base.txt", "src-tauri/Cargo.toml", "src-tauri/src/lib.rs"], path)
    _run(["git", "commit", "-q", "-m", "base"], path)


def _secret_case(test: _Selftest, vendor: str, relative: str, content: str, want: str, command: str = "git commit -m x") -> None:
    with tempfile.TemporaryDirectory(prefix="murmur-hook-secret-") as temp:
        repo = Path(temp) / "repo"
        _init_repo(repo)
        target = repo / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(content + "\n", encoding="utf-8")
        _run(["git", "add", "--", relative], repo)
        got, _ = test.invoke(vendor, "secret-scan", command, repo)
        test.result(f"{vendor}: secret in {relative}", got, want)


def _write_receipt(repo: Path, task_id: str, *, risk: bool = False) -> Tuple[Path, Dict[str, Any], Dict[str, Any]]:
    runner = _task_runner_module()
    top, common, head, branch = _repo_context(repo)
    task_dir = common / "agent-harness" / "tasks" / task_id
    task_dir.mkdir(parents=True, exist_ok=True)
    now = dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
    checks = [{"id": "unit", "command": "test -f owned.txt", "timeout_seconds": 30}]
    if risk:
        checks.append(
            {
                "id": "rust-lib",
                "command": runner.canonical_check_commands(runner.load_config())["rust-lib"],
                "timeout_seconds": 60,
            }
        )
    task: Dict[str, Any] = {
        "schema_version": 1,
        "task_id": task_id,
        "description": "hook guard selftest",
        "kind": "harness",
        "base_sha": head,
        "contract_sha256": "",
        "instructions_sha256": runner.instructions_hash(top),
        "dependency_revisions": runner.dependency_revisions(top),
        "repo_realpath": str(top),
        "git_common_dir": str(common),
        "worktree_path": str(top),
        "branch": branch,
        "owned_paths": ["owned.txt"],
        "risk_flags": ["lock"] if risk else [],
        "writer": "fake",
        "reviewer": "fake",
        "max_repair_rounds": 2,
        "checks": checks,
        "final_checks": [],
        "expected_change": True,
        "created_at": now,
    }
    task["contract_sha256"] = runner.contract_hash(task)
    (task_dir / "task.json").write_text(json.dumps(task, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    diff = runner.staged_diff(top)
    diff_hash = _sha256(diff)
    evidence = [runner.run_check(top, task_dir, check, "round-01") for check in checks]
    if any(not check["passed"] for check in evidence):
        raise GuardFailure("hook selftest deterministic evidence unexpectedly failed")
    review_kinds = ["spec", "adversarial"] + (["lock-security"] if risk else [])
    reviews = []
    writer_run = runner.invoke_model(
        "fake",
        role="writer",
        prompt="hook guard selftest writer",
        schema_name="model-result",
        worktree=top,
        task_dir=task_dir,
        label="round-01-writer",
        timeout_seconds=30,
        instructions_sha256=task["instructions_sha256"],
    )
    for kind in review_kinds:
        model_review = runner.invoke_model(
            "fake",
            role="reviewer",
            prompt=f"hook guard selftest {kind}",
            schema_name="review",
            worktree=top,
            task_dir=task_dir,
            label=f"round-01-{kind}",
            timeout_seconds=30,
            instructions_sha256=task["instructions_sha256"],
        )
        review = runner._review_evidence(model_review)
        review.update({"kind": kind, "staged_diff_sha256": diff_hash, "created_at": runner.utc_now()})
        reviews.append(review)
    (task_dir / "diffs").mkdir(exist_ok=True)
    (task_dir / "diffs" / "attested.diff").write_bytes(diff)
    attestation = runner.create_attestation(task, top, 1, evidence, reviews, [writer_run])
    (task_dir / "attestation.json").write_text(json.dumps(attestation, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    runner.set_state(
        task_dir,
        "PASSED",
        round=1,
        phase="complete",
        attestation=str(task_dir / "attestation.json"),
        staged_diff_sha256=attestation["staged_diff_sha256"],
        tree_sha=attestation["tree_sha"],
    )
    return task_dir, task, attestation


def _finish_repo() -> Path:
    temp = Path(tempfile.mkdtemp(prefix="murmur-hook-finish-"))
    repo = temp / "repo"
    _init_repo(repo)
    (repo / "owned.txt").write_text("changed\n", encoding="utf-8")
    _run(["git", "add", "owned.txt"], repo)
    return repo


def _linked_worktree_runner_case(test: _Selftest) -> None:
    """Prove an attestation made by the real runner passes from its linked WT."""

    with tempfile.TemporaryDirectory(prefix="murmur-hook-linked-worktree-") as temp:
        repo = Path(temp) / "repo"
        _init_repo(repo)
        runner = _task_runner_module()
        init_args = argparse.Namespace(
            task_id="hook-linked-integration",
            kind="harness",
            agent="fake",
            reviewer="fake",
            prompt="linked worktree hook integration",
            owned=["base.txt"],
            risk=[],
            check=["unit::test -f base.txt"],
            final_check=["final::test -f base.txt"],
            max_repair_rounds=2,
            base=None,
            branch=None,
            expected_change=False,
            quiet=True,
            _allow_test_adapter=True,
        )
        previous_cwd = Path.cwd()
        try:
            os.chdir(repo)
            runner.cmd_init(init_args)
            contract, task_dir, _ = runner.load_task_from_current_repo(
                "hook-linked-integration", repo
            )
            worktree = Path(contract["worktree_path"]).resolve()
            if runner.run_task(contract, task_dir, allow_test_adapter=True) != "PASSED":
                test.result("runner linked-worktree run", "FAIL", "PASS")
                return
        except Exception as exc:
            test.result("runner linked-worktree init", f"FAIL: {exc}", "PASS")
            return
        finally:
            os.chdir(previous_cwd)
        _, common, _, _ = _repo_context(worktree)
        task_dir = common / "agent-harness" / "tasks" / "hook-linked-integration"
        task = _load_json(task_dir / "task.json")
        paths_match = (
            Path(str(task.get("repo_realpath", ""))).resolve() == repo.resolve()
            and Path(str(task.get("worktree_path", ""))).resolve() == worktree
            and worktree != repo.resolve()
        )
        test.result("runner receipt identifies linked worktree", "PASS" if paths_match else "FAIL", "PASS")
        for vendor in ("codex", "claude"):
            got, _ = test.invoke(
                vendor,
                "finish-guard",
                "git commit -m integration",
                worktree,
                allow_test_adapter=True,
            )
            test.result(f"{vendor}: real runner linked receipt", got, "ALLOW")
            got, _ = test.invoke(
                vendor,
                "finish-guard",
                "git commit -m integration",
                repo,
                payload_workdir=worktree,
                allow_test_adapter=True,
            )
            test.result(f"{vendor}: payload workdir selects linked receipt", got, "ALLOW")

        artifact = next((task_dir / "results").glob("*spec*.json"), None)
        if artifact is None:
            test.result("runner review artifact exists", "FAIL", "PASS")
            return
        artifact.write_text('{"tampered":true}\n', encoding="utf-8")
        got, _ = test.invoke(
            "codex",
            "finish-guard",
            "git commit -m integration",
            worktree,
            allow_test_adapter=True,
        )
        test.result("codex: tampered runner artifact", got, "BLOCK")


def _resource_lane_runner_cases(test: _Selftest) -> None:
    """Exercise the bounded supervisor without launching any product build."""

    runner = SOURCE_ROOT / "scripts" / "agent-resource-run"
    test.result(
        "resource lane runner is executable",
        "PASS" if runner.is_file() and os.access(runner, os.X_OK) else "FAIL",
        "PASS",
    )
    with tempfile.TemporaryDirectory(prefix="murmur-resource-lane-") as temp:
        repo = Path(temp) / "repo"
        repo.mkdir()
        _run(["git", "init", "-q"], repo)

        completed = subprocess.run(
            [str(runner), "--deadline-seconds", "3", "--", "sh", "-c", "exit 0"],
            cwd=str(repo),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        test.result(
            "resource lane runner executes a light command",
            "PASS" if completed.returncode == 0 else f"FAIL({completed.returncode})",
            "PASS",
        )

        completed = subprocess.run(
            [
                str(runner),
                "--deadline-seconds",
                "3",
                "--",
                "sh",
                "-c",
                'test "$CARGO_BUILD_JOBS" = 2 && test "$RUST_TEST_THREADS" = 1 && '
                'test "$RAYON_NUM_THREADS" = 2 && test "$OMP_NUM_THREADS" = 1 && '
                'test "$VECLIB_MAXIMUM_THREADS" = 1',
            ],
            cwd=str(repo),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        test.result(
            "resource lane runner injects bounded defaults",
            "PASS" if completed.returncode == 0 else f"FAIL({completed.returncode})",
            "PASS",
        )

        started = dt.datetime.now(dt.timezone.utc)
        completed = subprocess.run(
            [
                str(runner),
                "--deadline-seconds",
                "0.20",
                "--term-grace-seconds",
                "0.05",
                "--",
                "sh",
                "-c",
                "sleep 5",
            ],
            cwd=str(repo),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=5,
        )
        elapsed = (dt.datetime.now(dt.timezone.utc) - started).total_seconds()
        test.result(
            "resource lane runner enforces aggregate deadline",
            "PASS" if completed.returncode == 124 and elapsed < 3 else f"FAIL({completed.returncode})",
            "PASS",
        )


def _run_selftest() -> int:
    test = _Selftest()
    print("-- canonical command guard (Claude + Codex payloads) --")
    for vendor in ("codex", "claude"):
        with tempfile.TemporaryDirectory(prefix="murmur-hook-command-") as temp:
            repo = Path(temp) / "repo"
            _init_repo(repo)
            cases = [
                ("git push protected", "git push origin HEAD:main", "BLOCK"),
                ("git global options", "git -c user.name=x -C . --no-pager push origin murmur", "BLOCK"),
                ("multiline push", "git -c user.name=x \\" + "\n --no-pager push origin master", "BLOCK"),
                ("feature push", "git push -u origin feature/x", "ALLOW"),
                ("absolute security", "/usr/bin/security find-identity -v", "BLOCK"),
                ("sudo security", "sudo /usr/bin/security unlock-keychain login.keychain", "BLOCK"),
                ("security management text", "pkill security", "ALLOW"),
                ("notary credential store", "xcrun notarytool store-credentials murmur", "BLOCK"),
                ("notary submit", "xcrun notarytool submit app.dmg --wait", "ALLOW"),
                ("cargo toolchain clippy", "cargo +stable clippy --all-targets", "BLOCK"),
                ("cargo test", "cargo test --lib", "BLOCK"),
                ("codesign deep", "/usr/bin/codesign --deep --sign X app", "BLOCK"),
                ("codesign helper", "/usr/bin/codesign --options runtime --sign X helper", "ALLOW"),
                ("root delete", "/bin/rm -rf -- /", "BLOCK"),
                ("scoped delete", "/bin/rm -rf target/tmp", "ALLOW"),
                ("PR creation", "gh pr create --base murmur --title x", "ALLOW"),
                ("quoted push search", "rg 'git push origin murmur' .", "ALLOW"),
                ("quoted security search", "rg 'security find-identity' .", "ALLOW"),
            ]
            for label, command, want in cases:
                test.expect(label, vendor, "block-bash", command, repo, want)

            indirections = [
                ("env split-string", "env -S 'git push origin murmur'"),
                ("eval", "eval 'git commit -m x'"),
                ("source", "source ./guard-bypass.sh"),
                ("dot source", ". ./guard-bypass.sh"),
                ("exec", "exec git push origin main"),
                ("xargs", "printf git | xargs git push origin main"),
                ("find exec", "find . -exec git push origin main \\;"),
                ("command substitution", "$(printf git) push origin main"),
            ]
            for action in ("block-bash", "secret-scan", "finish-guard"):
                for label, command in indirections:
                    test.expect(f"{action} rejects {label}", vendor, action, command, repo, "BLOCK")
                test.expect(f"{action} permits direct command", vendor, action, "git status --short", repo, "ALLOW")
                test.expect(
                    f"{action} permits quoted indirection text",
                    vendor,
                    action,
                    "rg '$(literal only)' .",
                    repo,
                    "ALLOW",
                )

        resource_cases = [
            ("direct cargo metadata", "cargo metadata --no-deps", "BLOCK"),
            ("direct Rust test", "cd src-tauri && cargo test --lib", "BLOCK"),
            ("direct Angular build", "npx ng build", "BLOCK"),
            ("direct npm dev", "npm run dev", "BLOCK"),
            ("direct full CI", "bash scripts/ci.sh", "BLOCK"),
            ("read-only cargo search", "rg 'cargo test --lib' .", "ALLOW"),
            (
                "lane-wrapped Rust test",
                "scripts/agent-resource-run --chdir src-tauri -- cargo test --lib",
                "ALLOW",
            ),
            (
                "lane-wrapped full CI",
                "scripts/agent-resource-run -- bash scripts/ci.sh",
                "ALLOW",
            ),
            (
                "lookalike lane runner",
                "/tmp/scripts/agent-resource-run -- cargo test --lib",
                "BLOCK",
            ),
            (
                "test-only guardian env",
                "MURMUR_AGENT_SELFTEST_GUARDIAN_RELEASE=/tmp/x scripts/agent-resource-run -- true",
                "BLOCK",
            ),
        ]
        for label, command, want in resource_cases:
            test.expect(label, vendor, "block-bash", command, SOURCE_ROOT, want)

    print("-- staged secret scan (no path exclusions) --")
    for vendor in ("codex", "claude"):
        _secret_case(test, vendor, "plain.txt", "token=" + "sk" + "-ant-" + "A" * 28, "BLOCK")
        _secret_case(test, vendor, "project.txt", "token=" + "sk" + "-proj-" + "B" * 28, "BLOCK")
        _secret_case(test, vendor, "github.txt", "token=gh" + "p_" + "C" * 36, "BLOCK")
        _secret_case(test, vendor, "Cargo.lock", "checksum=" + "c" * 64, "BLOCK")
        _secret_case(test, vendor, ".codex/hooks/fixture.sh", "token=gh" + "s_" + "D" * 24, "BLOCK")
        _secret_case(test, vendor, "private.txt", "-----BEGIN " + "OPENSSH PRIVATE KEY-----", "BLOCK")
        _secret_case(
            test,
            vendor,
            "dev.env",
            "MURMUR_DEV_DEK=" + "0123456789abcdef" * 4,
            "ALLOW",
        )
        _secret_case(test, vendor, "plain.txt", "normal source text", "ALLOW", "git -c user.name=x commit -m x")

    print("-- hash-bound finish gate --")
    for vendor in ("codex", "claude"):
        repo = _finish_repo()
        got, _ = test.invoke(
            vendor,
            "finish-guard",
            "git -c user.name=x commit -m x",
            repo,
            use_default_finish=True,
        )
        test.result(f"{vendor}: default-enforce missing manifest", got, "BLOCK")

        # Malformed manifest is found by explicit task id and must fail closed.
        _, common, _, _ = _repo_context(repo)
        malformed_dir = common / "agent-harness" / "tasks" / "malformed"
        malformed_dir.mkdir(parents=True)
        (malformed_dir / "task.json").write_text("{", encoding="utf-8")
        got, _ = test.invoke(
            vendor,
            "finish-guard",
            "git commit -m x",
            repo,
            extra_env={"MURMUR_AGENT_TASK_ID": "malformed"},
        )
        test.result(f"{vendor}: malformed task manifest", got, "BLOCK")

        task_dir, task, attestation = _write_receipt(repo, "fresh")
        got, _ = test.invoke(vendor, "finish-guard", "git commit -m x", repo)
        test.result(f"{vendor}: production rejects fake receipt", got, "BLOCK")
        got, _ = test.invoke(
            vendor,
            "finish-guard",
            "git commit -m x",
            repo,
            allow_test_adapter=True,
        )
        test.result(f"{vendor}: internal selftest accepts fresh receipt", got, "ALLOW")

        (task_dir / "attestation.json").write_text('{"verdict":"PASS"}\n', encoding="utf-8")
        got, _ = test.invoke(
            vendor, "finish-guard", "git commit -m x", repo, allow_test_adapter=True
        )
        test.result(f"{vendor}: minimal receipt", got, "BLOCK")

        (task_dir / "attestation.json").write_text(json.dumps(attestation, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        attestation["reviews"][0]["verdict"] = "FAIL"
        (task_dir / "attestation.json").write_text(json.dumps(attestation, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        got, _ = test.invoke(
            vendor, "finish-guard", "git commit -m x", repo, allow_test_adapter=True
        )
        test.result(f"{vendor}: failed review", got, "BLOCK")

        # Rebuild a fresh receipt, then mutate the staged tree.
        task_dir, task, attestation = _write_receipt(repo, "fresh")
        (repo / "owned.txt").write_text("changed again\n", encoding="utf-8")
        _run(["git", "add", "owned.txt"], repo)
        got, _ = test.invoke(
            vendor, "finish-guard", "git commit -m x", repo, allow_test_adapter=True
        )
        test.result(f"{vendor}: changed staged hash", got, "BLOCK")

        # Remove the non-risk task so discovery remains unambiguous, then prove
        # path/risk-required reviews and evidence cannot be omitted.
        import shutil

        shutil.rmtree(task_dir)
        task_dir, task, attestation = _write_receipt(repo, "lock-risk", risk=True)
        attestation["reviews"] = [review for review in attestation["reviews"] if review["kind"] != "lock-security"]
        (task_dir / "attestation.json").write_text(json.dumps(attestation, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        got, _ = test.invoke(
            vendor, "finish-guard", "git commit -m x", repo, allow_test_adapter=True
        )
        test.result(f"{vendor}: missing risk review", got, "BLOCK")

        task_dir, task, attestation = _write_receipt(repo, "lock-risk")
        attestation["reviews"][0]["reviewer"]["session_id"] = attestation["writer"]["session_id"]
        (task_dir / "attestation.json").write_text(json.dumps(attestation, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        got, _ = test.invoke(
            vendor, "finish-guard", "git commit -m x", repo, allow_test_adapter=True
        )
        test.result(f"{vendor}: reviewer reused writer session", got, "BLOCK")

        shutil.rmtree(repo.parent)

    print("-- real runner / linked-worktree integration --")
    _linked_worktree_runner_case(test)

    print("-- repo-global resource lane --")
    _resource_lane_runner_cases(test)

    if test.failures:
        print("guardrail self-test: FAIL")
        for failure in test.failures:
            print(" - " + failure)
        return 1
    print(f"guardrail self-test: PASS ({test.count} assertions)")
    return 0


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("action", choices=("block-bash", "secret-scan", "finish-guard", "selftest"))
    parser.add_argument("--vendor", choices=("codex", "claude"), default="codex")
    args = parser.parse_args(argv)
    if args.action == "selftest":
        return _run_selftest()
    try:
        command, process_cwd = _read_payload()
        reason = _unsupported_execution_indirection(command)
        if reason is not None:
            pass
        elif args.action == "block-bash":
            reason = _block_bash(command, process_cwd)
        elif args.action == "secret-scan":
            reason = _secret_scan(command, process_cwd)
        else:
            reason = _finish_guard(command, process_cwd)
    except GuardFailure as exc:
        reason = str(exc)
    return _emit(args.vendor, reason, args.action)


if __name__ == "__main__":
    raise SystemExit(main())
