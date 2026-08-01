#!/usr/bin/env python3
"""Canonical Claude/Codex hook guard for Murmur's development harness.

The vendor hook files are deliberately tiny adapters.  All command parsing,
secret scanning, and hash-bound completion checks live here so the two agent
clients cannot silently drift.
"""

from __future__ import annotations

import argparse
import atexit
import copy
import datetime as dt
import fnmatch
import hashlib
import json
import os
import re
import shlex
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
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


class NoTaskForWorktree(GuardFailure):
    """Raised when no harness task claims the current worktree (normal mode)."""


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


def _runtime_module() -> Any:
    """Import the runner helpers so fingerprints/diffs have one exact definition."""

    harness_path = str(HARNESS_ROOT)
    if harness_path not in sys.path:
        sys.path.insert(0, harness_path)
    try:
        import runtime  # type: ignore
    except Exception as exc:  # pragma: no cover - exercised as a fail-closed path
        raise GuardFailure(f"cannot load the canonical harness runtime: {exc}") from exc
    return runtime


def _v2_verifier_module() -> Any:
    harness_path = str(HARNESS_ROOT)
    if harness_path not in sys.path:
        sys.path.insert(0, harness_path)
    try:
        import verifier  # type: ignore
    except Exception as exc:  # pragma: no cover - fail closed
        raise GuardFailure(f"cannot load the canonical v2 verifier: {exc}") from exc
    return verifier


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


_QUOTED_HEREDOC_RE = re.compile(
    r"<<(?P<strip>-)?[ \t]*(?P<quote>['\"])(?P<name>[A-Za-z_][A-Za-z0-9_]*)"
    r"(?P=quote)"
)


def _strip_quoted_heredoc_bodies(command: str) -> str:
    """Hide literal heredoc payloads while preserving executable shell lines.

    A quoted delimiter disables shell expansion in its body, so Rust doc
    backticks and ``$()`` there are data. Unquoted heredocs remain visible and
    fail closed because the shell would execute their substitutions.
    """

    visible: List[str] = []
    pending: List[Tuple[str, bool]] = []
    for line in command.splitlines(keepends=True):
        body = line.rstrip("\r\n")
        newline = line[len(body) :]
        if pending:
            delimiter, strip_tabs = pending[0]
            candidate = body.lstrip("\t") if strip_tabs else body
            if candidate == delimiter:
                pending.pop(0)
            visible.append(newline)
            continue
        visible.append(line)
        pending.extend(
            (match.group("name"), bool(match.group("strip")))
            for match in _QUOTED_HEREDOC_RE.finditer(body)
        )
    return "".join(visible)


def _unsupported_execution_indirection(command: str) -> Optional[str]:
    """Fail closed when the hook cannot see the executable command directly."""

    visible_command = _strip_quoted_heredoc_bodies(command)
    single_quoted = False
    double_quoted = False
    escaped = False
    active_shell_indirection = False
    index = 0
    while index < len(visible_command):
        character = visible_command[index]
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
            or visible_command.startswith("$(", index)
            or visible_command.startswith("<(", index)
            or visible_command.startswith(">(", index)
        ):
            active_shell_indirection = True
            break
        index += 1
    if active_shell_indirection:
        return "shell substitution/process indirection is unsupported by the command guard"
    for simple in _simple_commands(visible_command):
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
        if executable in {"source", "."}:
            # `source`/`.` of a known-safe env file (e.g. ~/.cargo/env) is a
            # standard setup step, not executable indirection. All other targets
            # stay blocked.
            target = effective[1] if len(effective) > 1 else ""
            if target not in resource_policy.SAFE_SOURCE_TARGETS:
                return f"execution indirection via {executable!r} is unsupported by the command guard"
        elif executable in {"eval", "exec", "xargs"}:
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


def _v2_terminal_state_is_proven(task_dir: Path) -> bool:
    """Use the v2 append-only last-state event; state.json is a projection."""

    try:
        verifier = _v2_verifier_module()
        last_wrapper: Optional[Dict[str, Any]] = None
        with (task_dir / "events.jsonl").open(
            "r", encoding="utf-8", errors="strict"
        ) as handle:
            for line in handle:
                event = json.loads(line)
                if not isinstance(event, dict):
                    raise ValueError("v2 event is not an object")
                if event.get("event") == "state":
                    if not isinstance(event.get("state"), dict):
                        raise ValueError("v2 state event payload is malformed")
                    last_wrapper = event

        state = verifier.last_state_event(task_dir)
        if last_wrapper is None or state != last_wrapper["state"]:
            return False
        updated_at = state.get("updated_at")
        if (
            state.get("schema_version") != 2
            or state.get("task_id") != task_dir.name
            or state.get("status") not in verifier.V2_STATES
            or not isinstance(updated_at, str)
            or last_wrapper.get("at") != updated_at
        ):
            return False
        _parse_time(updated_at, f"{task_dir.name} state event updated_at")
        return state["status"] in verifier.V2_TERMINAL_STATES
    except Exception:
        # Missing/malformed authoritative state means unknown-live whenever the
        # task still claims an existing worktree.
        return False


def _live_harness_task_ids(common: Path) -> List[str]:
    """Return nonterminal or state-unknown tasks with an existing worktree."""

    result: List[str] = []
    root = common / "agent-harness" / "v2" / "tasks"
    if root.is_dir():
        for task_dir in sorted(path for path in root.iterdir() if path.is_dir()):
            try:
                contract = json.loads((task_dir / "task.json").read_text(encoding="utf-8"))
                worktree_raw = contract["worktree_path"]
                if not isinstance(worktree_raw, str) or not worktree_raw:
                    continue
                worktree = Path(worktree_raw).resolve()
            except (
                OSError,
                KeyError,
                TypeError,
                ValueError,
                json.JSONDecodeError,
            ):
                continue
            if not worktree.exists():
                continue
            if not _v2_terminal_state_is_proven(task_dir):
                result.append(task_dir.name)
    return result


