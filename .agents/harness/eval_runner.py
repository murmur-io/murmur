#!/usr/bin/env python3
"""Murmur's dependency-free development-agent evaluation runner.

The harness evaluates the model *and* its CLI/rules/tool envelope.  It is
deliberately separate from Murmur's in-product AI evals.  Trial workspaces are
disposable and history-free; task manifests, fake solutions and hidden graders
never get copied into them.
"""

from __future__ import annotations

import argparse
import datetime as dt
import fnmatch
import hashlib
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tarfile
import tempfile
import time
import uuid
from pathlib import Path, PurePosixPath
from typing import Any, Dict, Iterable, List, Optional, Sequence, Tuple


REPO_ROOT = Path(__file__).resolve().parents[2]
HARNESS_ROOT = Path(__file__).resolve().parent
EVAL_ROOT = HARNESS_ROOT / "evals"
TASK_ROOT = EVAL_ROOT / "tasks"
SUITE_ROOT = EVAL_ROOT / "suites"
FIXTURE_ROOT = EVAL_ROOT / "fixtures"
GRADER_ROOT = EVAL_ROOT / "graders"

PASS = "PASS"
AGENT_FAIL = "AGENT_FAIL"
SCOPE_FAIL = "SCOPE_FAIL"
HARNESS_FAIL = "HARNESS_FAIL"
TIMEOUT = "TIMEOUT"
FLAKE = "FLAKE"
STATUSES = (PASS, AGENT_FAIL, SCOPE_FAIL, HARNESS_FAIL, TIMEOUT, FLAKE)

# Authentication should come from the installed CLI's credential store, not
# from ambient API-key variables that model-launched shell commands could dump.
SAFE_CHILD_ENVIRONMENT = {
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "TMPDIR",
    "TMP",
    "TEMP",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "SHELL",
    "TERM",
    "COLORTERM",
    "CODEX_HOME",
    "CLAUDE_CONFIG_DIR",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
    "CURL_CA_BUNDLE",
    "__CF_USER_TEXT_ENCODING",
}
SENSITIVE_ENVIRONMENT_NAMES = {
    "ANTHROPIC_API_KEY",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AZURE_CLIENT_ID",
    "AZURE_CLIENT_SECRET",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CODEX_API_KEY",
    "DATABASE_URL",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GITLAB_TOKEN",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "HOMEBREW_GITHUB_API_TOKEN",
    "MURMUR_DEV_DEK",
    "MURMUR_DEV_KEK",
    "NODE_AUTH_TOKEN",
    "NPM_TOKEN",
    "OPENAI_API_KEY",
    "PGPASSWORD",
    "RAILWAY_TOKEN",
    "SENTRY_AUTH_TOKEN",
    "SSH_AUTH_SOCK",
}
SENSITIVE_ENVIRONMENT_SUFFIXES = (
    "_TOKEN",
    "_KEY",
    "_SECRET",
    "_PASSWORD",
    "_CREDENTIAL",
    "_CREDENTIALS",
    "_DEK",
    "_KEK",
)
SENSITIVE_HOST_PATHS = (
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
)


class HarnessError(RuntimeError):
    """An infrastructure/configuration error, not an agent-quality failure."""


def sensitive_environment_name(name: str) -> bool:
    upper = name.upper()
    return upper in SENSITIVE_ENVIRONMENT_NAMES or upper.endswith(SENSITIVE_ENVIRONMENT_SUFFIXES)


def sanitized_child_environment(cwd: Path) -> Tuple[Dict[str, str], List[str]]:
    environment: Dict[str, str] = {}
    stripped: List[str] = []
    for name, value in os.environ.items():
        allowed = name in SAFE_CHILD_ENVIRONMENT or name.startswith("LC_")
        if allowed and not sensitive_environment_name(name):
            environment[name] = value
        else:
            stripped.append(name)
    environment.setdefault("PATH", os.defpath)
    environment["PWD"] = str(cwd)
    environment["CI"] = "1"
    environment["MURMUR_AGENT_EVAL"] = "1"
    environment["NO_COLOR"] = "1"
    environment["CLAUDE_CODE_SUBPROCESS_ENV_SCRUB"] = "1"
    return environment, sorted(set(stripped))


def utc_now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat().replace("+00:00", "Z")


def slug_run_id() -> str:
    stamp = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    return "%s-%s" % (stamp, uuid.uuid4().hex[:8])


def read_json(path: Path) -> Dict[str, Any]:
    try:
        with path.open("r", encoding="utf-8") as handle:
            value = json.load(handle)
    except (OSError, ValueError) as exc:
        raise HarnessError("cannot read JSON %s: %s" % (path, exc)) from exc
    if not isinstance(value, dict):
        raise HarnessError("JSON root must be an object: %s" % path)
    return value


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".tmp-%s" % os.getpid())
    with temporary.open("w", encoding="utf-8") as handle:
        json.dump(value, handle, indent=2, sort_keys=True, ensure_ascii=False)
        handle.write("\n")
    os.replace(str(temporary), str(path))