def _primary_branch_surgery_reason(invocation: GitInvocation) -> Optional[str]:
    if invocation.subcommand not in {
        "checkout",
        "switch",
        "merge",
        "rebase",
        "reset",
        "cherry-pick",
        "pull",
    }:
        return None
    top_raw = _git_text(
        invocation.cwd, "rev-parse", "--show-toplevel", check=False
    )
    if not top_raw:
        return None
    top = Path(top_raw).resolve()
    if top != _primary_worktree(top):
        return None
    common_raw = Path(_git_text(top, "rev-parse", "--git-common-dir"))
    common = (
        common_raw.resolve()
        if common_raw.is_absolute()
        else (top / common_raw).resolve()
    )
    live = _live_harness_task_ids(common)
    if not live:
        return None
    preview = ", ".join(live[:3]) + ("…" if len(live) > 3 else "")
    return (
        f"primary-checkout branch surgery is blocked while Harness task(s) "
        f"are live or state-unknown ({preview}); continue from the isolated task/driver worktree "
        "and use server-side PR merge"
    )


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
        surgery_reason = _primary_branch_surgery_reason(invocation)
        if surgery_reason is not None:
            return surgery_reason
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
    dev = resource_policy.command_is_dev_in(command, process_cwd)
    heavy = resource_policy.command_is_heavy_in(command, process_cwd)
    if (dev or heavy) and _worktree_has_active_task(process_cwd):
        if dev:
            return (
                "long-lived dev commands must run through scripts/agent-dev-run; "
                "the dev supervisor stays outside the global lane while its cargo/rustc "
                "descendants acquire it per process"
            )
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


def _validate_task_manifest(document: Any, manifest: Path) -> Dict[str, Any]:
    if not isinstance(document, dict):
        raise GuardFailure(f"task manifest is not an object: {manifest}")
    try:
        v2 = _v2_verifier_module()
        v2.validate_hashed_document(
            document,
            "v2-task",
            "contract_sha256",
            "task contract",
        )
    except GuardFailure:
        raise
    except Exception as exc:
        raise GuardFailure(f"invalid task manifest {manifest}: {exc}") from exc
    return document


def _resolve_task(repo: Path, common: Path) -> Tuple[Dict[str, Any], Path]:
    tasks_root = common / "agent-harness" / "v2" / "tasks"
    explicit_id = os.environ.get("MURMUR_AGENT_TASK_ID", "").strip()
    if explicit_id:
        if not TASK_ID_RE.fullmatch(explicit_id):
            raise GuardFailure("MURMUR_AGENT_TASK_ID is invalid")
        manifest = tasks_root / explicit_id / "task.json"
        if not manifest.is_file():
            raise GuardFailure("explicit task reference does not exist")
        document = _validate_task_manifest(_load_json(manifest), manifest)
        if _manifest_worktree(document) != repo.resolve():
            raise GuardFailure("explicit task manifest belongs to a different worktree")
        return document, manifest.parent

    matches: List[Tuple[Dict[str, Any], Path]] = []
    if tasks_root.is_dir():
        for manifest in sorted(tasks_root.glob("*/task.json")):
            try:
                raw = _load_json(manifest)
            except GuardFailure:
                # A foreign unreadable record is lifecycle debt for `doctor`,
                # not proof that it claims this worktree. Explicit task refs
                # still validate and fail closed above.
                continue
            if _manifest_worktree(raw) != repo.resolve():
                continue
            document = _validate_task_manifest(raw, manifest)
            matches.append((document, manifest.parent))
    if len(matches) > 1:
        raise GuardFailure("multiple task manifests claim this worktree")
    if len(matches) == 1:
        return matches[0]
    raise NoTaskForWorktree(
        "no task manifest matches this worktree; use scripts/agent-harness open"
    )


def _worktree_has_active_task(process_cwd: Path) -> bool:
    """True when a harness task claims this worktree (harness mode)."""
    try:
        top = _git_text(process_cwd, "rev-parse", "--show-toplevel", check=False)
        if not top:
            return False
        repo = Path(top).resolve()
        _, common, _, _ = _repo_context(repo)
        _resolve_task(repo, common)
        return True
    except NoTaskForWorktree:
        return False
    except GuardFailure:
        # Tasks exist but are ambiguous/malformed → fail safe: treat as harness
        # mode so the lane stays enforced.
        return True
    except Exception:
        # Not a resolvable git worktree → normal mode.
        return False


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
            task, _task_dir = _resolve_task(repo, common)
            raise GuardFailure(
                "the Harness owns the durable commit intent and receipt; "
                f"use scripts/agent-harness commit {task['task_id']} -m <message>"
            )
        except NoTaskForWorktree:
            # Normal mode: no active harness task, so the commit itself is allowed. Before letting
            # it through, run the ONE check that is cheaper than reading this comment.
            return _control_plane_audit_refusal(commits[0])
        except GuardFailure as exc:
            reason = str(exc)
    if mode == "advisory":
        print(f"finish-guard advisory: {reason}", file=sys.stderr)
        return None
    return "Definition-of-Done receipt rejected: " + reason


def _control_plane_audit_refusal(invocation: "GitInvocation") -> Optional[str]:
    """Run agent-config-audit before a commit lands, and refuse if it fails.

    CLAUDE.md records why: on PR #535 `cargo test --lib`, `ng lint` and `ng build` were all green
    while the diff carried `.claude/`<->`.codex/` binding-rule drift AND a Bash-hook change that
    silently disabled secret scanning and this very guard. The audit caught both in 0.09 s — but it
    only ran remotely, so the feedback arrived one CI round-trip later, after the commit existed.

    The check costs less than a tenth of a second; the round trip costs thirteen minutes. Deciding
    whether a diff "touches the control plane" costs more thought than just running it, so it runs
    on every commit.

    Fails OPEN on anything that is not a clean non-zero verdict — a missing script, an unreadable
    repo, a timeout. This hook must never be the reason a commit cannot happen when the auditor
    itself is broken; it exists to surface a real FAIL earlier, not to add a new way to be stuck.
    `MURMUR_FINISH_GUARD=off` disables it with everything else in this guard.
    """

    try:
        repo = _repo_for_invocation(invocation)
    except Exception:
        return None
    script = repo / "scripts" / "agent-config-audit"
    if not script.is_file():
        return None
    try:
        completed = subprocess.run(
            [str(script), "--ci"],
            cwd=str(repo),
            capture_output=True,
            text=True,
            timeout=60,
            check=False,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if completed.returncode == 0:
        return None
    detail = ""
    for line in (completed.stdout or "").splitlines():
        if line.startswith("[FAIL]"):
            detail = line.strip()
            break
    return (
        "control-plane audit failed before this commit"
        + (f": {detail}" if detail else "")
        + " — run scripts/agent-config-audit for the full report"
    )


def _guard_refusal_task_dirs(
    process_cwd: Path, *, include_live_primary_tasks: bool
) -> List[Path]:
    """Find task ledgers that should receive a best-effort refusal event."""

    top_raw = _git_text(
        process_cwd, "rev-parse", "--show-toplevel", check=False
    )
    if not top_raw:
        return []
    top = Path(top_raw).resolve()
    common_raw = Path(_git_text(top, "rev-parse", "--git-common-dir"))
    common = (
        common_raw.resolve()
        if common_raw.is_absolute()
        else (top / common_raw).resolve()
    )
    primary = _primary_worktree(top)
    matches: List[Path] = []
    root = common / "agent-harness" / "v2" / "tasks"
    if root.is_dir():
        for task_dir in sorted(path for path in root.iterdir() if path.is_dir()):
            try:
                task = _load_json(task_dir / "task.json")
                claimed = _manifest_worktree(task)
            except Exception:
                continue
            if claimed == top:
                matches.append(task_dir)
                continue
            if not include_live_primary_tasks or top != primary:
                continue
            if claimed is None or not claimed.exists():
                continue
            if not _v2_terminal_state_is_proven(task_dir):
                matches.append(task_dir)
    return sorted(set(matches))


def _record_guard_refusal(
    process_cwd: Path, command: str, guard: str, reason: str
) -> None:
    """Append diagnostics without ever changing the guard's allow/block result."""

    try:
        compact = " ".join(command.split())
        preview = (
            "<secret-bearing command redacted>"
            if guard == "secret-scan"
            else compact[:80]
        )
        payload = (
            json.dumps(
                {
                    "at": dt.datetime.now(dt.timezone.utc)
                    .isoformat()
                    .replace("+00:00", "Z"),
                    "event": "guard-refusal",
                    "guard": guard,
                    "reason": reason,
                    "command_preview": preview,
                    "command_sha256": hashlib.sha256(
                        command.encode("utf-8", "surrogateescape")
                    ).hexdigest(),
                    "cwd": str(process_cwd.resolve()),
                },
                sort_keys=True,
                separators=(",", ":"),
            ).encode("utf-8")
            + b"\n"
        )
        for task_dir in _guard_refusal_task_dirs(
            process_cwd,
            include_live_primary_tasks=reason.startswith(
                "primary-checkout branch surgery"
            ),
        ):
            path = task_dir / "events.jsonl"
            flags = os.O_WRONLY | os.O_APPEND
            if hasattr(os, "O_CLOEXEC"):
                flags |= os.O_CLOEXEC
            if hasattr(os, "O_NOFOLLOW"):
                flags |= os.O_NOFOLLOW
            descriptor = os.open(path, flags)
            try:
                metadata = os.fstat(descriptor)
                if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
                    continue
                if os.write(descriptor, payload) != len(payload):
                    continue
                os.fsync(descriptor)
            finally:
                os.close(descriptor)
    except Exception:
        # Telemetry is never authority and may not weaken or strengthen a
        # refusal. Doctor can report an unwritable/corrupt task ledger.
        return


def _emit(
    vendor: str,
    reason: Optional[str],
    guard: str,
    *,
    command: str = "",
    process_cwd: Optional[Path] = None,
) -> int:
    if reason is None:
        return 0
    _record_guard_refusal(process_cwd or Path.cwd(), command, guard, reason)
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
    toolchain = SOURCE_ROOT / "rust-toolchain.toml"
    if not toolchain.is_file():
        raise GuardFailure("hook selftest requires the repository Rust toolchain pin")
    (path / "rust-toolchain.toml").write_bytes(toolchain.read_bytes())
    cargo_src = path / "src-tauri" / "src"
    cargo_src.mkdir(parents=True)
    (path / "src-tauri" / "Cargo.toml").write_text(
        '[package]\nname="hook-selftest"\nversion="0.0.0"\nedition="2021"\n\n'
        "[workspace]\n",
        encoding="utf-8",
    )
    (cargo_src / "lib.rs").write_text(
        "#[cfg(test)] mod tests { #[test] fn smoke() { assert_eq!(2 + 2, 4); } }\n",
        encoding="utf-8",
    )
    _run(
        [
            "git",
            "add",
            "base.txt",
            "rust-toolchain.toml",
            "src-tauri/Cargo.toml",
            "src-tauri/src/lib.rs",
        ],
        path,
    )
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


def _finish_repo() -> Path:
    # Cargo discovers .cargo/config.toml by walking every parent of its cwd.
    # The outer harness TMPDIR lives below the primary checkout's common .git,
    # so a nested fixture created there can accidentally discover mutable
    # primary-worktree configuration. Keep the Rust-backed finish fixture
    # below this sealed worktree instead; the source diff fingerprint catches
    # contamination and the caller removes the directory after every case.
    fixture_parent = SOURCE_ROOT / "target" / "harness-selftest"
    fixture_parent.mkdir(parents=True, exist_ok=True)
    temp = Path(tempfile.mkdtemp(prefix="murmur-hook-finish-", dir=fixture_parent))
    atexit.register(shutil.rmtree, temp, ignore_errors=True)
    repo = temp / "repo"
    _init_repo(repo)
    (repo / "owned.txt").write_text("changed\n", encoding="utf-8")
    _run(["git", "add", "owned.txt"], repo)
    return repo


def _write_task_claim(repo: Path, task_id: str) -> Path:
    runtime = _runtime_module()
    verifier = _v2_verifier_module()
    top, common, head, branch = _repo_context(repo)
    task_dir = common / "agent-harness" / "v2" / "tasks" / task_id
    task_dir.mkdir(parents=True, exist_ok=True)
    contract: Dict[str, Any] = {
        "schema_version": 2,
        "task_id": task_id,
        "description": "hook guard selftest",
        "kind": "harness",
        "base_sha": head,
        "contract_sha256": "",
        "repo_realpath": str(top),
        "git_common_dir": str(common),
        "worktree_path": str(top),
        "branch": branch,
        "owned_paths": ["owned.txt"],
        "claims": [],
        "reviewer": "fake",
        "expected_change": True,
        "created_at": runtime.utc_now(),
    }
    contract["contract_sha256"] = verifier.document_hash(
        contract, "contract_sha256"
    )
    runtime.atomic_write_json(task_dir / "task.json", contract)
    return task_dir


def _resource_lane_runner_cases(test: _Selftest) -> None:
    """Exercise the bounded supervisor without launching any product build."""

    runner = SOURCE_ROOT / "scripts" / "agent-resource-run"
    dev_runner = SOURCE_ROOT / "scripts" / "agent-dev-run"
    test.result(
        "resource lane runner is executable",
        "PASS" if runner.is_file() and os.access(runner, os.X_OK) else "FAIL",
        "PASS",
    )
    test.result(
        "dev runner is executable",
        "PASS" if dev_runner.is_file() and os.access(dev_runner, os.X_OK) else "FAIL",
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

        lane_env = {
            **os.environ,
            "MURMUR_HARNESS_TASK": "lane-owner",
        }
        owner = subprocess.Popen(
            [
                str(runner),
                "--deadline-seconds",
                "3",
                "--",
                "sh",
                "-c",
                "sleep 2",
            ],
            cwd=str(repo),
            env=lane_env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            common = Path(
                _run(
                    [
                        "git",
                        "rev-parse",
                        "--path-format=absolute",
                        "--git-common-dir",
                    ],
                    repo,
                    text=True,
                ).stdout.strip()
            )
            lane_path = (
                _runtime_module().shared_resource_root_for_common(common)
                / "cargo.lock"
            )
            owner_seen = False
            for _attempt in range(50):
                try:
                    payload = json.loads(lane_path.read_text(encoding="utf-8"))
                    owner_seen = payload.get("pid") == owner.pid
                except (FileNotFoundError, OSError, json.JSONDecodeError):
                    owner_seen = False
                if owner_seen:
                    break
                time.sleep(0.02)
            test.result(
                "resource lane publishes owner before waiters",
                "PASS" if owner_seen else "FAIL",
                "PASS",
            )

            waiter = subprocess.run(
                [
                    str(runner),
                    "--deadline-seconds",
                    "0.25",
                    "--heartbeat-seconds",
                    "0.05",
                    "--",
                    "sh",
                    "-c",
                    "exit 0",
                ],
                cwd=str(repo),
                env={**os.environ, "MURMUR_HARNESS_TASK": "lane-waiter"},
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                check=False,
                timeout=3,
            )
            waiter_stderr = waiter.stderr
            test.result(
                "resource runner wait reports owner immediately and heartbeats",
                "PASS"
                if (
                    waiter.returncode == 124
                    and f"owner_pid={owner.pid}" in waiter_stderr
                    and "task=lane-owner" in waiter_stderr
                    and "command=sh -c sleep 2" in waiter_stderr
                    and waiter_stderr.count("heartbeat=") >= 2
                )
                else f"FAIL({waiter.returncode}: {waiter_stderr[-300:]})",
                "PASS",
            )

            direct_task = common / "agent-harness" / "v2" / "tasks" / "lane-direct"
            direct_task.mkdir(parents=True)
            direct_code = (
                "import pathlib,sys;"
                f"sys.path.insert(0,{str((SOURCE_ROOT / '.agents' / 'harness')).__repr__()});"
                "import runtime;"
                "runtime.acquire_cargo_lane("
                "pathlib.Path(sys.argv[1]),0.25,"
                "command='direct-selftest',heartbeat_seconds=0.05)"
            )
            direct = subprocess.run(
                [sys.executable, "-c", direct_code, str(direct_task)],
                cwd=str(repo),
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                check=False,
                timeout=3,
            )
            test.result(
                "run_check Cargo lane path reports owner and heartbeats",
                "PASS"
                if (
                    direct.returncode != 0
                    and f"owner_pid={owner.pid}" in direct.stderr
                    and "task=lane-owner" in direct.stderr
                    and "command=sh -c sleep 2" in direct.stderr
                    and direct.stderr.count("heartbeat=") >= 2
                )
                else f"FAIL({direct.returncode}: {direct.stderr[-300:]})",
                "PASS",
            )
        finally:
            if owner.poll() is None:
                owner.terminate()
            try:
                owner.communicate(timeout=2)
            except subprocess.TimeoutExpired:
                owner.kill()
                owner.communicate(timeout=2)

        fake_bin = Path(temp) / "fake-bin"
        fake_bin.mkdir()
        tool_log = Path(temp) / "tool.log"
        ready_file = Path(temp) / "dev.ready"
        child_pid_file = Path(temp) / "dev-child.pid"
        fake_cargo = fake_bin / "cargo"
        fake_rustc = fake_bin / "rustc"
        fake_npm = fake_bin / "npm"
        fake_cargo.write_text(
            "#!/bin/sh\n"
            "set -eu\n"
            'if test "${MURMUR_AGENT_DEV_TEST_MODE:-}" = compile_hold; then\n'
            '  printf "%s\\n" "$$" > "$MURMUR_AGENT_DEV_TEST_CHILD_PID"\n'
            '  : > "$MURMUR_AGENT_DEV_TEST_READY"\n'
            "  trap 'exit 0' HUP INT TERM\n"
            "  while :; do sleep 0.05; done\n"
            "fi\n"
            'printf "cargo lane=%s rustc=%s\\n" '
            '"${MURMUR_AGENT_RESOURCE_LANE_ACTIVE:-}" "${RUSTC:-}" '
            '>> "$MURMUR_AGENT_DEV_TEST_LOG"\n'
            '"$RUSTC" --dev-proxy-selftest\n',
            encoding="utf-8",
        )
        fake_rustc.write_text(
            "#!/bin/sh\n"
            "set -eu\n"
            'printf "rustc lane=%s\\n" "${MURMUR_AGENT_RESOURCE_LANE_ACTIVE:-}" '
            '>> "$MURMUR_AGENT_DEV_TEST_LOG"\n',
            encoding="utf-8",
        )
        fake_npm.write_text(
            "#!/bin/sh\n"
            "set -eu\n"
            'case "${MURMUR_AGENT_DEV_TEST_MODE:-proxy}" in\n'
            '  proxy) "$CARGO" --dev-proxy-selftest; "$RUSTC" --dev-proxy-selftest ;;\n'
            '  forge_lane) MURMUR_AGENT_RESOURCE_LANE_ACTIVE=1 "$CARGO" --dev-proxy-selftest ;;\n'
            "  hold)\n"
            '    printf "%s\\n" "$$" > "$MURMUR_AGENT_DEV_TEST_CHILD_PID"\n'
            '    : > "$MURMUR_AGENT_DEV_TEST_READY"\n'
            "    trap 'exit 0' HUP INT TERM\n"
            "    while :; do sleep 0.05; done\n"
            "    ;;\n"
            '  compile_hold) "$CARGO" --dev-proxy-selftest ;;\n'
            "  *) exit 65 ;;\n"
            "esac\n",
            encoding="utf-8",
        )
        for executable in (fake_cargo, fake_rustc, fake_npm):
            executable.chmod(0o755)

        dev_env = os.environ.copy()
        for reserved in (
            "MURMUR_AGENT_RESOURCE_LANE_ACTIVE",
            "MURMUR_AGENT_DEV_PROXY_DIR",
            "MURMUR_AGENT_DEV_PROXY_TOKEN",
            "MURMUR_AGENT_DEV_REAL_CARGO",
            "MURMUR_AGENT_DEV_REAL_RUSTC",
        ):
            # The complete CI command may itself be resource-lane wrapped.
            # These child cases model a fresh operator dev invocation; the
            # production runner still rejects inherited/forged supervisor state.
            dev_env.pop(reserved, None)
        dev_env["PATH"] = str(fake_bin) + os.pathsep + dev_env.get("PATH", "")
        dev_env["MURMUR_AGENT_DEV_TEST_LOG"] = str(tool_log)
        completed = subprocess.run(
            [str(dev_runner), "--", "npm", "run", "dev"],
            cwd=str(repo),
            env=dev_env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            check=False,
            timeout=5,
        )
        tool_output = tool_log.read_text(encoding="utf-8") if tool_log.is_file() else ""
        proxy_ok = (
            completed.returncode == 0
            and "cargo lane=1" in tool_output
            and f"rustc={fake_rustc}" in tool_output
            and tool_output.count("rustc lane=1") == 2
        )
        test.result(
            "dev runner proxies cargo and rustc without recursive flock",
            "PASS" if proxy_ok else f"FAIL({completed.returncode}: {tool_output.strip()})",
            "PASS",
        )

        lane_owner_ready = Path(temp) / "lane-owner.ready"
        owner_env = os.environ.copy()
        owner_env["MURMUR_AGENT_DEV_TEST_READY"] = str(lane_owner_ready)
        lane_owner = subprocess.Popen(
            [
                str(runner),
                "--deadline-seconds",
                "3",
                "--",
                "sh",
                "-c",
                ': > "$MURMUR_AGENT_DEV_TEST_READY"; sleep 2',
            ],
            cwd=str(repo),
            env=owner_env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            ready_until = time.monotonic() + 2
            while time.monotonic() < ready_until and not lane_owner_ready.exists():
                if lane_owner.poll() is not None:
                    break
                time.sleep(0.02)
            before_forge = tool_log.read_text(encoding="utf-8") if tool_log.is_file() else ""
            forge_env = dev_env.copy()
            forge_env["MURMUR_AGENT_DEV_TEST_MODE"] = "forge_lane"
            forge_env["MURMUR_AGENT_DEADLINE_SECONDS"] = "0.20"
            forged = subprocess.run(
                [str(dev_runner), "--", "npm", "run", "dev"],
                cwd=str(repo),
                env=forge_env,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
                check=False,
                timeout=3,
            )
            after_forge = tool_log.read_text(encoding="utf-8") if tool_log.is_file() else ""
            test.result(
                "dev child cannot forge lane marker to bypass held flock",
                "PASS"
                if lane_owner_ready.exists()
                and forged.returncode == 124
                and after_forge == before_forge
                else f"FAIL(owner={lane_owner.poll()}, forged={forged.returncode})",
                "PASS",
            )
        finally:
            if lane_owner.poll() is None:
                lane_owner.terminate()
            try:
                lane_owner.wait(timeout=3)
            except subprocess.TimeoutExpired:
                lane_owner.kill()
                lane_owner.wait(timeout=3)

        hold_env = dev_env.copy()
        hold_env["MURMUR_AGENT_DEV_TEST_MODE"] = "hold"
        hold_env["MURMUR_AGENT_DEV_TEST_READY"] = str(ready_file)
        hold_env["MURMUR_AGENT_DEV_TEST_CHILD_PID"] = str(child_pid_file)
        dev_process = subprocess.Popen(
            [str(dev_runner), "--term-grace-seconds", "0.20", "--", "npm", "run", "dev"],
            cwd=str(repo),
            env=hold_env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            ready_until = time.monotonic() + 3
            while time.monotonic() < ready_until and not ready_file.exists():
                if dev_process.poll() is not None:
                    break
                time.sleep(0.02)
            lane_probe = subprocess.run(
                [str(runner), "--deadline-seconds", "0.50", "--", "sh", "-c", "exit 0"],
                cwd=str(repo),
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
                timeout=3,
            )
            test.result(
                "long-lived dev supervisor does not hold resource lane",
                "PASS"
                if ready_file.exists() and lane_probe.returncode == 0
                else f"FAIL(dev={dev_process.poll()}, lane={lane_probe.returncode})",
                "PASS",
            )
            dev_process.send_signal(signal.SIGTERM)
            dev_return = dev_process.wait(timeout=4)
            child_alive = False
            if child_pid_file.is_file():
                child_pid = int(child_pid_file.read_text(encoding="utf-8").strip())
                try:
                    os.kill(child_pid, 0)
                    child_alive = True
                except ProcessLookupError:
                    pass
            test.result(
                "dev runner signal tears down its owned process group",
                "PASS" if dev_return == 143 and not child_alive else f"FAIL({dev_return})",
                "PASS",
            )
        finally:
            if dev_process.poll() is None:
                dev_process.kill()
                dev_process.wait(timeout=3)

        ready_file.unlink(missing_ok=True)
        child_pid_file.unlink(missing_ok=True)
        compile_env = dev_env.copy()
        compile_env["MURMUR_AGENT_DEV_TEST_MODE"] = "compile_hold"
        compile_env["MURMUR_AGENT_DEV_TEST_READY"] = str(ready_file)
        compile_env["MURMUR_AGENT_DEV_TEST_CHILD_PID"] = str(child_pid_file)
        dev_process = subprocess.Popen(
            [str(dev_runner), "--term-grace-seconds", "0.20", "--", "npm", "run", "dev"],
            cwd=str(repo),
            env=compile_env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            ready_until = time.monotonic() + 3
            while time.monotonic() < ready_until and not ready_file.exists():
                if dev_process.poll() is not None:
                    break
                time.sleep(0.02)
            dev_process.send_signal(signal.SIGTERM)
            dev_return = dev_process.wait(timeout=4)
            lane_probe = subprocess.run(
                [str(runner), "--deadline-seconds", "0.50", "--", "sh", "-c", "exit 0"],
                cwd=str(repo),
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
                timeout=3,
            )
            child_alive = False
            if child_pid_file.is_file():
                child_pid = int(child_pid_file.read_text(encoding="utf-8").strip())
                try:
                    os.kill(child_pid, 0)
                    child_alive = True
                except ProcessLookupError:
                    pass
            test.result(
                "dev signal reaps an in-flight cargo lane and releases flock",
                "PASS"
                if ready_file.exists()
                and dev_return == 143
                and lane_probe.returncode == 0
                and not child_alive
                else f"FAIL(dev={dev_return}, lane={lane_probe.returncode})",
                "PASS",
            )
        finally:
            if dev_process.poll() is None:
                dev_process.kill()
                dev_process.wait(timeout=3)


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
                (
                    "catch-up merge then feature push",
                    "git checkout feature/x && git merge origin/murmur && git push origin feature/x",
                    "ALLOW",
                ),
                ("absolute security", "/usr/bin/security find-identity -v", "BLOCK"),
                ("sudo security", "sudo /usr/bin/security unlock-keychain login.keychain", "BLOCK"),
                ("security management text", "pkill security", "ALLOW"),
                ("notary credential store", "xcrun notarytool store-credentials murmur", "BLOCK"),
                ("notary submit", "xcrun notarytool submit app.dmg --wait", "ALLOW"),
                ("cargo toolchain clippy", "cargo +stable clippy --all-targets", "BLOCK"),
                ("cargo test", "cargo test --lib", "ALLOW"),
                ("codesign deep", "/usr/bin/codesign --deep --sign X app", "BLOCK"),
                ("codesign helper", "/usr/bin/codesign --options runtime --sign X helper", "ALLOW"),
                ("root delete", "/bin/rm -rf -- /", "BLOCK"),
                ("scoped delete", "/bin/rm -rf target/tmp", "ALLOW"),
                ("PR creation", "gh pr create --base murmur --title x", "ALLOW"),
                ("quoted push search", "rg 'git push origin murmur' .", "ALLOW"),
                ("quoted security search", "rg 'security find-identity' .", "ALLOW"),
                ("source cargo env", "source ~/.cargo/env", "ALLOW"),
                ("dot source cargo env", ". $HOME/.cargo/env", "ALLOW"),
                ("source arbitrary script", "source ./guard-bypass.sh", "BLOCK"),
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
                test.expect(
                    f"{action} permits literal quoted heredoc body",
                    vendor,
                    action,
                    "python3 - <<'PY'\n"
                    "print(\"Rust `doc` and $(literal) with writer's apostrophe\")\n"
                    "PY\n",
                    repo,
                    "ALLOW",
                )
                test.expect(
                    f"{action} rejects expanding heredoc substitution",
                    vendor,
                    action,
                    "python3 - <<PY\n$(printf git) push origin main\nPY\n",
                    repo,
                    "BLOCK",
                )

            live_worktree = Path(temp) / "live-v2-task"
            live_worktree.mkdir()
            live_task = (
                repo
                / ".git"
                / "agent-harness"
                / "v2"
                / "tasks"
                / "primary-isolation"
            )
            live_task.mkdir(parents=True)
            (live_task / "task.json").write_text(
                json.dumps({"worktree_path": str(live_worktree.resolve())})
                + "\n",
                encoding="utf-8",
            )
            live_updated_at = "2026-07-28T10:00:00Z"
            live_state = {
                "schema_version": 2,
                "task_id": live_task.name,
                "status": "OPEN",
                "updated_at": live_updated_at,
            }
            (live_task / "events.jsonl").write_text(
                json.dumps(
                    {
                        "at": live_updated_at,
                        "event": "state",
                        "state": live_state,
                    }
                )
                + "\n",
                encoding="utf-8",
            )
            test.expect(
                "SIGKILL ledger-only v2 task blocks primary branch surgery",
                vendor,
                "block-bash",
                "git checkout -b unsafe-primary-move",
                repo,
                "BLOCK",
            )
            refusal_events = [
                json.loads(line)
                for line in (live_task / "events.jsonl")
                .read_text(encoding="utf-8")
                .splitlines()
                if line.strip()
            ]
            refusal = next(
                (
                    event
                    for event in reversed(refusal_events)
                    if event.get("event") == "guard-refusal"
                ),
                {},
            )
            test.result(
                f"{vendor}: guard refusal is recorded in active task ledger",
                "RECORDED"
                if (
                    refusal.get("guard") == "block-bash"
                    and refusal.get("command_preview")
                    == "git checkout -b unsafe-primary-move"
                    and SHA256_RE.fullmatch(
                        str(refusal.get("command_sha256", ""))
                    )
                )
                else "MISSING",
                "RECORDED",
            )
            test.expect(
                "git pull is primary branch surgery",
                vendor,
                "block-bash",
                "git pull --ff-only",
                repo,
                "BLOCK",
            )

            terminal_updated_at = "2026-07-28T10:01:00Z"
            terminal_state = {
                **live_state,
                "status": "CLOSED",
                "updated_at": terminal_updated_at,
            }
            with (live_task / "events.jsonl").open("a", encoding="utf-8") as handle:
                handle.write(
                    json.dumps(
                        {
                            "at": terminal_updated_at,
                            "event": "state",
                            "state": terminal_state,
                        }
                    )
                    + "\n"
                )
            (live_task / "state.json").write_text(
                "{malformed projection\n", encoding="utf-8"
            )
            test.expect(
                "authoritative v2 terminal event permits existing task worktree",
                vendor,
                "block-bash",
                "git checkout --detach HEAD",
                repo,
                "ALLOW",
            )
            (live_task / "events.jsonl").write_text(
                "{malformed authoritative state\n", encoding="utf-8"
            )
            test.expect(
                "malformed v2 authoritative state is unknown-live",
                vendor,
                "block-bash",
                "git checkout --detach HEAD",
                repo,
                "BLOCK",
            )
            (live_task / "events.jsonl").write_text(
                json.dumps(
                    {
                        "at": terminal_updated_at,
                        "event": "state",
                        "state": terminal_state,
                    }
                )
                + "\n",
                encoding="utf-8",
            )

            driver = Path(temp) / "driver"
            _run(
                ["git", "worktree", "add", "--detach", str(driver), "HEAD"],
                repo,
            )
            test.expect(
                "dedicated driver worktree may move independently",
                vendor,
                "block-bash",
                "git checkout --detach HEAD",
                driver,
                "ALLOW",
            )

        resource_fixture_root = Path(
            tempfile.mkdtemp(prefix="murmur-hook-resource-command-")
        )
        resource_case_cwd = resource_fixture_root / "repo"
        _init_repo(resource_case_cwd)
        resource_cases = [
            ("direct cargo metadata", "cargo metadata --no-deps", "ALLOW"),
            ("direct Rust test", "cd src-tauri && cargo test --lib", "ALLOW"),
            ("direct Angular build", "npx ng build", "ALLOW"),
            ("direct npm dev", "npm run dev", "ALLOW"),
            ("direct full CI", "bash scripts/ci.sh", "ALLOW"),
            ("read-only cargo search", "rg 'cargo test --lib' .", "ALLOW"),
            (
                "gh PR body mentions cargo",
                "gh pr create --base murmur --title x --body 'Fixes the cargo build path'",
                "ALLOW",
            ),
            (
                "gh PR body with backticks",
                "gh pr create --title x --body 'run `cargo test --lib` first'",
                "ALLOW",
            ),
            ("gh then cargo still heavy", "gh pr view 1 && cargo build", "ALLOW"),
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
                "lane-wrapped long-lived dev",
                "scripts/agent-resource-run -- npm run dev",
                "ALLOW",
            ),
            (
                "lane-wrapped chdir long-lived dev",
                "scripts/agent-resource-run --chdir /tmp/deck npm run dev -- --port 4173",
                "ALLOW",
            ),
            (
                "dedicated npm dev",
                "scripts/agent-dev-run -- npm run dev",
                "ALLOW",
            ),
            (
                "dedicated npm dev port",
                "scripts/agent-dev-run -- npm run dev -- --port 4173",
                "ALLOW",
            ),
            (
                "dedicated npm dev host",
                "scripts/agent-dev-run -- npm run dev -- --host 127.0.0.1",
                "ALLOW",
            ),
            (
                "dedicated npm start",
                "scripts/agent-dev-run -- npm run start",
                "ALLOW",
            ),
            (
                "dedicated dev config override",
                "scripts/agent-dev-run -- npm run dev -- --config injected.json",
                "ALLOW",
            ),
            (
                "dedicated runner arbitrary cargo",
                "scripts/agent-dev-run -- cargo test --lib",
                "ALLOW",
            ),
            (
                "dedicated runner npm build",
                "scripts/agent-dev-run -- npm run build",
                "ALLOW",
            ),
            (
                "lookalike dev runner",
                "/tmp/scripts/agent-dev-run -- npm run dev",
                "ALLOW",
            ),
            (
                "lookalike lane runner",
                "/tmp/scripts/agent-resource-run -- cargo test --lib",
                "ALLOW",
            ),
            (
                "test-only guardian env",
                "MURMUR_AGENT_SELFTEST_GUARDIAN_RELEASE=/tmp/x scripts/agent-resource-run -- true",
                "ALLOW",
            ),
            (
                "forged lane-active env",
                "MURMUR_AGENT_RESOURCE_LANE_ACTIVE=1 scripts/agent-dev-run -- npm run dev",
                "ALLOW",
            ),
        ]
        for label, command, want in resource_cases:
            test.expect(
                label,
                vendor,
                "block-bash",
                command,
                resource_case_cwd,
                want,
            )
        shutil.rmtree(resource_fixture_root)

        for label, cmd, want in [
            ("gh live $() substitution is heavy", 'gh pr create --title x --body "$(cargo build --release)"', "HEAVY"),
            ("gh single-quoted backticks are light", "gh pr create --title x --body 'run `cargo test` first'", "LIGHT"),
            ("gh plain cargo word is light", 'gh pr create --title x --body "fixes the cargo build"', "LIGHT"),
        ]:
            got = "HEAVY" if resource_policy.command_is_heavy(cmd) else "LIGHT"
            test.result(label, got, want)

        # Finding 2 fix: mode-independent anti-bypass classifier coverage.
        # These are the misuse/lookalike patterns whose block-bash result
        # correctly flipped BLOCK -> ALLOW in the no-task fixture above (the
        # resource-lane gate now only fires when a harness task claims the
        # worktree). Their classification in resource_policy.py did NOT
        # change and is still correct, but block-bash no longer exercises it
        # in no-task mode, so it lost its only regression guard. Pin the
        # classifier directly, independent of task/mode, using the exact
        # execution directory (SOURCE_ROOT) the hook payload would carry --
        # command_is_heavy_in/command_is_dev_in resolve lookalike-runner
        # paths relative to that cwd, so the real repo root is required for
        # the "outside the repo" impostor checks to behave as intended.
        # Verified non-vacuous: a clearly-safe command ("git status --short")
        # classifies heavy=False, dev=False against the same cwd.
        anti_bypass_heavy = [
            ("plain unwrapped cargo test", "cargo test --lib"),
            (
                "dev-runner allowlist violation: injected config flag",
                "scripts/agent-dev-run -- npm run dev -- --config injected.json",
            ),
            (
                "dev-runner allowlist violation: arbitrary cargo",
                "scripts/agent-dev-run -- cargo test --lib",
            ),
            (
                "dev-runner allowlist violation: npm build",
                "scripts/agent-dev-run -- npm run build",
            ),
            (
                "impostor dev runner outside the repo",
                "/tmp/scripts/agent-dev-run -- npm run dev",
            ),
            (
                "impostor lane runner outside the repo",
                "/tmp/scripts/agent-resource-run -- cargo test --lib",
            ),
            (
                "forged selftest-guardian-release env",
                "MURMUR_AGENT_SELFTEST_GUARDIAN_RELEASE=/tmp/x scripts/agent-resource-run -- true",
            ),
            (
                "forged resource-lane-active env",
                "MURMUR_AGENT_RESOURCE_LANE_ACTIVE=1 scripts/agent-dev-run -- npm run dev",
            ),
        ]
        for label, cmd in anti_bypass_heavy:
            got = "HEAVY" if resource_policy.command_is_heavy_in(cmd, SOURCE_ROOT) else "LIGHT"
            test.result(f"anti-bypass classifier: {label}", got, "HEAVY")

        # The lane runner (agent-resource-run) is only meant to wrap bounded
        # one-off commands; wrapping a long-lived dev server in it (instead
        # of agent-dev-run) is itself a lane-starvation misuse pattern -- and
        # it is the one case here where command_is_dev_in is the function
        # that actually reproduces the original BLOCK (command_is_heavy_in
        # is also true for these, but the dev-axis classification is the one
        # this pattern specifically hinges on).
        anti_bypass_dev = [
            (
                "lane runner misused for a long-lived dev server",
                "scripts/agent-resource-run -- npm run dev",
            ),
            (
                "lane runner misused for a chdir'd long-lived dev server",
                "scripts/agent-resource-run --chdir /tmp/deck npm run dev -- --port 4173",
            ),
        ]
        for label, cmd in anti_bypass_dev:
            got = "DEV" if resource_policy.command_is_dev_in(cmd, SOURCE_ROOT) else "NOT-DEV"
            test.result(f"anti-bypass classifier: {label}", got, "DEV")

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

    print("-- verifier-only finish gate --")
    for vendor in ("codex", "claude"):
        repo = _finish_repo()
        got, _ = test.invoke(
            vendor,
            "finish-guard",
            "git -c user.name=x commit -m x",
            repo,
            use_default_finish=True,
        )
        test.result(f"{vendor}: normal worktree commit", got, "ALLOW")

        _, common, _, _ = _repo_context(repo)
        malformed = common / "agent-harness" / "v2" / "tasks" / "malformed"
        malformed.mkdir(parents=True)
        (malformed / "task.json").write_text("{", encoding="utf-8")
        got, _ = test.invoke(
            vendor,
            "finish-guard",
            "git commit -m x",
            repo,
            extra_env={"MURMUR_AGENT_TASK_ID": "malformed"},
        )
        test.result(f"{vendor}: malformed explicit task fails closed", got, "BLOCK")
        shutil.rmtree(malformed)

        bad = common / "agent-harness" / "v2" / "tasks" / "bad-task"
        bad.mkdir(parents=True)
        (bad / "task.json").write_text(
            json.dumps(
                {
                    "schema_version": 2,
                    "task_id": "bad-task",
                    "worktree_path": str(repo.resolve()),
                    "contract_sha256": "0" * 64,
                }
            )
            + "\n",
            encoding="utf-8",
        )
        got, _ = test.invoke(
            vendor,
            "finish-guard",
            "git commit -m x",
            repo,
        )
        test.result(f"{vendor}: malformed claiming task fails closed", got, "BLOCK")
        shutil.rmtree(bad)

        got, _ = test.invoke(vendor, "block-bash", "cargo test --lib", repo)
        test.result(f"{vendor}: no-task heavy command remains normal", got, "ALLOW")
        _write_task_claim(repo, "active-task")
        got, _ = test.invoke(vendor, "finish-guard", "git commit -m x", repo)
        test.result(f"{vendor}: raw task commit delegates to harness", got, "BLOCK")
        got, _ = test.invoke(vendor, "block-bash", "cargo test --lib", repo)
        test.result(f"{vendor}: task heavy command requires lane", got, "BLOCK")
        lane_runner = SOURCE_ROOT / "scripts" / "agent-resource-run"
        got, _ = test.invoke(
            vendor,
            "block-bash",
            f"{lane_runner} --chdir src-tauri -- cargo test --lib",
            repo,
        )
        test.result(f"{vendor}: task lane-wrapped command", got, "ALLOW")
        shutil.rmtree(repo.parent)

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
    command = ""
    process_cwd = Path.cwd()
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
    return _emit(
        args.vendor,
        reason,
        args.action,
        command=command,
        process_cwd=process_cwd,
    )


if __name__ == "__main__":
    raise SystemExit(main())