def write_text(path: Path, value: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(value, encoding="utf-8")


def relative_under(root: Path, value: str, label: str) -> Path:
    candidate = Path(value)
    if candidate.is_absolute() or ".." in candidate.parts:
        raise HarnessError("%s must be a safe relative path: %s" % (label, value))
    resolved_root = root.resolve()
    resolved = (resolved_root / candidate).resolve()
    try:
        resolved.relative_to(resolved_root)
    except ValueError as exc:
        raise HarnessError("%s escapes %s: %s" % (label, root, value)) from exc
    return resolved


def normalized_repo_path(value: str, label: str = "path") -> str:
    if not value or "\\" in value:
        raise HarnessError("%s must be a non-empty POSIX path: %r" % (label, value))
    path = PurePosixPath(value)
    if path.is_absolute() or ".." in path.parts or "." in path.parts:
        raise HarnessError("unsafe %s: %s" % (label, value))
    return str(path)


def task_path(task_id: str) -> Path:
    if not re.fullmatch(r"[a-z0-9][a-z0-9._-]{1,63}", task_id):
        raise HarnessError("invalid task id: %s" % task_id)
    return TASK_ROOT / (task_id + ".json")


def load_task(task_id: str) -> Dict[str, Any]:
    path = task_path(task_id)
    task = read_json(path)
    validate_task(task, path)
    return task


def load_suite(name: str) -> List[str]:
    path = relative_under(SUITE_ROOT, name + ".json", "suite")
    data = read_json(path)
    values = data.get("tasks")
    if not isinstance(values, list) or not values:
        raise HarnessError("suite %s must contain a non-empty tasks array" % name)
    result: List[str] = []
    for value in values:
        if not isinstance(value, str):
            raise HarnessError("suite %s contains a non-string task id" % name)
        load_task(value)
        result.append(value)
    if len(result) != len(set(result)):
        raise HarnessError("suite %s contains duplicate task ids" % name)
    return result


def validate_task(task: Dict[str, Any], path: Optional[Path] = None) -> None:
    where = str(path or "task")
    required = (
        "schema_version",
        "task_id",
        "description",
        "prompt",
        "source",
        "allowed_paths",
        "expected_change",
        "graders",
    )
    missing = [key for key in required if key not in task]
    if missing:
        raise HarnessError("%s missing keys: %s" % (where, ", ".join(missing)))
    if task["schema_version"] != 1:
        raise HarnessError("%s has unsupported schema_version" % where)
    task_id = task["task_id"]
    if not isinstance(task_id, str) or task_path(task_id).stem != task_id:
        raise HarnessError("%s has invalid task_id" % where)
    if path is not None and path.stem != task_id:
        raise HarnessError("task filename and task_id differ: %s" % path)
    for key in ("description", "prompt"):
        if not isinstance(task[key], str) or not task[key].strip():
            raise HarnessError("%s.%s must be a non-empty string" % (where, key))
    source = task["source"]
    if not isinstance(source, dict) or source.get("kind") not in ("fixture", "repo"):
        raise HarnessError("%s.source.kind must be fixture or repo" % where)
    if source["kind"] == "fixture":
        initial = source.get("initial")
        if not isinstance(initial, str):
            raise HarnessError("%s fixture source needs initial" % where)
        initial_path = relative_under(EVAL_ROOT, initial, "fixture initial")
        if not initial_path.is_dir():
            raise HarnessError("fixture initial does not exist: %s" % initial_path)
    elif "rev" in source and not isinstance(source["rev"], str):
        raise HarnessError("%s.source.rev must be a string" % where)
    allowed = task["allowed_paths"]
    if not isinstance(allowed, list) or any(not isinstance(item, str) for item in allowed):
        raise HarnessError("%s.allowed_paths must be an array of strings" % where)
    for item in allowed:
        normalized_repo_path(item, "allowed path")
    if not isinstance(task["expected_change"], bool):
        raise HarnessError("%s.expected_change must be boolean" % where)
    graders = task["graders"]
    if not isinstance(graders, list) or not graders:
        raise HarnessError("%s.graders must be a non-empty array" % where)
    grader_ids = set()
    for grader in graders:
        if not isinstance(grader, dict):
            raise HarnessError("%s grader must be an object" % where)
        if not isinstance(grader.get("id"), str) or not grader["id"]:
            raise HarnessError("%s grader needs id" % where)
        if grader["id"] in grader_ids:
            raise HarnessError("%s has duplicate grader id %s" % (where, grader["id"]))
        grader_ids.add(grader["id"])
        script = grader.get("script")
        if not isinstance(script, str):
            raise HarnessError("%s grader needs script" % where)
        script_path = relative_under(EVAL_ROOT, script, "grader script")
        try:
            script_path.relative_to(GRADER_ROOT.resolve())
        except ValueError as exc:
            raise HarnessError("grader must live under hidden graders/: %s" % script) from exc
        if not script_path.is_file():
            raise HarnessError("grader script missing: %s" % script_path)
    fake = task.get("fake", {})
    if not isinstance(fake, dict):
        raise HarnessError("%s.fake must be an object" % where)
    for key in ("good_overlay", "bad_overlay"):
        if key in fake:
            overlay = relative_under(EVAL_ROOT, fake[key], "fake overlay")
            if not overlay.is_dir():
                raise HarnessError("fake overlay missing: %s" % overlay)


def all_tasks() -> List[Dict[str, Any]]:
    result = []
    for path in sorted(TASK_ROOT.glob("*.json")):
        task = read_json(path)
        validate_task(task, path)
        result.append(task)
    return result


def safe_extract_tar(archive: Path, destination: Path) -> None:
    destination_real = destination.resolve()
    with tarfile.open(str(archive), "r:") as bundle:
        for member in bundle.getmembers():
            name = PurePosixPath(member.name)
            if name.is_absolute() or ".." in name.parts:
                raise HarnessError("git archive contains unsafe path: %s" % member.name)
            target = (destination / Path(*name.parts)).resolve()
            try:
                target.relative_to(destination_real)
            except ValueError as exc:
                raise HarnessError("git archive path escapes snapshot: %s" % member.name) from exc
            if member.issym() or member.islnk():
                link = PurePosixPath(member.linkname)
                if link.is_absolute() or ".." in link.parts:
                    raise HarnessError("git archive contains unsafe link: %s" % member.name)
        bundle.extractall(str(destination))


def run_simple(command: Sequence[str], cwd: Path, timeout: float = 30.0) -> subprocess.CompletedProcess:
    try:
        return subprocess.run(
            list(command),
            cwd=str(cwd),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            timeout=timeout,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise HarnessError("command failed: %s: %s" % (" ".join(command), exc)) from exc


def prepare_source(
    task: Dict[str, Any],
    workspace: Path,
    repo_root: Path = REPO_ROOT,
    base_sha: Optional[str] = None,
) -> Dict[str, Any]:
    workspace.mkdir(parents=True, exist_ok=False)
    source = task["source"]
    if source["kind"] == "fixture":
        initial = relative_under(EVAL_ROOT, source["initial"], "fixture initial")
        shutil.copytree(str(initial), str(workspace), dirs_exist_ok=True, symlinks=True)
        return {"kind": "fixture", "initial": source["initial"]}

    rev = base_sha or source.get("rev") or "HEAD"
    resolved = run_simple(["git", "rev-parse", "--verify", "%s^{commit}" % rev], repo_root)
    if resolved.returncode != 0:
        raise HarnessError("cannot resolve repo snapshot %s: %s" % (rev, resolved.stderr.strip()))
    commit = resolved.stdout.strip()
    with tempfile.NamedTemporaryFile(prefix="murmur-eval-archive-", suffix=".tar", delete=False) as handle:
        archive_path = Path(handle.name)
    try:
        with archive_path.open("wb") as archive_handle:
            process = subprocess.run(
                ["git", "archive", "--format=tar", commit],
                cwd=str(repo_root),
                stdout=archive_handle,
                stderr=subprocess.PIPE,
                check=False,
            )
        if process.returncode != 0:
            raise HarnessError("git archive failed: %s" % process.stderr.decode("utf-8", "replace"))
        safe_extract_tar(archive_path, workspace)
    finally:
        try:
            archive_path.unlink()
        except FileNotFoundError:
            pass

    # Hidden tasks, fake solutions, graders and prior outputs never enter a repo trial.
    shutil.rmtree(str(workspace / ".agents" / "harness" / "evals"), ignore_errors=True)
    shutil.rmtree(str(workspace / ".git"), ignore_errors=True)
    return {"kind": "repo", "commit": commit}


def escaping_workspace_links(workspace: Path) -> List[str]:
    root = workspace.resolve()
    violations = []
    for path in workspace.rglob("*"):
        if not path.is_symlink():
            continue
        resolved = path.resolve(strict=False)
        try:
            resolved.relative_to(root)
        except ValueError:
            violations.append(path.relative_to(workspace).as_posix())
    return violations


def validate_workspace_links(workspace: Path) -> None:
    violations = escaping_workspace_links(workspace)
    if violations:
        raise HarnessError("workspace symlink escapes trial root: %s" % ", ".join(violations))


def file_digest(path: Path) -> str:
    mode = path.lstat().st_mode & 0o7777
    digest = hashlib.sha256()
    digest.update(("mode:%o\0" % mode).encode("ascii"))
    if path.is_symlink():
        digest.update(b"symlink\0")
        digest.update(os.readlink(str(path)).encode("utf-8", "surrogateescape"))
    else:
        digest.update(b"file\0")
        with path.open("rb") as handle:
            while True:
                chunk = handle.read(1024 * 1024)
                if not chunk:
                    break
                digest.update(chunk)
    return digest.hexdigest()


def snapshot_tree(workspace: Path) -> Dict[str, str]:
    result: Dict[str, str] = {}
    for root, directories, files in os.walk(str(workspace), topdown=True, followlinks=False):
        directories[:] = sorted(directories)
        for name in sorted(files):
            path = Path(root) / name
            relative = path.relative_to(workspace).as_posix()
            result[relative] = file_digest(path)
        # os.walk lists symlinked directories as dirs but does not descend with followlinks=False.
        for name in list(directories):
            path = Path(root) / name
            if path.is_symlink():
                relative = path.relative_to(workspace).as_posix()
                result[relative] = file_digest(path)
                directories.remove(name)
    return result


def changed_paths(before: Dict[str, str], after: Dict[str, str]) -> List[Dict[str, str]]:
    result = []
    for path in sorted(set(before) | set(after)):
        if path not in before:
            result.append({"path": path, "kind": "added"})
        elif path not in after:
            result.append({"path": path, "kind": "deleted"})
        elif before[path] != after[path]:
            result.append({"path": path, "kind": "modified"})
    return result


def path_allowed(path: str, patterns: Iterable[str]) -> bool:
    for raw in patterns:
        pattern = normalized_repo_path(raw, "allowed path")
        prefix = pattern[:-3] if pattern.endswith("/**") else pattern
        if path == prefix or path.startswith(prefix.rstrip("/") + "/"):
            return True
        if any(char in pattern for char in "*?[") and fnmatch.fnmatchcase(path, pattern):
            return True
    return False


def save_changed_artifacts(workspace: Path, changes: List[Dict[str, str]], destination: Path) -> None:
    for change in changes:
        if change["kind"] == "deleted":
            continue
        source = workspace / Path(change["path"])
        if source.is_file() and not source.is_symlink() and source.stat().st_size <= 1024 * 1024:
            target = destination / Path(change["path"])
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(str(source), str(target))


def process_with_timeout(
    command: Sequence[str],
    cwd: Path,
    prompt: str,
    timeout_seconds: float,
    stdout_path: Path,
    stderr_path: Path,
) -> Dict[str, Any]:
    started = time.monotonic()
    environment, stripped_environment_variables = sanitized_child_environment(cwd)
    try:
        process = subprocess.Popen(
            list(command),
            cwd=str(cwd),
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            errors="replace",
            env=environment,
            start_new_session=True,
        )
    except OSError as exc:
        raise HarnessError("cannot start agent command: %s" % exc) from exc
    timed_out = False
    try:
        stdout, stderr = process.communicate(prompt, timeout=timeout_seconds)
    except subprocess.TimeoutExpired:
        timed_out = True
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            stdout, stderr = process.communicate(timeout=3)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            stdout, stderr = process.communicate()
    duration = time.monotonic() - started
    write_text(stdout_path, stdout)
    write_text(stderr_path, stderr)
    return {
        "command": list(command),
        "exit_code": process.returncode,
        "timed_out": timed_out,
        "duration_seconds": round(duration, 6),
        "stdout": stdout,
        "stderr": stderr,
        "stripped_environment_variables": stripped_environment_variables,
    }


def cli_version(agent: str) -> Optional[str]:
    if agent == "fake":
        return "builtin-fake-v1"
    executable = shutil.which(agent)
    if not executable:
        return None
    result = run_simple([executable, "--version"], REPO_ROOT, timeout=10)
    if result.returncode != 0:
        return None
    return (result.stdout or result.stderr).strip()


def extract_agent_response(stdout: str) -> str:
    messages: List[str] = []
    for line in stdout.splitlines():
        try:
            event = json.loads(line)
        except ValueError:
            continue
        if not isinstance(event, dict):
            continue
        if event.get("type") == "result" and isinstance(event.get("result"), str):
            messages.append(event["result"])
        item = event.get("item")
        if isinstance(item, dict) and item.get("type") in ("agent_message", "message"):
            for key in ("text", "content"):
                if isinstance(item.get(key), str):
                    messages.append(item[key])
        for key in ("output_text", "final_output"):
            if isinstance(event.get(key), str):
                messages.append(event[key])
    if messages:
        return messages[-1]
    return stdout[-100000:]


def base_prompt(task: Dict[str, Any], feedback: Optional[str] = None) -> str:
    allowed = task["allowed_paths"]
    allowed_text = "\n".join("- %s" % path for path in allowed) if allowed else "- no file edits allowed"
    prompt = """You are running inside a disposable, history-free Murmur evaluation workspace.

Solve the task below. Work only inside the current workspace. Do not search parent directories or
other checkouts. Hidden deterministic graders are intentionally unavailable to you. Do not create
commits. You may edit only these paths:
%s

Expected change: %s

TASK
%s

Finish with a concise summary and the checks you ran. The runner, not you, owns PASS/FAIL.
""" % (allowed_text, "yes" if task["expected_change"] else "no (analysis/no-op)", task["prompt"])
    if feedback:
        prompt += "\nREPAIR FEEDBACK FROM THE RUNNER\n%s\n" % feedback
    return prompt


def overlay_tree(overlay: Path, workspace: Path) -> None:
    for root, directories, files in os.walk(str(overlay), topdown=True, followlinks=False):
        directories[:] = sorted(directories)
        relative_root = Path(root).relative_to(overlay)
        for directory in directories:
            (workspace / relative_root / directory).mkdir(parents=True, exist_ok=True)
        for filename in sorted(files):
            source = Path(root) / filename
            target = workspace / relative_root / filename
            target.parent.mkdir(parents=True, exist_ok=True)
            if source.is_symlink():
                if target.exists() or target.is_symlink():
                    target.unlink()
                target.symlink_to(os.readlink(str(source)))
            else:
                shutil.copy2(str(source), str(target))


def invoke_fake(
    task: Dict[str, Any],
    workspace: Path,
    mode: str,
    attempt: int,
    trial_index: int,
    timeout_seconds: float,
    trace_dir: Path,
) -> Dict[str, Any]:
    selected = mode
    if mode == "repair":
        selected = "bad" if attempt == 0 else "good"
    elif mode == "flaky":
        selected = "good" if trial_index % 2 == 1 else "bad"
    if selected == "timeout":
        return process_with_timeout(
            [sys.executable, "-c", "import time; time.sleep(60)"],
            workspace,
            "",
            timeout_seconds,
            trace_dir / "agent.stdout.jsonl",
            trace_dir / "agent.stderr.log",
        )
    if selected == "fail":
        write_text(trace_dir / "agent.stdout.jsonl", "fake agent failed\n")
        write_text(trace_dir / "agent.stderr.log", "intentional fake failure\n")
        return {
            "command": ["fake", selected],
            "exit_code": 17,
            "timed_out": False,
            "duration_seconds": 0.0,
            "stdout": "fake agent failed\n",
            "stderr": "intentional fake failure\n",
        }
    fake = task.get("fake", {})
    overlay_key = "%s_overlay" % selected
    if overlay_key in fake:
        overlay = relative_under(EVAL_ROOT, fake[overlay_key], "fake overlay")
        overlay_tree(overlay, workspace)
    elif selected not in ("good", "bad"):
        raise HarnessError("unknown fake mode: %s" % mode)
    response = fake.get("%s_response" % selected, "fake %s response" % selected)
    write_text(trace_dir / "agent.stdout.jsonl", response + "\n")
    write_text(trace_dir / "agent.stderr.log", "")
    return {
        "command": ["fake", selected],
        "exit_code": 0,
        "timed_out": False,
        "duration_seconds": 0.0,
        "stdout": response,
        "stderr": "",
    }


def invoke_agent(
    agent: str,
    task: Dict[str, Any],
    workspace: Path,
    prompt: str,
    timeout_seconds: float,
    trace_dir: Path,
    fake_mode: str,
    attempt: int,
    trial_index: int,
    model: Optional[str] = None,
) -> Dict[str, Any]:
    if agent == "fake":
        return invoke_fake(task, workspace, fake_mode, attempt, trial_index, timeout_seconds, trace_dir)
    if agent == "codex":
        filesystem_entries = [
            '":minimal"="read"',
            '":tmpdir"="write"',
            '":slash_tmp"="write"',
            '":workspace_roots"={'
            + '"."="write","**/.env"="deny","**/.env.*"="deny",'
            + '"**/*.p12"="deny","**/*.pem"="deny",'
            + '"**/id_ed25519"="deny","**/id_rsa"="deny"}',
            *(json.dumps(path) + '="deny"' for path in SENSITIVE_HOST_PATHS),
        ]
        filesystem_profile = "{" + ",".join(filesystem_entries) + "}"
        command = [
            "codex",
            "exec",
            "--cd",
            str(workspace),
            "--ephemeral",
            "--skip-git-repo-check",
            "--ignore-user-config",
            "--strict-config",
            "--config",
            "permissions.murmur_eval.filesystem=" + filesystem_profile,
            "--config",
            "permissions.murmur_eval.network.enabled=false",
            "--config",
            'default_permissions="murmur_eval"',
            "--json",
            "-",
        ]
        if model:
            command[2:2] = ["--model", model]
    elif agent == "claude":
        denied_read_paths = list(SENSITIVE_HOST_PATHS) + [str(REPO_ROOT) + "/**"]
        sandbox_settings = json.dumps(
            {
                "permissions": {
                    "deny": ["Read(%s)" % path for path in denied_read_paths]
                    + ["Read(**/*.pem)", "Read(**/*.p12)", "Read(**/id_rsa)", "Read(**/id_ed25519)"]
                },
                "sandbox": {
                    "enabled": True,
                    "failIfUnavailable": True,
                    "allowUnsandboxedCommands": False,
                    "filesystem": {"denyRead": ["~/"], "allowRead": [str(workspace)]},
                    "network": {"deniedDomains": ["*"]},
                    "credentials": {
                        "files": [{"path": path, "mode": "deny"} for path in SENSITIVE_HOST_PATHS],
                        "envVars": [
                            {"name": name, "mode": "deny"} for name in sorted(SENSITIVE_ENVIRONMENT_NAMES)
                        ],
                    },
                },
            },
            separators=(",", ":"),
        )
        command = [
            "claude",
            "--print",
            "--output-format",
            "stream-json",
            "--verbose",
            "--no-session-persistence",
            "--permission-mode",
            "dontAsk",
            "--setting-sources",
            "project",
            "--settings",
            sandbox_settings,
            "--strict-mcp-config",
            "--mcp-config",
            '{"mcpServers":{}}',
            "--allowedTools",
            "Bash,Edit,Read,Write,Glob,Grep",
        ]
        if model:
            command.extend(["--model", model])
    else:
        raise HarnessError("unknown agent: %s" % agent)
    return process_with_timeout(
        command,
        workspace,
        prompt,
        timeout_seconds,
        trace_dir / "agent.stdout.jsonl",
        trace_dir / "agent.stderr.log",
    )


def run_grader(
    grader: Dict[str, Any],
    task: Dict[str, Any],
    workspace: Path,
    context_path: Path,
    trace_dir: Path,
) -> Dict[str, Any]:
    script = relative_under(EVAL_ROOT, grader["script"], "grader script")
    command = [
        sys.executable,
        str(script),
        "--task",
        task["task_id"],
        "--workspace",
        str(workspace),
        "--context",
        str(context_path),
    ]
    command.extend(str(value) for value in grader.get("args", []))
    outcome = process_with_timeout(
        command,
        GRADER_ROOT,
        "",
        float(grader.get("timeout_seconds", 30)),
        trace_dir / (grader["id"] + ".stdout.jsonl"),
        trace_dir / (grader["id"] + ".stderr.log"),
    )
    result: Dict[str, Any] = {
        "id": grader["id"],
        "duration_seconds": outcome["duration_seconds"],
        "exit_code": outcome["exit_code"],
        "timed_out": outcome["timed_out"],
        "environment": {
            "policy": "minimal-v1",
            "stripped_variable_names": outcome.get("stripped_environment_variables", []),
        },
    }
    if outcome["timed_out"]:
        result.update({"harness_error": True, "pass": False, "message": "grader timed out"})
        return result
    if outcome["exit_code"] != 0:
        result.update(
            {
                "harness_error": True,
                "pass": False,
                "message": "grader exited %s: %s" % (outcome["exit_code"], outcome["stderr"][-1000:]),
            }
        )
        return result
    payload: Optional[Dict[str, Any]] = None
    for line in reversed(outcome["stdout"].splitlines()):
        try:
            candidate = json.loads(line)
        except ValueError:
            continue
        if isinstance(candidate, dict):
            payload = candidate
            break
    if payload is None or not isinstance(payload.get("pass"), bool):
        result.update({"harness_error": True, "pass": False, "message": "grader emitted invalid JSON"})
        return result
    result.update(payload)
    result["harness_error"] = False
    return result


def grade_attempt(
    task: Dict[str, Any],
    workspace: Path,
    before: Dict[str, str],
    response_text: str,
    agent_outcome: Dict[str, Any],
    trace_dir: Path,
) -> Dict[str, Any]:
    after = snapshot_tree(workspace)
    changes = changed_paths(before, after)
    violations = [change["path"] for change in changes if not path_allowed(change["path"], task["allowed_paths"])]
    for path in escaping_workspace_links(workspace):
        if path not in violations:
            violations.append(path)
    context = {
        "schema_version": 1,
        "task_id": task["task_id"],
        "response_text": response_text,
        "changes": changes,
        "scope_violations": violations,
        "agent_exit_code": agent_outcome["exit_code"],
        "agent_timed_out": agent_outcome["timed_out"],
    }
    context_path = trace_dir / "grader-context.json"
    write_json(context_path, context)
    write_json(trace_dir / "tree-before.json", before)
    write_json(trace_dir / "tree-after.json", after)
    write_json(trace_dir / "changes.json", changes)
    save_changed_artifacts(workspace, changes, trace_dir / "changed-files")

    reasons: List[str] = []
    graders: List[Dict[str, Any]] = []
    if violations:
        status = SCOPE_FAIL
        reasons.append("write/symlink scope boundary violated: %s" % ", ".join(violations))
    elif agent_outcome["timed_out"]:
        status = TIMEOUT
        reasons.append("agent exceeded wall timeout")
    elif agent_outcome["exit_code"] != 0:
        status = AGENT_FAIL
        reasons.append("agent exited with code %s" % agent_outcome["exit_code"])
    else:
        status = PASS
        if task["expected_change"] and not changes:
            status = AGENT_FAIL
            reasons.append("task required a change but workspace is unchanged")
        if not task["expected_change"] and changes:
            status = AGENT_FAIL
            reasons.append("analysis/no-op task modified the workspace")
        for grader in task["graders"]:
            result = run_grader(grader, task, workspace, context_path, trace_dir / "graders")
            graders.append(result)
        harness_errors = [item for item in graders if item.get("harness_error")]
        failed = [item for item in graders if not item.get("pass")]
        if harness_errors:
            status = HARNESS_FAIL
            reasons.extend("grader %s: %s" % (item["id"], item.get("message", "infra error")) for item in harness_errors)
        elif failed:
            status = AGENT_FAIL
            reasons.extend("grader %s: %s" % (item["id"], item.get("message", "failed")) for item in failed)

    return {
        "status": status,
        "reasons": reasons,
        "changes": changes,
        "scope_violations": violations,
        "graders": graders,
        "after": after,
    }


def run_trial(
    task: Dict[str, Any],
    agent: str,
    trial_index: int,
    trace_dir: Path,
    timeout_seconds: float,
    repair_rounds: int,
    fake_mode: str = "good",
    repo_root: Path = REPO_ROOT,
    base_sha: Optional[str] = None,
    keep_workspace: bool = False,
    model: Optional[str] = None,
) -> Dict[str, Any]:
    started = time.monotonic()
    trace_dir.mkdir(parents=True, exist_ok=False)
    write_json(trace_dir / "task.json", task)
    temporary = Path(tempfile.mkdtemp(prefix="murmur-agent-eval-%s-" % task["task_id"]))
    workspace = temporary / "workspace"
    rounds: List[Dict[str, Any]] = []
    try:
        source = prepare_source(task, workspace, repo_root=repo_root, base_sha=base_sha)
        validate_workspace_links(workspace)
        if (workspace / ".git").exists():
            raise HarnessError("trial workspace unexpectedly contains .git")
        if (workspace / ".agents" / "harness" / "evals" / "graders").exists():
            raise HarnessError("hidden graders leaked into trial workspace")
        before = snapshot_tree(workspace)
        write_json(trace_dir / "source.json", source)
        write_json(trace_dir / "initial-tree.json", before)
        feedback: Optional[str] = None
        final_grade: Optional[Dict[str, Any]] = None
        for attempt in range(repair_rounds + 1):
            round_dir = trace_dir / "rounds" / ("%02d" % (attempt + 1))
            round_dir.mkdir(parents=True, exist_ok=False)
            prompt = base_prompt(task, feedback)
            write_text(round_dir / "prompt.txt", prompt)
            outcome = invoke_agent(
                agent,
                task,
                workspace,
                prompt,
                timeout_seconds,
                round_dir,
                fake_mode,
                attempt,
                trial_index,
                model,
            )
            write_json(
                round_dir / "agent-invocation.json",
                {
                    "command": outcome["command"],
                    "exit_code": outcome["exit_code"],
                    "timed_out": outcome["timed_out"],
                    "duration_seconds": outcome["duration_seconds"],
                    "environment": {
                        "policy": "minimal-v1",
                        "stripped_variable_names": outcome.get("stripped_environment_variables", []),
                    },
                },
            )
            response = extract_agent_response(outcome["stdout"])
            write_text(round_dir / "response.txt", response)
            grade = grade_attempt(task, workspace, before, response, outcome, round_dir)
            round_result = {
                "round": attempt + 1,
                "agent_exit_code": outcome["exit_code"],
                "agent_timed_out": outcome["timed_out"],
                "agent_duration_seconds": outcome["duration_seconds"],
                "status": grade["status"],
                "reasons": grade["reasons"],
                "changes": grade["changes"],
                "scope_violations": grade["scope_violations"],
                "graders": grade["graders"],
            }
            write_json(round_dir / "result.json", round_result)
            rounds.append(round_result)
            final_grade = grade
            if grade["status"] == PASS:
                break
            if grade["status"] != AGENT_FAIL or attempt >= repair_rounds:
                break
            feedback = "\n".join(grade["reasons"]) or "Deterministic acceptance checks failed."
        assert final_grade is not None
        if keep_workspace:
            shutil.copytree(str(workspace), str(trace_dir / "final-workspace"), symlinks=True)
        result = {
            "schema_version": 1,
            "task_id": task["task_id"],
            "agent": agent,
            "model": model,
            "trial": trial_index,
            "mode": "repair" if repair_rounds else "single-shot",
            "repair_budget": repair_rounds,
            "rounds_used": len(rounds),
            "status": final_grade["status"],
            "reasons": final_grade["reasons"],
            "scope_violations": final_grade["scope_violations"],
            "agent_claimed_success": bool(rounds[-1]["agent_exit_code"] == 0 and not rounds[-1]["agent_timed_out"]),
            "duration_seconds": round(time.monotonic() - started, 6),
            "rounds": rounds,
            "source": source,
            "created_at": utc_now(),
        }
    except Exception as exc:
        result = {
            "schema_version": 1,
            "task_id": task.get("task_id", "unknown"),
            "agent": agent,
            "model": model,
            "trial": trial_index,
            "mode": "repair" if repair_rounds else "single-shot",
            "repair_budget": repair_rounds,
            "rounds_used": len(rounds),
            "status": HARNESS_FAIL,
            "reasons": ["%s: %s" % (type(exc).__name__, exc)],
            "scope_violations": [],
            "agent_claimed_success": False,
            "duration_seconds": round(time.monotonic() - started, 6),
            "rounds": rounds,
            "created_at": utc_now(),
        }
    finally:
        shutil.rmtree(str(temporary), ignore_errors=True)
    write_json(trace_dir / "result.json", result)
    return result


def aggregate_task_status(trials: Sequence[Dict[str, Any]]) -> str:
    values = {trial["status"] for trial in trials}
    if len(values) > 1:
        return FLAKE
    return next(iter(values)) if values else HARNESS_FAIL


def compute_metrics(results: Sequence[Dict[str, Any]]) -> Dict[str, Any]:
    grouped: Dict[str, List[Dict[str, Any]]] = {}
    for result in results:
        grouped.setdefault(result["task_id"], []).append(result)
    for trials in grouped.values():
        trials.sort(key=lambda item: item["trial"])
    task_count = len(grouped)
    trial_count = len(results)
    pass_at_1_count = sum(1 for values in grouped.values() if values[0]["status"] == PASS)
    any_pass_count = sum(1 for values in grouped.values() if any(item["status"] == PASS for item in values))
    all_pass_count = sum(1 for values in grouped.values() if values and all(item["status"] == PASS for item in values))
    false_green = sum(1 for item in results if item["status"] != PASS and item.get("agent_claimed_success"))
    status_counts = {status: 0 for status in STATUSES}
    for values in grouped.values():
        status_counts[aggregate_task_status(values)] += 1
    duration = sum(float(item.get("duration_seconds", 0)) for item in results)
    return {
        "tasks": task_count,
        "trials": trial_count,
        "pass_at_1": round(pass_at_1_count / task_count, 6) if task_count else 0.0,
        "any_pass_at_k": round(any_pass_count / task_count, 6) if task_count else 0.0,
        "all_pass_at_k": round(all_pass_count / task_count, 6) if task_count else 0.0,
        "false_green_count": false_green,
        "false_green_rate": round(false_green / trial_count, 6) if trial_count else 0.0,
        "duration_seconds": round(duration, 6),
        "average_trial_duration_seconds": round(duration / trial_count, 6) if trial_count else 0.0,
        "status_counts": status_counts,
    }


def default_results_root(repo_root: Path = REPO_ROOT) -> Path:
    process = run_simple(["git", "rev-parse", "--git-common-dir"], repo_root)
    if process.returncode != 0:
        return repo_root / ".agent-harness-results" / "evals"
    common = Path(process.stdout.strip())
    if not common.is_absolute():
        common = (repo_root / common).resolve()
    return common / "agent-harness" / "evals"


def build_report(run: Dict[str, Any]) -> Dict[str, Any]:
    results = run["results"]
    grouped: Dict[str, List[Dict[str, Any]]] = {}
    for result in results:
        grouped.setdefault(result["task_id"], []).append(result)
    tasks = []
    for task_id, trials in sorted(grouped.items()):
        trials.sort(key=lambda item: item["trial"])
        tasks.append(
            {
                "task_id": task_id,
                "status": aggregate_task_status(trials),
                "trials": [item["status"] for item in trials],
                "reasons": [reason for item in trials for reason in item.get("reasons", [])],
            }
        )
    return {
        "schema_version": 1,
        "run_id": run["run_id"],
        "agent": run["agent"],
        "model": run.get("model"),
        "cli_version": run.get("cli_version"),
        "suite": run["suite"],
        "mode": run["mode"],
        "metrics": compute_metrics(results),
        "tasks": tasks,
        "created_at": utc_now(),
    }


def print_report(report: Dict[str, Any]) -> None:
    metrics = report["metrics"]
    print("Eval run: %s" % report["run_id"])
    subject = report["agent"]
    if report.get("model"):
        subject += "/" + report["model"]
    if report.get("cli_version"):
        subject += " (" + report["cli_version"] + ")"
    print("Agent: %s | suite: %s | mode: %s" % (subject, report["suite"], report["mode"]))
    print(
        "pass@1 %.1f%% | any-pass@k %.1f%% | all-pass@k %.1f%% | false-green %d | %.2fs"
        % (
            metrics["pass_at_1"] * 100,
            metrics["any_pass_at_k"] * 100,
            metrics["all_pass_at_k"] * 100,
            metrics["false_green_count"],
            metrics["duration_seconds"],
        )
    )
    for task in report["tasks"]:
        print("%-34s %-12s %s" % (task["task_id"], task["status"], ",".join(task["trials"])))


def command_list(args: argparse.Namespace) -> int:
    ids = load_suite(args.suite) if args.suite else [task["task_id"] for task in all_tasks()]
    tasks = [load_task(task_id) for task_id in ids]
    if args.json:
        print(json.dumps(tasks, indent=2, sort_keys=True))
    else:
        for task in tasks:
            print("%-34s change=%-5s source=%-7s %s" % (
                task["task_id"],
                str(task["expected_change"]).lower(),
                task["source"]["kind"],
                task["description"],
            ))
    return 0


def command_run(args: argparse.Namespace) -> int:
    if args.trials < 1:
        raise HarnessError("--trials must be at least 1")
    if args.repair_rounds < 0 or args.repair_rounds > 5:
        raise HarnessError("--repair-rounds must be between 0 and 5")
    ids = args.task or load_suite(args.suite)
    tasks = [load_task(task_id) for task_id in ids]
    run_id = args.run_id or slug_run_id()
    results_root = Path(args.output_dir).resolve() if args.output_dir else default_results_root()
    run_dir = results_root / run_id
    run_dir.mkdir(parents=True, exist_ok=False)
    metadata = {
        "schema_version": 1,
        "run_id": run_id,
        "agent": args.agent,
        "model": args.model,
        "cli_version": cli_version(args.agent),
        "suite": args.suite if not args.task else "selected",
        "task_ids": ids,
        "trials_per_task": args.trials,
        "mode": "repair" if args.repair_rounds else "single-shot",
        "repair_rounds": args.repair_rounds,
        "timeout_seconds": args.timeout,
        "base_sha": args.base_sha,
        "created_at": utc_now(),
    }
    write_json(run_dir / "run-metadata.json", metadata)
    results: List[Dict[str, Any]] = []
    for task in tasks:
        for trial in range(1, args.trials + 1):
            trace_dir = run_dir / "trials" / task["task_id"] / ("%03d" % trial)
            result = run_trial(
                task,
                args.agent,
                trial,
                trace_dir,
                args.timeout,
                args.repair_rounds,
                fake_mode=args.fake_mode,
                base_sha=args.base_sha,
                keep_workspace=args.keep_workspaces,
                model=args.model,
            )
            results.append(result)
            if not args.quiet:
                print("%s trial %d: %s" % (task["task_id"], trial, result["status"]), flush=True)
    run = dict(metadata)
    run["results"] = results
    write_json(run_dir / "run.json", run)
    report = build_report(run)
    write_json(run_dir / "report.json", report)
    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        print_report(report)
        print("Trace bundle: %s" % run_dir)
    return 0 if all(item["status"] == PASS for item in results) else 1


def resolve_report_path(value: str, output_dir: Optional[str]) -> Path:
    direct = Path(value)
    if direct.is_dir():
        return direct / "report.json"
    if direct.is_file():
        return direct
    root = Path(output_dir).resolve() if output_dir else default_results_root()
    return root / value / "report.json"


def command_report(args: argparse.Namespace) -> int:
    path = resolve_report_path(args.run, args.output_dir)
    report = read_json(path)
    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        print_report(report)
    return 0


def command_doctor(args: argparse.Namespace) -> int:
    checks: List[Dict[str, Any]] = []

    def add(name: str, ok: bool, message: str, required: bool = True) -> None:
        checks.append({"name": name, "ok": ok, "message": message, "required": required})

    add("python", sys.version_info >= (3, 9), sys.version.split()[0])
    add("git", shutil.which("git") is not None, shutil.which("git") or "missing")
    try:
        tasks = all_tasks()
        add("tasks", bool(tasks), "%d valid task(s)" % len(tasks))
        smoke = load_suite("smoke")
        add("smoke-suite", len(smoke) == 11, "%d task(s), expected 11" % len(smoke))
    except HarnessError as exc:
        add("task-config", False, str(exc))
    for cli in ("codex", "claude"):
        executable = shutil.which(cli)
        if not executable:
            add(cli, False, "not installed (fake-agent selftest still works)", required=False)
            continue
        version = run_simple([executable, "--version"], REPO_ROOT, timeout=10)
        add(cli, version.returncode == 0, (version.stdout or version.stderr).strip(), required=False)
    payload = {
        "ok": all(item["ok"] for item in checks if item["required"]),
        "checks": checks,
    }
    if args.json:
        print(json.dumps(payload, indent=2, sort_keys=True))
    else:
        for item in checks:
            marker = "PASS" if item["ok"] else ("WARN" if not item["required"] else "FAIL")
            print("%-4s %-20s %s" % (marker, item["name"], item["message"]))
    return 0 if payload["ok"] else 1


def create_tiny_repo(root: Path) -> None:
    root.mkdir(parents=True)
    commands = [
        ["git", "init", "-q"],
        ["git", "config", "user.name", "Harness Selftest"],
        ["git", "config", "user.email", "harness@example.invalid"],
    ]
    for command in commands:
        result = run_simple(command, root)
        if result.returncode != 0:
            raise HarnessError("selftest git setup failed: %s" % result.stderr)
    write_text(root / "visible.txt", "current\n")
    write_text(root / "history-secret.txt", "must-not-leak\n")
    hidden = root / ".agents" / "harness" / "evals" / "graders" / "hidden.py"
    write_text(hidden, "raise SystemExit('hidden grader leaked')\n")
    for command in (["git", "add", "."], ["git", "commit", "-qm", "first"]):
        result = run_simple(command, root)
        if result.returncode != 0:
            raise HarnessError("selftest git commit failed: %s" % result.stderr)
    (root / "history-secret.txt").unlink()
    result = run_simple(["git", "add", "-u"], root)
    if result.returncode != 0:
        raise HarnessError("selftest git add failed: %s" % result.stderr)
    result = run_simple(["git", "commit", "-qm", "remove history-only file"], root)
    if result.returncode != 0:
        raise HarnessError("selftest git commit failed: %s" % result.stderr)


def command_selftest(args: argparse.Namespace) -> int:
    assertions: List[Dict[str, Any]] = []

    def check(name: str, condition: bool, detail: str = "") -> None:
        assertions.append({"name": name, "pass": bool(condition), "detail": detail})

    with tempfile.TemporaryDirectory(prefix="murmur-eval-selftest-") as raw:
        root = Path(raw)
        try:
            smoke = load_suite("smoke")
            check("smoke-has-eleven-tasks", len(smoke) == 11, "count=%d" % len(smoke))
            good_results = []
            bad_results = []
            for task_id in smoke:
                task = load_task(task_id)
                good = run_trial(task, "fake", 1, root / "good" / task_id, 10, 0, fake_mode="good")
                bad = run_trial(task, "fake", 1, root / "bad" / task_id, 10, 0, fake_mode="bad")
                good_results.append(good)
                bad_results.append(bad)
            check(
                "hidden-graders-distinguish-good",
                all(item["status"] == PASS for item in good_results),
                json.dumps({item["task_id"]: item["status"] for item in good_results}, sort_keys=True),
            )
            check(
                "hidden-graders-distinguish-bad",
                all(item["status"] != PASS for item in bad_results),
                json.dumps({item["task_id"]: item["status"] for item in bad_results}, sort_keys=True),
            )

            scope = next(item for item in bad_results if item["task_id"] == "out-of-scope-attempt")
            check("scope-hard-fail", scope["status"] == SCOPE_FAIL, scope["status"])
            noop_good = next(item for item in good_results if item["task_id"] == "angular22-noop")
            noop_bad = next(item for item in bad_results if item["task_id"] == "angular22-noop")
            check("no-op-pass-and-edit-fail", noop_good["status"] == PASS and noop_bad["status"] == AGENT_FAIL)
            stale_good = next(item for item in good_results if item["task_id"] == "stale-receipt-hash")
            stale_bad = next(item for item in bad_results if item["task_id"] == "stale-receipt-hash")
            check("stale-tree-detected", stale_good["status"] == PASS and stale_bad["status"] == AGENT_FAIL)

            sample_task = load_task("hook-git-option-bypass")
            fixture_workspace = root / "fixture-workspace"
            prepare_source(sample_task, fixture_workspace)
            check("fixture-history-free", not (fixture_workspace / ".git").exists())
            check("fixture-hides-graders", not any(path.name == "smoke.py" for path in fixture_workspace.rglob("*")))
            escape_link = fixture_workspace / "hooks" / "escape-link"
            escape_link.symlink_to(root)
            check("external-symlink-detected", escaping_workspace_links(fixture_workspace) == ["hooks/escape-link"])
            escape_link.unlink()

            tiny_repo = root / "tiny-repo"
            create_tiny_repo(tiny_repo)
            repo_task = dict(sample_task)
            repo_task["source"] = {"kind": "repo", "rev": "HEAD"}
            repo_workspace = root / "repo-workspace"
            source = prepare_source(repo_task, repo_workspace, repo_root=tiny_repo)
            check("repo-source-is-archive", source.get("kind") == "repo" and len(source.get("commit", "")) == 40)
            check("repo-source-history-free", not (repo_workspace / ".git").exists() and not (repo_workspace / "history-secret.txt").exists())
            check("repo-source-hides-graders", not (repo_workspace / ".agents" / "harness" / "evals").exists())

            metric_input = [
                {"task_id": "a", "trial": 1, "status": PASS, "duration_seconds": 1, "agent_claimed_success": True},
                {"task_id": "a", "trial": 2, "status": AGENT_FAIL, "duration_seconds": 1, "agent_claimed_success": True},
                {"task_id": "b", "trial": 1, "status": AGENT_FAIL, "duration_seconds": 1, "agent_claimed_success": True},
                {"task_id": "b", "trial": 2, "status": PASS, "duration_seconds": 1, "agent_claimed_success": True},
            ]
            metrics = compute_metrics(metric_input)
            check(
                "metrics-pass-any-all",
                metrics["pass_at_1"] == 0.5 and metrics["any_pass_at_k"] == 1.0 and metrics["all_pass_at_k"] == 0.0,
                json.dumps(metrics, sort_keys=True),
            )
            check("metrics-flake", metrics["status_counts"][FLAKE] == 2)
            check("metrics-false-green", metrics["false_green_count"] == 2)

            repair_task = load_task("stale-receipt-hash")
            single = run_trial(repair_task, "fake", 1, root / "single", 10, 0, fake_mode="repair")
            repaired = run_trial(repair_task, "fake", 1, root / "repair", 10, 1, fake_mode="repair")
            exhausted = run_trial(repair_task, "fake", 1, root / "exhausted", 10, 2, fake_mode="bad")
            check("single-shot-is-default", single["status"] == AGENT_FAIL and single["rounds_used"] == 1)
            check("explicit-repair-can-pass", repaired["status"] == PASS and repaired["rounds_used"] == 2)
            check("repair-budget-is-bounded", exhausted["status"] == AGENT_FAIL and exhausted["rounds_used"] == 3)

            timeout = run_trial(repair_task, "fake", 1, root / "timeout", 0.05, 0, fake_mode="timeout")
            check("wall-timeout", timeout["status"] == TIMEOUT, timeout["status"])

            secret_name = "MURMUR_TEST_SECRET"
            secret_value = "eval-canary-must-never-enter-child-or-trace"
            had_secret = secret_name in os.environ
            previous_secret = os.environ.get(secret_name)
            os.environ[secret_name] = secret_value
            environment_stdout = root / "environment-child.stdout"
            environment_stderr = root / "environment-child.stderr"
            try:
                environment_result = process_with_timeout(
                    [
                        sys.executable,
                        "-c",
                        (
                            "import os,sys; value=os.environ.get('MURMUR_TEST_SECRET'); "
                            "print('absent' if value is None else value); "
                            "raise SystemExit(0 if value is None else 9)"
                        ),
                    ],
                    root,
                    "",
                    5,
                    environment_stdout,
                    environment_stderr,
                )
            finally:
                if had_secret:
                    assert previous_secret is not None
                    os.environ[secret_name] = previous_secret
                else:
                    os.environ.pop(secret_name, None)
            environment_trace = (
                environment_stdout.read_text(encoding="utf-8")
                + environment_stderr.read_text(encoding="utf-8")
                + json.dumps(environment_result.get("stripped_environment_variables", []))
            )
            check(
                "ambient-secret-hidden-from-child",
                environment_result["exit_code"] == 0
                and environment_result["stdout"].strip() == "absent"
                and secret_name in environment_result["stripped_environment_variables"],
            )
            check("ambient-secret-value-not-in-trace", secret_value not in environment_trace)
        except Exception as exc:  # selftest must turn unexpected failures into a useful result
            check("selftest-infrastructure", False, "%s: %s" % (type(exc).__name__, exc))

    payload = {"ok": all(item["pass"] for item in assertions), "assertions": assertions}
    if args.json:
        print(json.dumps(payload, indent=2, sort_keys=True))
    else:
        for item in assertions:
            print("%-4s %-34s %s" % ("PASS" if item["pass"] else "FAIL", item["name"], item["detail"]))
    return 0 if payload["ok"] else 1


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Murmur development-agent meta-eval harness")
    subparsers = parser.add_subparsers(dest="command", required=True)

    list_parser = subparsers.add_parser("list", help="list eval tasks")
    list_parser.add_argument("--suite", help="restrict to a suite")
    list_parser.add_argument("--json", action="store_true")
    list_parser.set_defaults(handler=command_list)

    run_parser = subparsers.add_parser("run", help="run disposable agent trials")
    run_parser.add_argument("--suite", default="smoke")
    run_parser.add_argument("--task", action="append", help="run a task id (repeatable; overrides suite)")
    run_parser.add_argument("--agent", choices=("codex", "claude", "fake"), required=True)
    run_parser.add_argument("--model", help="pin a model id/alias; omitted means the CLI default")
    run_parser.add_argument("--trials", type=int, default=1)
    run_parser.add_argument("--repair-rounds", type=int, default=0, help="explicit opt-in; normal evals are single-shot")
    run_parser.add_argument("--timeout", type=float, default=1800.0, help="wall timeout per agent invocation")
    run_parser.add_argument("--base-sha", help="override rev for source=repo tasks")
    run_parser.add_argument("--fake-mode", choices=("good", "bad", "repair", "flaky", "fail", "timeout"), default="good")
    run_parser.add_argument("--run-id")
    run_parser.add_argument("--output-dir")
    run_parser.add_argument("--keep-workspaces", action="store_true")
    run_parser.add_argument("--quiet", action="store_true")
    run_parser.add_argument("--json", action="store_true")
    run_parser.set_defaults(handler=command_run)

    report_parser = subparsers.add_parser("report", help="render a saved report")
    report_parser.add_argument("run", help="run id, run directory, or report.json")
    report_parser.add_argument("--output-dir")
    report_parser.add_argument("--json", action="store_true")
    report_parser.set_defaults(handler=command_report)

    doctor_parser = subparsers.add_parser("doctor", help="validate tasks and local CLI availability")
    doctor_parser.add_argument("--json", action="store_true")
    doctor_parser.set_defaults(handler=command_doctor)

    selftest_parser = subparsers.add_parser("selftest", help="exercise the harness with fake agents only")
    selftest_parser.add_argument("--json", action="store_true")
    selftest_parser.set_defaults(handler=command_selftest)
    return parser


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        return int(args.handler(args))
    except HarnessError as exc:
        print("harness error: %s" % exc, file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
