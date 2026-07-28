#!/usr/bin/env python3
"""Dependency-free deterministic fault tests for Harness v2."""

from __future__ import annotations

import argparse
import copy
import contextlib
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
from typing import Any, Dict, List, Mapping, Sequence

import cli as harness_cli
import task_runner as legacy
import verifier


ROOT = Path(__file__).resolve().parents[2]
CANONICAL_MURMUR_ORIGIN = "https://github.com/murmur-io/murmur.git"


class Tests:
    def __init__(self) -> None:
        self.count = 0
        self.failures: List[str] = []

    def equal(self, label: str, actual: Any, expected: Any) -> None:
        self.count += 1
        if actual == expected:
            print(f"  [PASS] {label}")
        else:
            self.failures.append(
                f"{label}: expected {expected!r}, found {actual!r}"
            )
            print(f"  [FAIL] {label}: {actual!r}")

    def true(self, label: str, value: bool) -> None:
        self.equal(label, bool(value), True)

    def raises(self, label: str, function: Any, contains: str = "") -> None:
        self.count += 1
        try:
            function()
        except Exception as exc:  # noqa: BLE001 - fault-injection assertion
            if not contains or contains in str(exc):
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
        capture_output=True,
        check=False,
    )
    if completed.returncode != 0:
        raise RuntimeError(
            f"git {' '.join(args)} failed: {completed.stderr.strip()}"
        )
    return completed.stdout.strip()


def _init_repo(repo: Path) -> str:
    repo.mkdir(parents=True)
    _git(repo, "init", "-q", "-b", "murmur")
    _git(repo, "config", "user.name", "QueaT")
    _git(repo, "config", "user.email", "kgm004a@gmail.com")
    (repo / "owned.txt").write_text("base\n", encoding="utf-8")
    _git(repo, "add", "owned.txt")
    _git(repo, "commit", "-q", "-m", "base")
    return _git(repo, "rev-parse", "HEAD")


def _standalone_driver(
    root: Path,
    *,
    name: str = ".murmur-agent-driver",
    canonical_origin: bool = True,
    detached: bool = True,
) -> tuple[Path, Path, str]:
    primary = root / "meetnotes"
    base = _init_repo(primary)
    driver = root / name
    _git(
        primary,
        "clone",
        "-q",
        "--local",
        "--no-hardlinks",
        "--no-checkout",
        str(primary),
        str(driver),
    )
    _git(driver, "config", "user.name", "QueaT")
    _git(driver, "config", "user.email", "kgm004a@gmail.com")
    if canonical_origin:
        _git(driver, "remote", "set-url", "origin", CANONICAL_MURMUR_ORIGIN)
    if detached:
        _git(driver, "checkout", "-q", "--detach", base)
    else:
        _git(driver, "checkout", "-q", "murmur")
    return primary, driver, base


def _invoke_open(driver: Path, args: argparse.Namespace) -> int:
    previous = Path.cwd()
    try:
        os.chdir(driver)
        with contextlib.redirect_stdout(io.StringIO()):
            return harness_cli.cmd_open(args)
    finally:
        os.chdir(previous)


def _git_tree_digest(git_dir: Path) -> str:
    digest = hashlib.sha256()
    for current, directories, files in os.walk(git_dir, followlinks=False):
        directories.sort()
        files.sort()
        current_path = Path(current)
        for name in [*directories, *files]:
            path = current_path / name
            relative = path.relative_to(git_dir).as_posix()
            metadata = path.lstat()
            digest.update(relative.encode("utf-8"))
            digest.update(b"\0")
            digest.update(str(metadata.st_mode).encode("ascii"))
            digest.update(b"\0")
            if path.is_symlink():
                digest.update(os.readlink(path).encode("utf-8"))
            elif path.is_file():
                digest.update(path.read_bytes())
            digest.update(b"\0")
    return digest.hexdigest()


def _open_args(task_id: str, base: str, branch: str) -> argparse.Namespace:
    return argparse.Namespace(
        task_id=task_id,
        prompt="open branch ownership selftest",
        prompt_file=None,
        owned=["owned.txt"],
        claim=[],
        reviewer="codex",
        base=base,
        branch=branch,
        kind="harness",
        expected_change=True,
    )


def open_branch_ownership_cases(test: Tests) -> None:
    with tempfile.TemporaryDirectory(prefix="murmur-v2-open-") as raw:
        root = Path(raw)
        _primary, driver, base = _standalone_driver(root)
        common = Path(
            _git(
                driver,
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            )
        )
        branch = "agent/v2/existing-valuable"
        _git(driver, "checkout", "-q", "-b", branch)
        (driver / "owned.txt").write_text(
            "base\nvaluable branch content\n", encoding="utf-8"
        )
        _git(driver, "add", "owned.txt")
        _git(driver, "commit", "-q", "-m", "valuable branch")
        valuable_oid = _git(driver, "rev-parse", "HEAD")
        _git(driver, "checkout", "-q", "--detach", base)

        test.raises(
            "OPEN rejects a pre-existing exact local branch",
            lambda: _invoke_open(
                driver,
                _open_args("existing-valuable", base, branch),
            ),
            "branch already exists",
        )
        test.equal(
            "OPEN preserves a pre-existing valuable branch",
            harness_cli._local_branch_oid(driver, branch),
            valuable_oid,
        )
        test.true(
            "OPEN preflight leaves no task record",
            not harness_cli.v2_task_dir(common, "existing-valuable").exists(),
        )

    with tempfile.TemporaryDirectory(prefix="murmur-v2-open-failure-") as raw:
        root = Path(raw)
        _primary, driver, base = _standalone_driver(root)
        branch = "agent/v2/forced-add-failure"
        original_run_capture = legacy.run_capture

        def fail_worktree_add(
            argv: Sequence[str],
            cwd: Path | None = None,
            *,
            check: bool = True,
            env: Mapping[str, str] | None = None,
        ) -> subprocess.CompletedProcess:
            if list(argv[:3]) == ["git", "worktree", "add"]:
                raise legacy.HarnessError("forced worktree-add failure")
            return original_run_capture(
                argv,
                cwd,
                check=check,
                env=env,
            )

        legacy.run_capture = fail_worktree_add
        try:
            test.raises(
                "OPEN reports a forced worktree-add failure",
                lambda: _invoke_open(
                    driver,
                    _open_args("forced-add-failure", base, branch),
                ),
                "forced worktree-add failure",
            )
        finally:
            legacy.run_capture = original_run_capture
        test.equal(
            "OPEN deletes its unchanged branch after worktree-add failure",
            harness_cli._local_branch_oid(driver, branch),
            None,
        )

    with tempfile.TemporaryDirectory(prefix="murmur-v2-open-moved-") as raw:
        root = Path(raw)
        _primary, driver, base = _standalone_driver(root)
        (driver / "owned.txt").write_text(
            "base\nvaluable moved content\n", encoding="utf-8"
        )
        _git(driver, "add", "owned.txt")
        _git(driver, "commit", "-q", "-m", "valuable moved tip")
        valuable_oid = _git(driver, "rev-parse", "HEAD")
        _git(driver, "checkout", "-q", "--detach", base)
        branch = "agent/v2/moved-during-failure"
        original_run_capture = legacy.run_capture

        def move_then_fail_worktree_add(
            argv: Sequence[str],
            cwd: Path | None = None,
            *,
            check: bool = True,
            env: Mapping[str, str] | None = None,
        ) -> subprocess.CompletedProcess:
            if list(argv[:3]) == ["git", "worktree", "add"]:
                _git(
                    driver,
                    "update-ref",
                    f"refs/heads/{branch}",
                    valuable_oid,
                )
                raise legacy.HarnessError("forced moved-branch failure")
            return original_run_capture(
                argv,
                cwd,
                check=check,
                env=env,
            )

        legacy.run_capture = move_then_fail_worktree_add
        try:
            test.raises(
                "OPEN reports failure after its branch moves",
                lambda: _invoke_open(
                    driver,
                    _open_args("moved-during-failure", base, branch),
                ),
                "forced moved-branch failure",
            )
        finally:
            legacy.run_capture = original_run_capture
        test.equal(
            "OPEN preserves a created branch that no longer has the expected OID",
            harness_cli._local_branch_oid(driver, branch),
            valuable_oid,
        )


def standalone_driver_open_cases(test: Tests) -> None:
    with tempfile.TemporaryDirectory(prefix="murmur-v2-driver-open-") as raw:
        root = Path(raw)
        primary, driver, base = _standalone_driver(root)
        empty_alternates = driver / ".git" / "objects" / "info" / "alternates"
        empty_alternates.parent.mkdir(parents=True, exist_ok=True)
        empty_alternates.write_bytes(b"")
        primary_git = primary / ".git"
        primary_refs_before = _git(
            primary,
            "for-each-ref",
            "--format=%(refname) %(objectname)",
        )
        primary_worktrees_before = _git(
            primary, "worktree", "list", "--porcelain"
        )
        primary_digest_before = _git_tree_digest(primary_git)

        task_id = "standalone-isolation"
        branch = f"agent/v2/{task_id}"
        opened = _invoke_open(
            driver,
            _open_args(task_id, base, branch),
        )
        primary_refs_after = _git(
            primary,
            "for-each-ref",
            "--format=%(refname) %(objectname)",
        )
        primary_worktrees_after = _git(
            primary, "worktree", "list", "--porcelain"
        )
        primary_digest_after = _git_tree_digest(primary_git)

        common = (driver / ".git").resolve()
        task_dir = harness_cli.v2_task_dir(common, task_id)
        task_root = root / ".murmur-agent-tasks" / "v2" / task_id
        worktree = task_root / "meetnotes"
        contract = legacy.load_json(task_dir / "task.json")
        runtime = legacy.load_json(task_dir / "runtime.json")
        metadata_valid = True
        try:
            loaded, loaded_dir, loaded_common = harness_cli.load_v2_task(
                task_id, driver
            )
            state = harness_cli.load_v2_state(task_dir)
            metadata_valid = (
                loaded == contract
                and loaded_dir == task_dir
                and loaded_common == common
                and state.get("status") == "OPEN"
            )
        except Exception:  # noqa: BLE001 - aggregate metadata assertion
            metadata_valid = False

        test.equal("OPEN standalone driver succeeds offline", opened, 0)
        test.equal(
            "OPEN creates task branch only in driver common",
            harness_cli._local_branch_oid(driver, branch),
            base,
        )
        test.equal(
            "OPEN creates no task branch in user primary",
            harness_cli._local_branch_oid(primary, branch),
            None,
        )
        test.equal(
            "OPEN leaves user primary refs byte-for-byte equivalent",
            primary_refs_after,
            primary_refs_before,
        )
        test.equal(
            "OPEN leaves user primary worktree registry unchanged",
            primary_worktrees_after,
            primary_worktrees_before,
        )
        test.equal(
            "OPEN leaves user primary .git digest unchanged",
            primary_digest_after,
            primary_digest_before,
        )
        test.equal(
            "OPEN task worktree uses fixed meetnotes leaf",
            worktree.name,
            "meetnotes",
        )
        test.true(
            "OPEN task worktree exists at the dedicated task root",
            worktree.is_dir() and not worktree.is_symlink(),
        )
        test.equal(
            "OPEN task worktree is registered only in driver common",
            Path(
                _git(
                    worktree,
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-common-dir",
                )
            ).resolve(),
            common,
        )
        test.equal(
            "OPEN contract records standalone driver common",
            contract["git_common_dir"],
            str(common),
        )
        test.equal(
            "OPEN contract records fixed task worktree",
            contract["worktree_path"],
            str(worktree.resolve()),
        )
        test.equal(
            "OPEN runtime records exact safe task root",
            runtime["task_root"],
            str(task_root.resolve()),
        )
        test.true("OPEN writes valid task metadata and state", metadata_valid)
        test.equal(
            "OPEN leaves standalone driver HEAD detached",
            _git(driver, "branch", "--show-current"),
            "",
        )

    with tempfile.TemporaryDirectory(prefix="murmur-v2-driver-linked-") as raw:
        root = Path(raw)
        primary = root / "meetnotes"
        base = _init_repo(primary)
        _git(primary, "remote", "add", "origin", CANONICAL_MURMUR_ORIGIN)
        driver = root / ".murmur-agent-driver"
        _git(
            primary,
            "worktree",
            "add",
            "-q",
            "--detach",
            str(driver),
            base,
        )
        test.raises(
            "OPEN rejects a linked driver worktree",
            lambda: _invoke_open(
                driver,
                _open_args(
                    "reject-linked",
                    base,
                    "agent/v2/reject-linked",
                ),
            ),
            "linked driver worktrees are forbidden",
        )

    with tempfile.TemporaryDirectory(prefix="murmur-v2-driver-origin-") as raw:
        root = Path(raw)
        _primary, driver, base = _standalone_driver(
            root, canonical_origin=False
        )
        test.raises(
            "OPEN rejects a local origin",
            lambda: _invoke_open(
                driver,
                _open_args(
                    "reject-local-origin",
                    base,
                    "agent/v2/reject-local-origin",
                ),
            ),
            "local/file origins are forbidden",
        )

    with tempfile.TemporaryDirectory(prefix="murmur-v2-driver-symlink-") as raw:
        root = Path(raw)
        _primary, driver, base = _standalone_driver(root)
        redirected = root / "redirected-tasks"
        redirected.mkdir()
        (root / ".murmur-agent-tasks").symlink_to(
            redirected, target_is_directory=True
        )
        branch = "agent/v2/reject-symlink-root"
        test.raises(
            "OPEN rejects a symlinked task-root component",
            lambda: _invoke_open(
                driver,
                _open_args("reject-symlink-root", base, branch),
            ),
            "unsafe or symlinked",
        )
        test.equal(
            "OPEN rejects symlink root before branch mutation",
            harness_cli._local_branch_oid(driver, branch),
            None,
        )

    with tempfile.TemporaryDirectory(prefix="murmur-v2-driver-dirty-") as raw:
        root = Path(raw)
        _primary, driver, base = _standalone_driver(root)
        (driver / "owned.txt").write_text(
            "base\ndirty driver\n", encoding="utf-8"
        )
        test.raises(
            "OPEN rejects a dirty standalone driver",
            lambda: _invoke_open(
                driver,
                _open_args(
                    "reject-dirty",
                    base,
                    "agent/v2/reject-dirty",
                ),
            ),
            "must be clean",
        )

    with tempfile.TemporaryDirectory(
        prefix="murmur-v2-driver-alternates-"
    ) as raw:
        root = Path(raw)
        primary, driver, base = _standalone_driver(root)
        alternates = driver / ".git" / "objects" / "info" / "alternates"
        alternates.parent.mkdir(parents=True, exist_ok=True)
        alternates.write_text(
            str(primary / ".git" / "objects") + "\n",
            encoding="utf-8",
        )
        test.raises(
            "OPEN rejects nonempty object alternates",
            lambda: _invoke_open(
                driver,
                _open_args(
                    "reject-alternates",
                    base,
                    "agent/v2/reject-alternates",
                ),
            ),
            "alternates must be absent or empty",
        )

    with tempfile.TemporaryDirectory(prefix="murmur-v2-driver-attached-") as raw:
        root = Path(raw)
        _primary, driver, base = _standalone_driver(root, detached=False)
        test.raises(
            "OPEN rejects an attached standalone driver HEAD",
            lambda: _invoke_open(
                driver,
                _open_args(
                    "reject-attached",
                    base,
                    "agent/v2/reject-attached",
                ),
            ),
            "HEAD must be detached",
        )

    with tempfile.TemporaryDirectory(prefix="murmur-v2-driver-name-") as raw:
        root = Path(raw)
        _primary, driver, base = _standalone_driver(root, name="driver")
        test.raises(
            "OPEN rejects a standalone clone with the wrong directory name",
            lambda: _invoke_open(
                driver,
                _open_args(
                    "reject-name",
                    base,
                    "agent/v2/reject-name",
                ),
            ),
            ".murmur-agent-driver",
        )

    with tempfile.TemporaryDirectory(prefix="murmur-v2-driver-existing-") as raw:
        root = Path(raw)
        _primary, driver, base = _standalone_driver(root)
        task_id = "reject-existing-root"
        branch = f"agent/v2/{task_id}"
        task_root = root / ".murmur-agent-tasks" / "v2" / task_id
        task_root.mkdir(parents=True)
        test.raises(
            "OPEN rejects a pre-existing task root",
            lambda: _invoke_open(
                driver,
                _open_args(task_id, base, branch),
            ),
            "task root already exists",
        )
        test.equal(
            "OPEN rejects pre-existing root before branch mutation",
            harness_cli._local_branch_oid(driver, branch),
            None,
        )


def _profile(
    paths: Sequence[str], claims: Sequence[str] = ()
) -> tuple[List[str], List[str], List[str]]:
    checks, reviews, risks = verifier.derive_profile(
        list(paths),
        list(claims),
        legacy.load_config(),
        reviewer="claude",
    )
    return (
        [item["id"] for item in checks],
        [item["kind"] for item in reviews],
        risks,
    )


def profile_cases(test: Tests) -> None:
    checks, reviews, risks = _profile(["docs/guide.md"])
    test.equal("PROFILE docs has no build check", checks, [])
    test.equal("PROFILE docs still has combined review", reviews, ["combined"])
    test.equal("PROFILE docs has no sensitive review", risks, [])

    checks, reviews, _ = _profile(["src-tauri/src/transcribe/render.rs"])
    test.equal("PROFILE Rust source always runs rust-lib", checks, ["rust-lib"])
    test.true("PROFILE Rust source has combined review", reviews == ["combined"])
    test.true("PROFILE Rust path does not infer runtime", "tauri-boot" not in checks)
    test.true("PROFILE Rust path does not infer performance", "perf-contracts" not in checks)

    checks, _, _ = _profile(["src-tauri/Cargo.toml"])
    test.equal("PROFILE Rust manifest runs rust-lib", checks, ["rust-lib"])

    checks, reviews, _ = _profile(["src/app/features/detail/detail.ts"])
    test.equal(
        "PROFILE Angular behavior runs lint build Playwright",
        checks,
        ["ng-lint", "ng-build", "playwright"],
    )
    test.equal("PROFILE Angular behavior has one reviewer", reviews, ["combined"])

    checks, _, _ = _profile(["src/app/features/detail/detail.scss"])
    test.equal(
        "PROFILE Angular style runs lint and build only",
        checks,
        ["ng-lint", "ng-build"],
    )

    checks, _, _ = _profile(
        ["src-tauri/src/lib.rs", "src/app/core/ipc.service.ts"]
    )
    test.equal(
        "PROFILE mixed surface is stable union",
        checks,
        ["rust-lib", "ng-lint", "ng-build", "playwright"],
    )

    checks, _, _ = _profile(
        ["src-tauri/src/transcribe/live.rs"], ["runtime", "performance"]
    )
    test.equal(
        "PROFILE explicit runtime and performance claims add checks",
        checks,
        ["rust-lib", "tauri-boot", "perf-contracts"],
    )

    checks, reviews, risks = _profile(["src-tauri/src/share/envelope.rs"])
    test.equal(
        "PROFILE protocol runs client and server checks",
        checks,
        ["rust-lib", "protocol-server"],
    )
    test.equal("PROFILE protocol actual risks", risks, ["egress", "protocol"])
    test.equal(
        "PROFILE protocol adds one cross-vendor specialist",
        reviews,
        ["combined", "egress-security", "protocol-security"],
    )
    checks, reviews, risks = _profile([".murmur-server-revision"])
    test.equal(
        "PROFILE server revision runs client and protocol checks",
        checks,
        ["rust-lib", "protocol-server"],
    )
    test.equal(
        "PROFILE server revision is protocol-sensitive",
        risks,
        ["protocol"],
    )
    test.equal(
        "PROFILE server revision adds protocol specialist",
        reviews,
        ["combined", "protocol-security"],
    )

    checks, reviews, risks = _profile(
        ["crates/murmur-protocol/src/envelope.rs"]
    )
    test.equal(
        "PROFILE protocol crate has protocol checks",
        checks,
        ["rust-lib", "protocol-server"],
    )
    test.equal("PROFILE protocol crate has protocol risk", risks, ["protocol"])
    test.equal(
        "PROFILE protocol crate adds specialist",
        reviews,
        ["combined", "protocol-security"],
    )

    checks, reviews, risks = _profile(["src-tauri/src/storage/meeting_store.rs"])
    test.equal("PROFILE lock surface retains rust baseline", checks, ["rust-lib"])
    test.equal("PROFILE shallow lock path matches", risks, ["lock"])
    test.equal(
        "PROFILE lock adds specialist",
        reviews,
        ["combined", "lock-security"],
    )

    checks, _, _ = _profile([".agents/harness/cli.py"])
    test.equal(
        "PROFILE harness control plane runs deterministic control-plane gates",
        checks,
        [
            "harness-python",
            "harness-v2-selftest",
            "receipt-selftest",
            "hook-selftest",
            "config-audit",
        ],
    )
    test.equal(
        "PROFILE v2 refuses protected self-certification scope",
        harness_cli._protected_v2_paths([".agents/harness"]),
        [".agents/harness"],
    )
    harness_python = legacy.load_config()["canonical_checks"][
        "harness-python"
    ]
    harness_python_result = subprocess.run(
        shlex.split(harness_python),
        cwd=str(ROOT),
        text=True,
        capture_output=True,
        check=False,
    )
    test.equal(
        "PROFILE canonical harness-python command executes",
        harness_python_result.returncode,
        0,
    )


def reviewer_tool_guard_cases(test: Tests) -> None:
    isolated_cwd = legacy.reviewer_execution_cwd()
    test.true(
        "REVIEWER isolated cwd and every ancestor are immutable root-owned directories",
        all(
            component.stat().st_uid == 0
            and stat.S_ISDIR(component.stat().st_mode)
            and not component.stat().st_mode & (stat.S_IWGRP | stat.S_IWOTH)
            for component in (isolated_cwd, *isolated_cwd.parents)
        ),
    )
    with tempfile.TemporaryDirectory(prefix="murmur-reviewer-cwd-") as raw:
        mutable_cwd = Path(raw)
        mutable_cwd.chmod(0o777)
        original_reviewer_cwd = legacy.REVIEWER_CWD
        legacy.REVIEWER_CWD = mutable_cwd
        try:
            test.raises(
                "REVIEWER mutable temporary cwd is rejected",
                legacy.reviewer_execution_cwd,
                "mutable by the invoking user",
            )
        finally:
            legacy.REVIEWER_CWD = original_reviewer_cwd

    codex_environment = legacy.reviewer_model_environment(
        {"HOME": "/Users/selftest", "PATH": "/usr/bin:/bin", "SHELL": "/bin/zsh"},
        vendor="codex",
        cwd=isolated_cwd,
    )
    test.equal(
        "REVIEWER shell is pinned independently of ambient startup state",
        codex_environment["SHELL"],
        "/bin/sh",
    )
    test.equal(
        "REVIEWER Codex HOME is pinned to the isolated cwd",
        codex_environment["HOME"],
        str(isolated_cwd),
    )
    test.equal(
        "REVIEWER Codex auth home remains explicit after HOME isolation",
        codex_environment["CODEX_HOME"],
        "/Users/selftest/.codex",
    )

    decision = legacy.REVIEWER_TOOL_DENIAL
    output = decision["hookSpecificOutput"]
    test.equal(
        "REVIEWER guard denies every intercepted local tool",
        output["permissionDecision"],
        "deny",
    )
    test.true(
        "REVIEWER guard never reflects model-controlled tool input",
        "tool_input" not in json.dumps(decision, sort_keys=True),
    )
    command = legacy.reviewer_tool_guard_command()
    test.true(
        "REVIEWER inline guard has no mutable worktree dependency",
        command.startswith("/usr/bin/printf ")
        and "reviewer_tool_guard" not in command
        and str(legacy.HARNESS_ROOT) not in command,
    )
    completed = subprocess.run(
        command,
        shell=True,
        text=True,
        input=json.dumps(
            {"tool_name": "Bash", "tool_input": {"command": "/bin/pwd"}}
        ),
        capture_output=True,
        check=False,
    )
    test.equal(
        "REVIEWER inline guard exits successfully",
        completed.returncode,
        0,
    )
    test.equal(
        "REVIEWER inline guard emits the static deny decision",
        json.loads(completed.stdout),
        decision,
    )

    with tempfile.TemporaryDirectory(prefix="murmur-reviewer-tools-") as raw:
        fixture_dir = Path(raw)

        def tool_fixture(
            name: str, events: Sequence[Mapping[str, Any]]
        ) -> Path:
            path = fixture_dir / name
            path.write_text(
                "".join(
                    json.dumps(event, sort_keys=True) + "\n"
                    for event in events
                ),
                encoding="utf-8",
            )
            return path

        claude_21220_projection = tool_fixture(
            "claude-2.1.220-sanitized.jsonl",
            [
                {
                    "type": "system",
                    "subtype": "init",
                    "claude_code_version": "2.1.220",
                    "tools": ["StructuredOutput"],
                    "mcp_servers": [],
                },
                {"type": "system", "subtype": "thinking_tokens"},
                {
                    "type": "assistant",
                    "message": {"content": [{"type": "thinking"}]},
                },
                {
                    "type": "assistant",
                    "message": {"content": [{"type": "text"}]},
                },
                {
                    "type": "assistant",
                    "message": {
                        "content": [
                            {
                                "type": "tool_use",
                                "name": "StructuredOutput",
                            }
                        ]
                    },
                },
                {"type": "user"},
                {"type": "rate_limit_event"},
                {"type": "result", "subtype": "success"},
            ],
        )
        test.equal(
            "REVIEWER sanitized Claude 2.1.220 StructuredOutput projection is admissible",
            legacy.reviewer_tool_activity(
                claude_21220_projection, "claude"
            ),
            [],
        )

        missing_init = tool_fixture(
            "claude-missing-init.jsonl",
            [
                {
                    "type": "assistant",
                    "message": {"content": [{"type": "text"}]},
                }
            ],
        )
        test.true(
            "REVIEWER Claude log without init telemetry is rejected",
            "missing Claude init telemetry"
            in legacy.reviewer_tool_activity(missing_init, "claude"),
        )

        valid_claude_init = {
            "type": "system",
            "subtype": "init",
            "tools": ["StructuredOutput"],
            "mcp_servers": [],
        }
        for label, tool_name in (("bash", "Bash"), ("read", "Read")):
            tool_log = tool_fixture(
                f"claude-{label}.jsonl",
                [
                    valid_claude_init,
                    {
                        "type": "assistant",
                        "message": {
                            "content": [
                                {
                                    "type": "tool_use",
                                    "name": tool_name,
                                }
                            ]
                        },
                    },
                ],
            )
            test.true(
                f"REVIEWER Claude {tool_name} tool use is rejected",
                bool(legacy.reviewer_tool_activity(tool_log, "claude")),
            )

        extra_tools = tool_fixture(
            "claude-extra-tools.jsonl",
            [
                {
                    **valid_claude_init,
                    "tools": ["StructuredOutput", "Read"],
                }
            ],
        )
        test.true(
            "REVIEWER unexpected Claude init tool surface is rejected",
            any(
                "unexpected-tools" in item
                for item in legacy.reviewer_tool_activity(
                    extra_tools, "claude"
                )
            ),
        )
        unexpected_mcp = tool_fixture(
            "claude-unexpected-mcp.jsonl",
            [
                {
                    **valid_claude_init,
                    "mcp_servers": [{"name": "local-filesystem"}],
                }
            ],
        )
        test.true(
            "REVIEWER unexpected Claude MCP surface is rejected",
            any(
                "unexpected-mcp" in item
                for item in legacy.reviewer_tool_activity(
                    unexpected_mcp, "claude"
                )
            ),
        )

        codex_prose = tool_fixture(
            "codex-prose.jsonl",
            [
                {
                    "type": "item.started",
                    "item": {"type": "reasoning"},
                },
                {
                    "type": "item.completed",
                    "item": {"type": "agent_message", "text": "PASS"},
                },
            ],
        )
        test.equal(
            "REVIEWER Codex reasoning and prose remain admissible",
            legacy.reviewer_tool_activity(codex_prose, "codex"),
            [],
        )
        for label, item_type in (
            ("command", "command_execution"),
            ("unknown", "dynamic_tool_call"),
        ):
            codex_tool = tool_fixture(
                f"codex-{label}.jsonl",
                [
                    {
                        "type": "item.started",
                        "item": {"type": item_type},
                    }
                ],
            )
            test.true(
                f"REVIEWER Codex {item_type} event fails closed",
                bool(legacy.reviewer_tool_activity(codex_tool, "codex")),
            )


def verdict_cases(test: Tests) -> None:
    base: Dict[str, Any] = {
        "verdict": "PASS",
        "findings": [],
        "proof_gaps": [],
        "probe_requests": [],
    }
    test.equal("VERDICT clean PASS", verifier.review_result_state(base), "PASSED")
    major = {
        **base,
        "findings": [
            {
                "severity": "MAJOR",
                "file": "x",
                "evidence": "broken",
                "required_fix": "fix",
            }
        ],
    }
    test.equal(
        "VERDICT PASS plus MAJOR is rejected",
        verifier.review_result_state(major),
        "NEEDS_FIX",
    )
    blocker = copy.deepcopy(major)
    blocker["findings"][0]["severity"] = "BLOCKER"
    test.equal(
        "VERDICT PASS plus BLOCKER is rejected",
        verifier.review_result_state(blocker),
        "NEEDS_FIX",
    )
    gap = {
        **base,
        "proof_gaps": [
            {
                "claim": "boot",
                "evidence_missing": "real launch",
                "how_to_prove": "runtime smoke",
            }
        ],
    }
    test.equal(
        "VERDICT PASS plus proof gap is rejected",
        verifier.review_result_state(gap),
        "NEEDS_EVIDENCE",
    )


def retry_cases(test: Tests) -> None:
    calls: List[int] = []

    def rate_limited(number: int) -> Mapping[str, Any]:
        calls.append(number)
        if number == 1:
            return {
                "ok": False,
                "transient": True,
                "error": "api_error_status=429",
                "retry_after_seconds": 0,
            }
        return {"ok": True, "result": "PASS"}

    result, attempts = verifier.retry_call(rate_limited, sleep=lambda _delay: None)
    test.equal("RETRY 429 then PASS attempts twice", calls, [1, 2])
    test.equal("RETRY 429 then PASS succeeds", result["result"], "PASS")
    test.equal("RETRY retains both labels", len(attempts), 2)

    timeout_calls: List[int] = []

    def timed_out_once(number: int) -> Mapping[str, Any]:
        timeout_calls.append(number)
        if number == 1:
            return {
                "ok": False,
                "transient": True,
                "error": "review timeout",
                "retry_after_seconds": 0,
            }
        return {"ok": True, "result": "PASS"}

    timeout_result, timeout_attempts = verifier.retry_call(
        timed_out_once, sleep=lambda _delay: None
    )
    test.equal("RETRY timeout then PASS attempts twice", timeout_calls, [1, 2])
    test.equal("RETRY timeout then PASS succeeds", timeout_result["result"], "PASS")
    test.equal("RETRY timeout telemetry keeps both attempts", len(timeout_attempts), 2)

    def always_transient(number: int) -> Mapping[str, Any]:
        return {
            "ok": False,
            "transient": True,
            "error": f"api_error_status=503 try={number}",
            "retry_after_seconds": 0,
        }

    paused_attempts: List[Mapping[str, Any]] = []
    try:
        verifier.retry_call(always_transient, sleep=lambda _delay: None)
    except verifier.ReviewPaused as exc:
        paused_attempts = exc.attempts
    test.equal(
        "RETRY second transient becomes explicit pause",
        len(paused_attempts),
        2,
    )


def _wait_until_missing(pid: int, timeout_seconds: float = 3.0) -> bool:
    deadline = time.monotonic() + timeout_seconds
    while legacy._pid_is_alive(pid) and time.monotonic() < deadline:
        time.sleep(0.05)
    return not legacy._pid_is_alive(pid)


def guardian_and_artifact_cases(test: Tests) -> None:
    with tempfile.TemporaryDirectory(prefix="murmur-v2-guardian-") as raw:
        root = Path(raw)

        # The leader returns 0 while a same-PGID descendant ignores TERM. The
        # guardian must escalate to KILL and the process result must remain red:
        # cleanup is not permission to green-wash leaked background work.
        descendant_pid_path = root / "leader-exit-descendant.pid"
        descendant = (
            "import os,pathlib,signal,time;"
            "signal.signal(signal.SIGTERM,signal.SIG_IGN);"
            f"pathlib.Path({str(descendant_pid_path)!r}).write_text(str(os.getpid()));"
            "time.sleep(30)"
        )
        leader = (
            "import pathlib,subprocess,sys,time;"
            f"pid_path=pathlib.Path({str(descendant_pid_path)!r});"
            f"subprocess.Popen([sys.executable,'-c',{descendant!r}]);"
            "deadline=time.monotonic()+3;"
            "exec('while not pid_path.exists() and "
            "time.monotonic()<deadline:\\n time.sleep(0.01)')"
        )
        leaked = legacy.run_logged_process(
            [sys.executable, "-c", leader],
            cwd=root,
            timeout_seconds=5,
            log_path=root / "leader-exit.log",
            term_grace_seconds=0.1,
        )
        descendant_pid = int(descendant_pid_path.read_text(encoding="utf-8"))
        test.true(
            "GUARDIAN leader exit drains surviving same-PGID descendant",
            _wait_until_missing(descendant_pid),
        )
        test.true(
            "GUARDIAN records live group after leader exit",
            leaked["leader_exited_with_live_group"],
        )
        test.equal(
            "GUARDIAN background descendant cannot green-wash exit zero",
            leaked["exit_code"],
            125,
        )
        test.equal(
            "GUARDIAN records background cleanup reason",
            leaked["termination_reason"],
            "leader-exited-with-live-group",
        )

        # A SIGKILLed runner cannot execute finally blocks. The detached
        # guardian must observe EOF on its liveness pipe and finish supervising
        # the complete managed group by itself.
        parent_kill_leader_pid = root / "parent-kill-leader.pid"
        parent_kill_descendant_pid = root / "parent-kill-descendant.pid"
        parent_kill_release = root / "parent-kill.release"
        parent_kill_descendant = (
            "import os,pathlib,signal,time;"
            "signal.signal(signal.SIGTERM,signal.SIG_IGN);"
            f"pathlib.Path({str(parent_kill_descendant_pid)!r}).write_text(str(os.getpid()));"
            "deadline=time.monotonic()+30;"
            f"\nwhile not pathlib.Path({str(parent_kill_release)!r}).exists() "
            "and time.monotonic()<deadline:\n time.sleep(0.05)"
        )
        parent_kill_managed = (
            "import os,pathlib,subprocess,sys,time;"
            f"pathlib.Path({str(parent_kill_leader_pid)!r}).write_text(str(os.getpid()));"
            f"subprocess.Popen([sys.executable,'-c',{parent_kill_descendant!r}]);"
            "deadline=time.monotonic()+30;"
            f"\nwhile not pathlib.Path({str(parent_kill_release)!r}).exists() "
            "and time.monotonic()<deadline:\n time.sleep(0.05)"
        )
        driver = (
            "import pathlib,sys;"
            "import task_runner;"
            f"task_runner.run_logged_process([sys.executable,'-c',{parent_kill_managed!r}],"
            f"cwd=pathlib.Path({str(root)!r}),timeout_seconds=30,"
            f"log_path=pathlib.Path({str(root / 'parent-kill.log')!r}),"
            "term_grace_seconds=0.1)"
        )
        driver_env = {
            **os.environ,
            "PYTHONPATH": str(Path(__file__).resolve().parent),
        }
        driver_process = subprocess.Popen(
            [sys.executable, "-c", driver],
            cwd=str(root),
            env=driver_env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        managed_pid = 0
        managed_descendant_pid = 0
        try:
            armed_deadline = time.monotonic() + 4.0
            while (
                not parent_kill_leader_pid.is_file()
                or not parent_kill_descendant_pid.is_file()
            ) and time.monotonic() < armed_deadline:
                time.sleep(0.05)
            test.true(
                "GUARDIAN parent-SIGKILL fixture arms complete group",
                parent_kill_leader_pid.is_file()
                and parent_kill_descendant_pid.is_file(),
            )
            if parent_kill_leader_pid.is_file() and parent_kill_descendant_pid.is_file():
                managed_pid = int(parent_kill_leader_pid.read_text(encoding="utf-8"))
                managed_descendant_pid = int(
                    parent_kill_descendant_pid.read_text(encoding="utf-8")
                )
                os.kill(driver_process.pid, signal.SIGKILL)
                driver_process.wait(timeout=3)
                result_deadline = time.monotonic() + 5.0
                guardian_results: List[Path] = []
                while time.monotonic() < result_deadline:
                    guardian_results = list(
                        root.glob("parent-kill-*.log.guardian.json")
                    )
                    if guardian_results:
                        break
                    time.sleep(0.05)
                test.true(
                    "GUARDIAN survives parent SIGKILL long enough to publish result",
                    len(guardian_results) == 1,
                )
                guardian_result = (
                    legacy.load_json(guardian_results[0])
                    if len(guardian_results) == 1
                    else {}
                )
                test.true(
                    "GUARDIAN records parent liveness loss",
                    bool(guardian_result.get("parent_lost")),
                )
                test.true(
                    "GUARDIAN parent SIGKILL drains managed leader",
                    _wait_until_missing(managed_pid),
                )
                test.true(
                    "GUARDIAN parent SIGKILL drains TERM-ignoring descendant",
                    _wait_until_missing(managed_descendant_pid),
                )
        finally:
            parent_kill_release.touch()
            if driver_process.poll() is None:
                try:
                    driver_process.wait(timeout=2)
                except subprocess.TimeoutExpired:
                    driver_process.kill()
                    driver_process.wait(timeout=3)

        # Reusing the same semantic label must allocate a fresh UUID namespace
        # and leave every byte of the prior execution untouched.
        log_base = root / "resume.log"
        first_logged = legacy.run_logged_process(
            [sys.executable, "-c", "print('first')"],
            cwd=root,
            timeout_seconds=5,
            log_path=log_base,
        )
        first_logged_bytes = {
            key: Path(str(first_logged[key])).read_bytes()
            for key in ("log", "guardian_path")
        }
        second_logged = legacy.run_logged_process(
            [sys.executable, "-c", "print('second')"],
            cwd=root,
            timeout_seconds=5,
            log_path=log_base,
        )
        test.true(
            "ARTIFACT repeated guarded log label gets a fresh UUID path",
            first_logged["log"] != second_logged["log"]
            and first_logged["guardian_path"] != second_logged["guardian_path"],
        )
        test.true(
            "ARTIFACT repeated guarded run preserves prior crash namespace",
            all(
                Path(str(first_logged[key])).read_bytes() == value
                for key, value in first_logged_bytes.items()
            ),
        )

        repo = root / "repo"
        _init_repo(repo)
        common = Path(
            _git(repo, "rev-parse", "--path-format=absolute", "--git-common-dir")
        )
        task_dir = common / "agent-harness" / "v2" / "tasks" / "artifacts"
        task_dir.mkdir(parents=True)
        legacy.atomic_write_json(
            task_dir / "task.json",
            {"worktree_path": str(repo.resolve())},
        )
        declared = {
            "id": "uuid-artifacts",
            "command": "printf same",
            "timeout_seconds": 5,
        }
        background_child = "import time;time.sleep(30)"
        background_leader = (
            "import subprocess,sys;"
            f"subprocess.Popen([sys.executable,'-c',{background_child!r}])"
        )
        background_check = legacy.run_check(
            repo,
            task_dir,
            {
                "id": "background-leak",
                "command": (
                    f"{json.dumps(sys.executable)} -c "
                    f"{json.dumps(background_leader)}"
                ),
                "timeout_seconds": 5,
            },
            "v2-resume",
        )
        test.true(
            "GUARDIAN check records background descendant lifecycle violation",
            background_check["leader_exited_with_live_group"],
        )
        test.true(
            "GUARDIAN check cannot PASS after background cleanup",
            background_check["exit_code"] != 0
            and not background_check["passed"]
            and background_check["outcome"] == "FAIL",
        )
        first_check = legacy.run_check(repo, task_dir, declared, "v2-resume")
        first_check_paths = [
            Path(str(first_check[key]))
            for key in (
                "log_path",
                "stdout_path",
                "stderr_path",
                "guardian_path",
                "sandbox_profile_path",
            )
        ]
        first_check_bytes = {path: path.read_bytes() for path in first_check_paths}
        second_check = legacy.run_check(repo, task_dir, declared, "v2-resume")
        second_check_paths = [
            Path(str(second_check[key]))
            for key in (
                "log_path",
                "stdout_path",
                "stderr_path",
                "guardian_path",
                "sandbox_profile_path",
            )
        ]
        test.true(
            "ARTIFACT immediate check resume gets disjoint UUID paths",
            set(first_check_paths).isdisjoint(second_check_paths),
        )
        test.true(
            "ARTIFACT immediate check resume preserves all prior bytes",
            all(path.read_bytes() == value for path, value in first_check_bytes.items()),
        )
        test.true(
            "ARTIFACT check evidence records exact stream and guardian paths",
            all(
                Path(str(first_check[key])).is_file()
                for key in ("stdout_path", "stderr_path", "guardian_path")
            ),
        )

        model_worktree = root / "model-worktree"
        model_worktree.mkdir()
        model_dir = root / "model-evidence"
        first_model = legacy.invoke_model(
            "fake",
            role="reviewer",
            prompt="selftest",
            schema_name="v2-review",
            worktree=model_worktree,
            task_dir=model_dir,
            label="review-combined-try-1",
            timeout_seconds=5,
            instructions_sha256="a" * 64,
        )
        first_model_paths = [
            Path(str(first_model[key]))
            for key in ("result_path", "invocation_path", "log")
        ]
        first_model_bytes = {path: path.read_bytes() for path in first_model_paths}
        second_model = legacy.invoke_model(
            "fake",
            role="reviewer",
            prompt="selftest",
            schema_name="v2-review",
            worktree=model_worktree,
            task_dir=model_dir,
            label="review-combined-try-1",
            timeout_seconds=5,
            instructions_sha256="a" * 64,
        )
        second_model_paths = [
            Path(str(second_model[key]))
            for key in ("result_path", "invocation_path", "log")
        ]
        test.true(
            "ARTIFACT repeated model label gets disjoint UUID paths",
            set(first_model_paths).isdisjoint(second_model_paths),
        )
        test.true(
            "ARTIFACT repeated model label preserves prior result/log/invocation",
            all(path.read_bytes() == value for path, value in first_model_bytes.items()),
        )


def readonly_review_wall_timeout_cases(test: Tests) -> None:
    with tempfile.TemporaryDirectory(prefix="murmur-v2-review-timeout-") as raw:
        root = Path(raw)
        repo = root / "repo"
        _init_repo(repo)
        attempt_dir = root / "attempt"
        fake_bin = root / "bin"
        fake_bin.mkdir()
        counter_path = root / "claude-invocations"
        fake_claude = fake_bin / "claude"
        fake_claude.write_text(
            "#!/usr/bin/env python3\n"
            "import json,os,pathlib,sys,time\n"
            "if '--version' in sys.argv:\n"
            " print('claude 2.1.999')\n"
            " raise SystemExit(0)\n"
            "sys.stdin.read()\n"
            f"counter=pathlib.Path({str(counter_path)!r})\n"
            "number=int(counter.read_text() or '0')+1 if counter.exists() else 1\n"
            "counter.write_text(str(number))\n"
            "print(json.dumps({'type':'system','subtype':'init',"
            "'tools':['StructuredOutput'],'mcp_servers':[]}),flush=True)\n"
            "if number == 1:\n"
            " time.sleep(10)\n"
            "result={'verdict':'PASS','summary':'second real process completed',"
            "'requirements_covered':['timeout retry'],'findings':[],"
            "'proof_gaps':[],'probe_requests':[]}\n"
            "print(json.dumps({'type':'result','session_id':f'session-{number}',"
            "'model':'claude-selftest','structured_output':result}),flush=True)\n",
            encoding="utf-8",
        )
        fake_claude.chmod(0o755)
        contract = {
            "task_id": "review-timeout",
            "description": "retry a real reviewer wall timeout",
        }
        plan = {
            "changed_paths": ["owned.txt"],
            "claims": [],
            "actual_risk_flags": [],
            "diff_sha256": "1" * 64,
            "plan_sha256": "2" * 64,
            "protocol_sha256": "3" * 64,
        }
        original_path = os.environ.get("PATH")
        original_load_config = legacy.load_config
        base_config = original_load_config()

        def timeout_config() -> Dict[str, Any]:
            return {
                **base_config,
                "reviewer_timeout_seconds": 1,
                "review_retry_max_delay_seconds": 0,
            }

        legacy.load_config = timeout_config
        os.environ["PATH"] = str(fake_bin) + os.pathsep + (original_path or "")
        try:
            record = verifier.invoke_readonly_review(
                contract=contract,
                plan=plan,
                worktree=repo,
                attempt_dir=attempt_dir,
                diff=b"diff --git a/owned.txt b/owned.txt\n",
                checks=[],
                review={"kind": "combined", "vendor": "claude"},
                probe_evidence_sha256=verifier.probe_evidence_hash([]),
                sleep=lambda _delay: None,
            )
        finally:
            legacy.load_config = original_load_config
            if original_path is None:
                os.environ.pop("PATH", None)
            else:
                os.environ["PATH"] = original_path
        attempts = record["attempts"]
        test.equal(
            "TIMEOUT real sleeping reviewer is retried exactly once",
            counter_path.read_text(encoding="utf-8"),
            "2",
        )
        test.equal("TIMEOUT retry record keeps both real attempts", len(attempts), 2)
        test.true(
            "TIMEOUT first real process is typed transient evidence",
            not attempts[0]["ok"]
            and attempts[0]["transient"]
            and attempts[0]["telemetry"]["terminal_reason"] == "timeout"
            and "ManagedProcessTimeout" in attempts[0]["error"],
        )
        test.true(
            "TIMEOUT second real process supplies the accepted review",
            attempts[1]["ok"] and record["result"]["verdict"] == "PASS",
        )
        background_child = "import time;time.sleep(30)"
        background_leader = (
            "import subprocess,sys;"
            f"subprocess.Popen([sys.executable,'-c',{background_child!r}])"
        )
        background_process_result = legacy.run_logged_process(
            [sys.executable, "-c", background_leader],
            cwd=root,
            timeout_seconds=5,
            log_path=root / "model-background.log",
            term_grace_seconds=0.1,
        )
        original_run_logged_process = legacy.run_logged_process
        legacy.run_logged_process = (
            lambda *_args, **_kwargs: dict(background_process_result)
        )
        os.environ["PATH"] = str(fake_bin) + os.pathsep + (original_path or "")
        try:
            test.raises(
                "GUARDIAN model cannot accept structured PASS with background descendant",
                lambda: legacy.invoke_model(
                    "claude",
                    role="reviewer",
                    prompt="background lifecycle violation",
                    schema_name="v2-review",
                    worktree=repo,
                    task_dir=attempt_dir,
                    label="review-background",
                    timeout_seconds=5,
                    instructions_sha256=plan["protocol_sha256"],
                ),
                "failed",
            )
        finally:
            legacy.run_logged_process = original_run_logged_process
            if original_path is None:
                os.environ.pop("PATH", None)
            else:
                os.environ["PATH"] = original_path
        events = [
            json.loads(line)
            for line in (attempt_dir / "events.jsonl").read_text(
                encoding="utf-8"
            ).splitlines()
            if line.strip()
        ]
        timeout_events = [
            event
            for event in events
            if event.get("event") == "model-process-exit"
            and event.get("label") == "review-combined-try-1"
        ]
        background_events = [
            event
            for event in events
            if event.get("event") == "model-process-exit"
            and event.get("label") == "review-background"
        ]
        test.true(
            "GUARDIAN model event preserves non-admissible background cleanup evidence",
            len(background_events) == 1
            and background_events[0].get("leader_exited_with_live_group") is True
            and background_events[0].get("exit_code") == 125,
        )
        test.equal(
            "TIMEOUT runner records the real timed-out process event",
            len(timeout_events),
            1,
        )
        if timeout_events:
            timeout_event = timeout_events[0]
            test.true(
                "TIMEOUT event records exact UUID log and guardian artifacts",
                bool(timeout_event.get("timed_out"))
                and Path(str(timeout_event.get("log_path"))).is_file()
                and Path(str(timeout_event.get("guardian_path"))).is_file(),
            )
            test.true(
                "TIMEOUT retry uses a disjoint model artifact namespace",
                timeout_event.get("log_path") != record["log_path"]
                and timeout_event.get("result_path") != record["result_path"]
                and timeout_event.get("invocation_path") != record["invocation_path"],
            )


def state_and_lock_cases(test: Tests) -> None:
    with tempfile.TemporaryDirectory(prefix="murmur-v2-state-") as raw:
        root = Path(raw)
        task_dir = root / "open-gap"
        task_dir.mkdir()
        code = (
            "import pathlib,sys;"
            f"sys.path.insert(0,{str(Path(__file__).resolve().parent)!r});"
            "import cli;"
            "cli.set_v2_state(pathlib.Path(sys.argv[1]),'OPEN',phase='open')"
        )
        environment = {
            **os.environ,
            "MURMUR_HARNESS_SELFTEST": "1",
            "MURMUR_HARNESS_SELFTEST_KILL_AFTER_STATE_EVENT": "OPEN",
        }
        killed = subprocess.run(
            [sys.executable, "-c", code, str(task_dir)],
            env=environment,
            capture_output=True,
            check=False,
        )
        test.equal(
            "STATE real SIGKILL occurs after authoritative OPEN event",
            killed.returncode,
            -signal.SIGKILL,
        )
        test.true(
            "STATE projection is absent at injected crash",
            not (task_dir / "state.json").exists(),
        )
        recovered = harness_cli.load_v2_state(task_dir)
        test.equal("STATE loader recovers OPEN from ledger", recovered["status"], "OPEN")
        test.equal(
            "STATE loader safely rebuilds projection",
            legacy.load_json(task_dir / "state.json"),
            recovered,
        )
        stale_projection = {
            **recovered,
            "status": "VERIFYING",
            "updated_at": "2000-01-01T00:00:00Z",
        }
        legacy.atomic_write_json(task_dir / "state.json", stale_projection)
        test.equal(
            "STATE stale divergent projection is repaired from ledger",
            harness_cli.load_v2_state(task_dir),
            recovered,
        )
        future_projection = {
            **recovered,
            "updated_at": "2999-01-01T00:00:00Z",
        }
        legacy.atomic_write_json(task_dir / "state.json", future_projection)
        test.raises(
            "STATE projection newer than ledger fails closed",
            lambda: harness_cli.load_v2_state(task_dir),
            "newer than",
        )
        legacy.atomic_write_json(task_dir / "state.json", recovered)

        lock_dir = root / "lock-task"
        lock_dir.mkdir()
        lock_code = (
            "import pathlib,sys,time;"
            f"sys.path.insert(0,{str(Path(__file__).resolve().parent)!r});"
            "import cli;"
            "lock=cli.acquire_v2_run_lock(pathlib.Path(sys.argv[1]),'selftest');"
            "print('ready',flush=True);time.sleep(5)"
        )
        owner = subprocess.Popen(
            [sys.executable, "-c", lock_code, str(lock_dir)],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            ready = owner.stdout.readline().strip() if owner.stdout else ""
            liveness, metadata = harness_cli.lock_liveness(lock_dir)
            test.equal("LOCK child acquired real flock", ready, "ready")
            test.equal("LOCK live owner is reported LIVE", liveness, "LIVE")
            test.equal(
                "LOCK live metadata carries owner PID",
                int((metadata or {}).get("pid", -1)),
                owner.pid,
            )
        finally:
            owner.terminate()
            owner.wait(timeout=3)
        liveness, _metadata = harness_cli.lock_liveness(lock_dir)
        test.equal("LOCK dead owner is reported STALE", liveness, "STALE")


def standalone_driver_lane_cases(test: Tests) -> None:
    """Independent driver/primary clones must still share one heavy-build lane."""

    with tempfile.TemporaryDirectory(prefix="murmur-v2-driver-lane-") as raw:
        root = Path(raw)
        primary = root / "meetnotes"
        driver = root / ".murmur-agent-driver"
        _init_repo(primary)
        _init_repo(driver)
        task_dirs = []
        for repository, task_id in (
            (primary, "primary-lane"),
            (driver, "driver-lane"),
        ):
            common = Path(
                _git(
                    repository,
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-common-dir",
                )
            )
            task_dir = common / "agent-harness" / "v2" / "tasks" / task_id
            task_dir.mkdir(parents=True)
            task_dirs.append(task_dir)
        primary_task, driver_task = task_dirs
        primary_root = legacy.shared_resource_root_for_task(primary_task)
        driver_root = legacy.shared_resource_root_for_task(driver_task)
        test.equal(
            "LANE standalone driver and primary resolve one resource root",
            driver_root,
            primary_root,
        )
        test.equal(
            "LANE shared root stays inside narrow task sibling",
            driver_root,
            root.resolve() / ".murmur-agent-tasks" / ".resources",
        )

        release_marker = root / "release-standalone-holder"
        holder_code = (
            "import pathlib,sys,time;"
            f"sys.path.insert(0,{str(Path(__file__).resolve().parent)!r});"
            "import task_runner;"
            "lease=task_runner.acquire_cargo_lane("
            "pathlib.Path(sys.argv[1]),2,command='standalone-driver-holder');"
            "print('ready',flush=True);"
            "marker=pathlib.Path(sys.argv[2]);deadline=time.monotonic()+10;"
            "\nwhile not marker.exists() and time.monotonic()<deadline:"
            "\n time.sleep(0.01)"
            "\n"
            "task_runner.release_cargo_lane(lease)"
        )
        holder = subprocess.Popen(
            [
                sys.executable,
                "-c",
                holder_code,
                str(primary_task),
                str(release_marker),
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        runner = ROOT / "scripts" / "agent-resource-run"
        try:
            ready = holder.stdout.readline().strip() if holder.stdout else ""
            blocked = subprocess.run(
                [
                    str(runner),
                    "--deadline-seconds",
                    "0.2",
                    "--",
                    "/usr/bin/true",
                ],
                cwd=str(driver),
                text=True,
                capture_output=True,
                check=False,
            )
            test.equal("LANE standalone holder acquired", ready, "ready")
            test.equal(
                "LANE operator wrapper cannot overlap independent clone",
                blocked.returncode,
                124,
            )
            test.true(
                "LANE independent-clone wait is visible",
                "waiting for lane" in blocked.stderr,
            )
        finally:
            release_marker.write_text("release\n", encoding="utf-8")
            if holder.poll() is None:
                try:
                    holder.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    holder.terminate()
                    holder.wait(timeout=3)
        released = subprocess.run(
            [str(runner), "--deadline-seconds", "1", "--", "/usr/bin/true"],
            cwd=str(driver),
            text=True,
            capture_output=True,
            check=False,
        )
        test.equal(
            "LANE independent clone proceeds after release",
            released.returncode,
            0,
        )


def checkpoint_cases(test: Tests) -> None:
    with tempfile.TemporaryDirectory(prefix="murmur-v2-checkpoint-") as raw:
        repo = Path(raw) / "repo"
        base = _init_repo(repo)
        (repo / "owned.txt").write_text("base\nchanged\n", encoding="utf-8")
        common = Path(
            _git(repo, "rev-parse", "--path-format=absolute", "--git-common-dir")
        )
        task_dir = common / "agent-harness" / "v2" / "tasks" / "checkpoint"
        attempt_dir = task_dir / "attempts" / ("a" * 64)
        (attempt_dir / "checks").mkdir(parents=True)
        contract = {
            "task_id": "checkpoint",
            "base_sha": base,
            "worktree_path": str(repo),
            "owned_paths": ["owned.txt"],
            "expected_change": True,
        }
        legacy.atomic_write_json(task_dir / "task.json", contract)
        paths, diff, tree = verifier.snapshot_scoped_diff(repo, contract, task_dir)
        plan = {
            "changed_paths": paths,
            "diff_sha256": legacy.sha256_bytes(diff),
            "tree_sha": tree,
            "plan_sha256": "1" * 64,
            "protocol_sha256": "2" * 64,
        }
        declared = {
            "id": "checkpoint",
            "command": "test -f owned.txt",
            "timeout_seconds": 10,
        }
        legacy.atomic_write_json(task_dir / "selftest-contract.json", contract)
        legacy.atomic_write_json(task_dir / "selftest-plan.json", plan)
        legacy.atomic_write_json(task_dir / "selftest-check.json", declared)
        code = (
            "import json,pathlib,sys;"
            f"sys.path.insert(0,{str(Path(__file__).resolve().parent)!r});"
            "import cli;"
            "td=pathlib.Path(sys.argv[1]);ad=pathlib.Path(sys.argv[2]);"
            "contract=json.loads((td/'selftest-contract.json').read_text());"
            "plan=json.loads((td/'selftest-plan.json').read_text());"
            "declared=json.loads((td/'selftest-check.json').read_text());"
            "cli._run_or_resume_check(contract,td,plan,ad,declared,"
            "pathlib.Path(contract['worktree_path']),"
            "checkpoint_number=1)"
        )
        killed = subprocess.run(
            [sys.executable, "-c", code, str(task_dir), str(attempt_dir)],
            env={
                **os.environ,
                "MURMUR_HARNESS_SELFTEST": "1",
                "MURMUR_HARNESS_SELFTEST_KILL_AFTER_CHECK": "1",
            },
            capture_output=True,
            check=False,
        )
        record_path = attempt_dir / "checks" / "checkpoint.json"
        test.equal(
            "CHECKPOINT process is SIGKILLed after artifact fsync",
            killed.returncode,
            -signal.SIGKILL,
        )
        test.true("CHECKPOINT record exists after SIGKILL", record_path.is_file())
        if not record_path.is_file():
            test.failures.append(
                "CHECKPOINT child stderr: "
                + killed.stderr.decode("utf-8", "replace")[-2000:]
            )
            return
        before_bytes = record_path.read_bytes()
        before_mtime = record_path.stat().st_mtime_ns
        record, did_run = harness_cli._run_or_resume_check(
            contract,
            task_dir,
            plan,
            attempt_dir,
            declared,
            repo,
            checkpoint_number=1,
        )
        test.equal("CHECKPOINT resume reuses green artifact", did_run, False)
        test.true("CHECKPOINT reused record remains green", record["evidence"]["passed"])
        test.equal("CHECKPOINT reused bytes are unchanged", record_path.read_bytes(), before_bytes)
        test.equal(
            "CHECKPOINT reused mtime is unchanged",
            record_path.stat().st_mtime_ns,
            before_mtime,
        )

        retryable = legacy.load_json(record_path)
        retryable["evidence"]["passed"] = False
        retryable["evidence"]["outcome"] = "BLOCKED"
        retryable["evidence"]["timed_out"] = True
        legacy.atomic_write_json(record_path, retryable)
        record, did_run = harness_cli._run_or_resume_check(
            contract,
            task_dir,
            plan,
            attempt_dir,
            declared,
            repo,
            checkpoint_number=2,
        )
        test.equal("CHECKPOINT retryable pause is rerun on resume", did_run, True)
        test.true("CHECKPOINT retry can become green", record["evidence"]["passed"])
        test.true(
            "CHECKPOINT resource wait telemetry is present",
            isinstance(record["evidence"].get("resource_wait_ms"), int),
        )
        release_marker = Path(raw) / "release-checkpoint-holder"
        lane_code = (
            "import pathlib,sys,time;"
            f"sys.path.insert(0,{str(Path(__file__).resolve().parent)!r});"
            "import task_runner;"
            "lock=task_runner.acquire_cargo_lane("
            "pathlib.Path(sys.argv[1]),2,command='lane-holder');"
            "print('ready',flush=True);"
            "marker=pathlib.Path(sys.argv[2]);deadline=time.monotonic()+10;"
            "\nwhile not marker.exists() and time.monotonic()<deadline:"
            "\n time.sleep(0.01)"
            "\nif marker.exists():"
            "\n time.sleep(0.05)"
            "\n"
            "task_runner.release_cargo_lane(lock)"
        )
        holder = subprocess.Popen(
            [
                sys.executable,
                "-c",
                lane_code,
                str(task_dir),
                str(release_marker),
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        try:
            ready = holder.stdout.readline().strip() if holder.stdout else ""

            class ReleaseOnVisibleWait(io.StringIO):
                def write(self, value: str) -> int:
                    written = super().write(value)
                    if "waiting for Cargo lane" in self.getvalue():
                        release_marker.write_text(
                            "release\n", encoding="utf-8"
                        )
                    return written

            wait_stderr = ReleaseOnVisibleWait()
            with contextlib.redirect_stderr(wait_stderr):
                waited = legacy.run_check(
                    repo,
                    task_dir,
                    {
                        "id": "resource-wait",
                        "command": "cargo --version",
                        "timeout_seconds": 5,
                    },
                    "resource-wait",
                )
            test.equal("CHECKPOINT controlled Cargo owner is ready", ready, "ready")
            test.true("CHECKPOINT contended Cargo probe passes", waited["passed"])
            test.true(
                "CHECKPOINT contended wait is visibly reported",
                "waiting for Cargo lane" in wait_stderr.getvalue(),
            )
            test.true(
                "CHECKPOINT evidence measures observed resource wait separately",
                waited["resource_wait_ms"] >= 20,
            )
        finally:
            release_marker.write_text("release\n", encoding="utf-8")
            if holder.poll() is None:
                try:
                    holder.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    holder.terminate()
                    holder.wait(timeout=3)


def protocol_and_runtime_cases(test: Tests) -> None:
    with tempfile.TemporaryDirectory(prefix="murmur-v2-protocol-") as raw:
        fixture = Path(raw) / "fixture"
        for relative in verifier.protocol_relative_paths(ROOT):
            source = ROOT / relative
            target = fixture / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
        before = verifier.protocol_bundle(fixture)["protocol_sha256"]
        runtime_impl = fixture / "scripts" / "harness-runtime-smoke.py"
        runtime_impl.write_text(
            runtime_impl.read_text(encoding="utf-8") + "\n# drift\n",
            encoding="utf-8",
        )
        after = verifier.protocol_bundle(fixture)["protocol_sha256"]
        test.true(
            "PROTOCOL runner-owned check implementation drift changes protocol hash",
            before != after,
        )
        test.raises(
            "PROTOCOL changed caller cannot plan under task-side protocol hash",
            lambda: verifier.executable_protocol_bundle(fixture),
            "executing Harness v2 protocol differs",
        )
        manifest = verifier.protocol_relative_paths(ROOT)
        test.true(
            "PROTOCOL manifest includes runtime smoke implementation",
            "scripts/harness-runtime-smoke.py" in manifest,
        )
        test.true(
            "PROTOCOL manifest includes its v2 fault suite",
            ".agents/harness/v2_selftest.py" in manifest,
        )
        test.true(
            "PROTOCOL manifest includes all canonical check scripts",
            all(
                path.relative_to(ROOT).as_posix() in manifest
                for path in (ROOT / ".agents" / "harness" / "checks").rglob("*")
                if path.is_file()
            ),
        )

        evidence_root = Path(raw) / "evidence"
        invocation_path = evidence_root / "results" / "review-invocation.json"
        invocation_path.parent.mkdir(parents=True)
        invocation = {
            "vendor": "claude",
            "role": "reviewer",
            "label": "review-combined-try-1",
            "instructions_sha256": "a" * 64,
            "session_id": "session-real",
            "model": "claude-test",
            "cli_version": "2.1.214",
            "invocation_id": "invocation-real",
            "argv": ["/usr/local/bin/claude", "--print"],
            "cwd": str(legacy.reviewer_execution_cwd()),
            "wall_timeout_seconds": 30,
            "removed_env_names": [],
            "created_at": legacy.utc_now(),
        }
        legacy.atomic_write_json(invocation_path, invocation)
        legacy.verify_model_invocation(
            evidence_root,
            vendor="claude",
            role="reviewer",
            label="review-combined-try-1",
            session_id="session-real",
            model="claude-test",
            cli_version="2.1.214",
            invocation_path_raw=str(invocation_path),
            invocation_sha256=legacy.sha256_file(invocation_path),
            expected_path=invocation_path,
            instructions_sha256="a" * 64,
            require_cwd_binding=True,
        )
        tampered = {**invocation, "role": "writer"}
        legacy.atomic_write_json(invocation_path, tampered)
        test.raises(
            "PROVENANCE forged reviewer invocation role is rejected",
            lambda: legacy.verify_model_invocation(
                evidence_root,
                vendor="claude",
                role="reviewer",
                label="review-combined-try-1",
                session_id="session-real",
                model="claude-test",
                cli_version="2.1.214",
                invocation_path_raw=str(invocation_path),
                invocation_sha256=legacy.sha256_file(invocation_path),
                expected_path=invocation_path,
                instructions_sha256="a" * 64,
                require_cwd_binding=True,
            ),
            "role",
        )
        tampered_cwd = {**invocation, "cwd": str(evidence_root.resolve())}
        legacy.atomic_write_json(invocation_path, tampered_cwd)
        test.raises(
            "PROVENANCE reviewer invocation outside isolated cwd is rejected",
            lambda: legacy.verify_model_invocation(
                evidence_root,
                vendor="claude",
                role="reviewer",
                label="review-combined-try-1",
                session_id="session-real",
                model="claude-test",
                cli_version="2.1.214",
                invocation_path_raw=str(invocation_path),
                invocation_sha256=legacy.sha256_file(invocation_path),
                expected_path=invocation_path,
                instructions_sha256="a" * 64,
                require_cwd_binding=True,
            ),
            "isolated cwd",
        )
        log_path = evidence_root / "logs" / "review.jsonl"
        log_path.parent.mkdir()
        log_path.write_text(
            json.dumps(
                {
                    "session_id": "session-from-another-run",
                    "model": "claude-test",
                }
            )
            + "\n",
            encoding="utf-8",
        )
        test.equal(
            "PROVENANCE real log exposes session mismatch",
            legacy.extract_model_metadata(
                log_path, "claude", "session-real"
            )["session_id"],
            "session-from-another-run",
        )

    runtime_path = ROOT / "scripts" / "harness-runtime-smoke.py"
    specification = importlib.util.spec_from_file_location(
        "harness_runtime_smoke_selftest", runtime_path
    )
    if specification is None or specification.loader is None:
        test.true("RUNTIME smoke module can be loaded", False)
        return
    runtime_module = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(runtime_module)
    with tempfile.TemporaryDirectory(prefix="murmur-runtime-budget-") as raw:
        root = Path(raw)
        mode, timeout = runtime_module.selected_timeout(
            root,
            explicit=None,
            warm_timeout=240,
            cold_timeout=900,
        )
        test.equal("RUNTIME cold checkout gets cold budget", (mode, timeout), ("cold", 900))
        binary = root / "src-tauri" / "target" / "debug" / "Murmur"
        binary.parent.mkdir(parents=True)
        binary.write_bytes(b"fixture")
        mode, timeout = runtime_module.selected_timeout(
            root,
            explicit=None,
            warm_timeout=240,
            cold_timeout=900,
        )
        test.equal("RUNTIME built checkout gets warm budget", (mode, timeout), ("warm", 240))
        mode, timeout = runtime_module.selected_timeout(
            root,
            explicit=17,
            warm_timeout=240,
            cold_timeout=900,
        )
        test.equal("RUNTIME explicit budget wins", (mode, timeout), ("warm", 17))


def commit_recovery_cases(test: Tests) -> None:
    with tempfile.TemporaryDirectory(prefix="murmur-v2-commit-") as raw:
        root = Path(raw)
        primary = root / "primary"
        primary.mkdir(parents=True)
        _git(primary, "init", "-q", "-b", "murmur")
        _git(primary, "config", "user.name", "QueaT")
        _git(primary, "config", "user.email", "kgm004a@gmail.com")
        for relative in verifier.protocol_relative_paths(ROOT):
            source = ROOT / relative
            target = primary / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
        primary_owned = primary / "docs" / "commit-recovery.md"
        primary_owned.parent.mkdir(parents=True, exist_ok=True)
        primary_owned.write_text("base\n", encoding="utf-8")
        _git(primary, "add", ".")
        _git(primary, "commit", "-q", "-m", "base")
        base = _git(primary, "rev-parse", "HEAD")
        repo = root / "task" / "meetnotes"
        repo.parent.mkdir()
        task_branch = "agent/v2/commit-crash"
        _git(
            primary,
            "worktree",
            "add",
            "-q",
            "-b",
            task_branch,
            str(repo),
            base,
        )
        owned = repo / "docs" / "commit-recovery.md"
        common = Path(
            _git(
                primary,
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            )
        )
        task_id = "commit-crash"
        task_dir = harness_cli.v2_task_dir(common, task_id)
        task_dir.mkdir(parents=True)
        contract: Dict[str, Any] = {
            "schema_version": 2,
            "task_id": task_id,
            "description": "real post-commit crash recovery",
            "kind": "docs",
            "base_sha": base,
            "contract_sha256": "",
            "repo_realpath": str(primary.resolve()),
            "git_common_dir": str(common.resolve()),
            "worktree_path": str(repo.resolve()),
            "branch": task_branch,
            "owned_paths": ["docs/commit-recovery.md"],
            "claims": [],
            "reviewer": "fake",
            "expected_change": True,
            "degraded_provenance": [{"event": "timeout"}],
            "created_at": legacy.utc_now(),
        }
        contract["contract_sha256"] = verifier.document_hash(
            contract, "contract_sha256"
        )
        legacy.validate_schema(
            contract,
            legacy.load_schema("v2-task"),
            label="v2 commit crash contract",
        )
        legacy.atomic_write_json(task_dir / "task.json", contract)
        legacy.atomic_write_json(
            task_dir / "runtime.json",
            {
                "schema_version": 2,
                "task_root": str(root),
                "shared_node_modules": None,
                "server_worktree": None,
                "server_source": str(root / "murmur-server"),
                "server_revision": None,
            },
        )
        harness_cli.set_v2_state(task_dir, "OPEN", phase="open")
        owned.write_text("base\ncommitted\n", encoding="utf-8")
        previous_review_verdict = os.environ.get(
            "MURMUR_HARNESS_FAKE_REVIEW_VERDICT"
        )
        os.environ["MURMUR_HARNESS_FAKE_REVIEW_VERDICT"] = "BLOCKED"
        try:
            with contextlib.redirect_stdout(io.StringIO()):
                incomplete = harness_cli.verify_task(
                    contract,
                    task_dir,
                    allow_test_adapter=True,
                )
        finally:
            if previous_review_verdict is None:
                os.environ.pop("MURMUR_HARNESS_FAKE_REVIEW_VERDICT", None)
            else:
                os.environ[
                    "MURMUR_HARNESS_FAKE_REVIEW_VERDICT"
                ] = previous_review_verdict
        test.equal(
            "REVIEW incomplete result pauses as NEEDS_EVIDENCE",
            incomplete,
            "NEEDS_EVIDENCE",
        )
        # An artifact-valid BLOCKED review is a deterministic checkpoint for
        # its exact diff/probe binding. Change the diff before expecting a new
        # reviewer invocation; unchanged evidence must not be sampled again.
        owned.write_text(
            "base\ncommitted after exact-diff change\n",
            encoding="utf-8",
        )
        with contextlib.redirect_stdout(io.StringIO()):
            verified = harness_cli.verify_task(
                contract,
                task_dir,
                allow_test_adapter=True,
            )
        test.equal("COMMIT fixture obtains exact v2 PASS", verified, "PASSED")
        passed_state = harness_cli.load_v2_state(task_dir)
        if verified != "PASSED":
            test.failures.append(
                "COMMIT fixture state detail: "
                + json.dumps(passed_state, sort_keys=True)
            )
            return
        evidence_path = Path(str(passed_state["evidence_path"]))
        original_evidence = legacy.load_json(evidence_path)
        laundered = copy.deepcopy(original_evidence)
        laundered["degraded_provenance"] = []
        laundered["evidence_sha256"] = verifier.document_hash(
            laundered, "evidence_sha256"
        )
        legacy.atomic_write_json(evidence_path, laundered)
        test.raises(
            "EVIDENCE degraded provenance cannot be laundered",
            lambda: verifier.verify_v2_evidence(
                contract, task_dir, allow_test_adapter=True
            ),
            "degraded_provenance is stale",
        )
        legacy.atomic_write_json(evidence_path, original_evidence)

        forged_summary = copy.deepcopy(original_evidence)
        forged_summary["findings"].append(
            {
                "review": "combined",
                "severity": "INFO",
                "file": "owned.txt",
                "evidence": "forged receipt-only finding",
                "required_fix": "none",
            }
        )
        forged_summary["evidence_sha256"] = verifier.document_hash(
            forged_summary, "evidence_sha256"
        )
        legacy.atomic_write_json(evidence_path, forged_summary)
        test.raises(
            "EVIDENCE rehashed top-level findings cannot diverge from review records",
            lambda: verifier.verify_v2_evidence(
                contract, task_dir, allow_test_adapter=True
            ),
            "findings differs from its bound review records",
        )
        legacy.atomic_write_json(evidence_path, original_evidence)

        forged_probe_summary = copy.deepcopy(original_evidence)
        forged_probe_summary["probe_requests"].append(
            {
                "review": "combined",
                "probe_id": "rust-lib",
                "rationale": "forged receipt-only probe request",
            }
        )
        forged_probe_summary["evidence_sha256"] = verifier.document_hash(
            forged_probe_summary, "evidence_sha256"
        )
        legacy.atomic_write_json(evidence_path, forged_probe_summary)
        test.raises(
            "EVIDENCE rehashed top-level probe requests cannot diverge from review records",
            lambda: verifier.verify_v2_evidence(
                contract, task_dir, allow_test_adapter=True
            ),
            "probe_requests differs from its bound review records",
        )
        legacy.atomic_write_json(evidence_path, original_evidence)

        original_review = original_evidence["reviews"][0]
        original_result_path = Path(str(original_review["result_path"]))
        original_review_result = legacy.load_json(original_result_path)
        for severity in ("MAJOR", "BLOCKER"):
            severe_result = copy.deepcopy(original_review_result)
            severe_result["findings"] = [
                {
                    "severity": severity,
                    "file": "docs/commit-recovery.md",
                    "evidence": "bound severe selftest finding",
                    "required_fix": "reject the v2 PASS",
                }
            ]
            legacy.atomic_write_json(original_result_path, severe_result)
            severe_evidence = copy.deepcopy(original_evidence)
            severe_evidence["reviews"][0]["result"] = severe_result
            severe_evidence["reviews"][0][
                "result_sha256"
            ] = legacy.sha256_file(original_result_path)
            outcomes = verifier.aggregate_review_outcomes(
                severe_evidence["reviews"]
            )
            severe_evidence.update(outcomes)
            severe_evidence["evidence_sha256"] = verifier.document_hash(
                severe_evidence, "evidence_sha256"
            )
            legacy.atomic_write_json(evidence_path, severe_evidence)
            test.raises(
                f"EVIDENCE bound {severity} review cannot verify PASS",
                lambda: verifier.verify_v2_evidence(
                    contract, task_dir, allow_test_adapter=True
                ),
                "unresolved MAJOR/BLOCKER",
            )

        gap_result = copy.deepcopy(original_review_result)
        gap_result["proof_gaps"] = [
            {
                "claim": "exact runtime proof",
                "evidence_missing": "runner artifact",
                "how_to_prove": "run an allowlisted probe",
            }
        ]
        legacy.atomic_write_json(original_result_path, gap_result)
        gap_evidence = copy.deepcopy(original_evidence)
        gap_evidence["reviews"][0]["result"] = gap_result
        gap_evidence["reviews"][0][
            "result_sha256"
        ] = legacy.sha256_file(original_result_path)
        gap_evidence.update(
            verifier.aggregate_review_outcomes(gap_evidence["reviews"])
        )
        gap_evidence["evidence_sha256"] = verifier.document_hash(
            gap_evidence, "evidence_sha256"
        )
        legacy.atomic_write_json(evidence_path, gap_evidence)
        test.raises(
            "EVIDENCE bound proof gap cannot verify PASS",
            lambda: verifier.verify_v2_evidence(
                contract, task_dir, allow_test_adapter=True
            ),
            "unresolved proof gaps",
        )

        probe_result = copy.deepcopy(original_review_result)
        probe_result["probe_requests"] = [
            {
                "probe_id": "rust-lib",
                "rationale": "bound empirical proof is still required",
            }
        ]
        legacy.atomic_write_json(original_result_path, probe_result)
        probe_evidence = copy.deepcopy(original_evidence)
        probe_evidence["reviews"][0]["result"] = probe_result
        probe_evidence["reviews"][0][
            "result_sha256"
        ] = legacy.sha256_file(original_result_path)
        probe_evidence.update(
            verifier.aggregate_review_outcomes(probe_evidence["reviews"])
        )
        probe_evidence["evidence_sha256"] = verifier.document_hash(
            probe_evidence, "evidence_sha256"
        )
        legacy.atomic_write_json(evidence_path, probe_evidence)
        test.raises(
            "EVIDENCE bound probe request cannot verify PASS",
            lambda: verifier.verify_v2_evidence(
                contract, task_dir, allow_test_adapter=True
            ),
            "unresolved proof gaps",
        )
        legacy.atomic_write_json(
            original_result_path, original_review_result
        )
        legacy.atomic_write_json(evidence_path, original_evidence)

        message = "selftest crash-resumable commit"
        child_code = (
            "import argparse,os,pathlib,sys;"
            f"sys.path.insert(0,{str(Path(__file__).resolve().parent)!r});"
            "import cli;"
            "os.chdir(sys.argv[1]);"
            "cli.cmd_v2_commit(argparse.Namespace("
            "task_id='commit-crash',message='selftest crash-resumable commit',"
            "_allow_test_adapter=True))"
        )
        killed = subprocess.run(
            [sys.executable, "-c", child_code, str(repo)],
            env={
                **os.environ,
                "MURMUR_HARNESS_SELFTEST": "1",
                "MURMUR_HARNESS_SELFTEST_KILL_AFTER_GIT_COMMIT": "1",
            },
            capture_output=True,
            check=False,
        )
        test.equal(
            "COMMIT child receives real SIGKILL after git commit",
            killed.returncode,
            -signal.SIGKILL,
        )
        test.equal(
            "COMMIT crash leaves exactly one child commit",
            _git(repo, "rev-list", "--count", f"{base}..HEAD"),
            "1",
        )
        committed_head = _git(repo, "rev-parse", "HEAD")
        test.equal(
            "COMMIT crash preserves PASSED state until receipt finalization",
            harness_cli.load_v2_state(task_dir)["status"],
            "PASSED",
        )
        test.true(
            "COMMIT durable intent precedes crash",
            (task_dir / "commit-intent.json").is_file(),
        )
        test.true(
            "COMMIT receipt is absent at injected crash",
            not (task_dir / "commit.json").exists(),
        )
        previous = Path.cwd()
        try:
            os.chdir(repo)
            with contextlib.redirect_stdout(io.StringIO()):
                resumed = harness_cli.cmd_v2_commit(
                    argparse.Namespace(
                        task_id=task_id,
                        message=message,
                        _allow_test_adapter=True,
                    )
                )
        finally:
            os.chdir(previous)
        test.equal("COMMIT resume command succeeds", resumed, 0)
        test.equal(
            "COMMIT resume finalizes COMMITTED state",
            harness_cli.load_v2_state(task_dir)["status"],
            "COMMITTED",
        )
        test.equal(
            "COMMIT resume does not create a second commit",
            _git(repo, "rev-list", "--count", f"{base}..HEAD"),
            "1",
        )
        test.equal(
            "COMMIT resume preserves the exact committed HEAD",
            _git(repo, "rev-parse", "HEAD"),
            committed_head,
        )
        receipt = harness_cli.verify_v2_committed(
            contract, task_dir, allow_test_adapter=True
        )
        test.equal(
            "COMMIT finalized receipt binds exact post-commit HEAD",
            receipt["commit_sha"],
            committed_head,
        )
        test.raises(
            "COMMIT idempotent resume rejects a forged receipt",
            lambda: harness_cli._validate_v2_commit_head(
                repo,
                {
                    "parent_sha": receipt["parent_sha"],
                    "tree_sha": receipt["tree_sha"],
                    "diff_sha256": receipt["diff_sha256"],
                },
                {
                    "parent_sha": receipt["parent_sha"],
                    "tree_sha": receipt["tree_sha"],
                    "diff_sha256": receipt["diff_sha256"],
                    "message": "different",
                    "message_sha256": hashlib.sha256(b"different").hexdigest(),
                },
                {"name": "QueaT", "email": "kgm004a@gmail.com"},
            ),
            "message differs",
        )

        # A trunk update may be merged after the immutable task commit without
        # invalidating its receipt.  Only Git's exact automatic merge tree is
        # admissible; any later branch-authored commit must be re-verified.
        (primary / "trunk.txt").write_text("trunk moved\n", encoding="utf-8")
        config_path = primary / ".agents" / "harness" / "config.json"
        drifted_config = json.loads(config_path.read_text(encoding="utf-8"))
        drifted_config["default_base"] = "origin/future-trunk"
        config_path.write_text(
            json.dumps(drifted_config, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
        for schema_name in (
            "v2-task",
            "v2-plan",
            "v2-review",
            "v2-evidence",
            "v2-commit",
        ):
            schema_path = (
                primary
                / ".agents"
                / "harness"
                / "schemas"
                / f"{schema_name}.schema.json"
            )
            drifted_schema = json.loads(
                schema_path.read_text(encoding="utf-8")
            )
            required = list(drifted_schema.get("required", []))
            required.append("future_required")
            drifted_schema["required"] = required
            schema_path.write_text(
                json.dumps(drifted_schema, indent=2, sort_keys=True) + "\n",
                encoding="utf-8",
            )
        policy_path = (
            primary
            / ".agents"
            / "harness"
            / "prompts"
            / "combined-reviewer.md"
        )
        policy_path.write_text(
            policy_path.read_text(encoding="utf-8")
            + "\nCURRENT POLICY DRIFT MUST NOT REVALIDATE OLD EVIDENCE.\n",
            encoding="utf-8",
        )
        _git(primary, "add", ".")
        _git(primary, "commit", "-q", "-m", "move trunk and verifier policy")
        trunk_tip = _git(primary, "rev-parse", "HEAD")
        _git(primary, "update-ref", "refs/remotes/origin/murmur", trunk_tip)
        _git(
            repo,
            "-c",
            "user.name=QueaT",
            "-c",
            "user.email=kgm004a@gmail.com",
            "merge",
            "--no-ff",
            "-q",
            "-m",
            "merge origin/murmur",
            "origin/murmur",
        )
        catchup_head = _git(repo, "rev-parse", "HEAD")
        catchup_receipt = harness_cli.verify_v2_committed(
            contract, task_dir, allow_test_adapter=True
        )
        test.equal(
            "COMMIT clean trunk catch-up preserves immutable receipt",
            catchup_receipt["commit_sha"],
            committed_head,
        )

        fixture_python = str(repo / ".agents" / "harness")
        fresh_schema_check = subprocess.run(
            [
                sys.executable,
                "-B",
                "-c",
                (
                    "import pathlib,sys;"
                    f"sys.path.insert(0,{fixture_python!r});"
                    "import task_runner as legacy,verifier;"
                    f"doc=legacy.load_json(pathlib.Path({str(task_dir / 'task.json')!r}));"
                    "verifier.validate_hashed_document("
                    "doc,'v2-task','contract_sha256','fresh task')"
                ),
            ],
            cwd=str(repo),
            text=True,
            capture_output=True,
            check=False,
        )
        test.true(
            "COMMIT fresh task validation still enforces the current stricter schema",
            fresh_schema_check.returncode != 0
            and "future_required" in fresh_schema_check.stderr,
        )
        pinned_verify = subprocess.run(
            [
                sys.executable,
                "-B",
                "-c",
                (
                    "import pathlib,sys;"
                    f"sys.path.insert(0,{fixture_python!r});"
                    "import cli,task_runner as legacy;"
                    f"task=pathlib.Path({str(task_dir)!r});"
                    "contract=legacy.load_json(task/'task.json');"
                    "receipt=cli.verify_v2_committed("
                    "contract,task,allow_test_adapter=True);"
                    "print(receipt['commit_sha'])"
                ),
            ],
            cwd=str(repo),
            text=True,
            capture_output=True,
            check=False,
        )
        test.equal(
            "COMMIT catch-up verifies with attested schema config and policy",
            (pinned_verify.returncode, pinned_verify.stdout.strip()),
            (0, committed_head),
        )
        (repo / "unverified.txt").write_text("branch content\n", encoding="utf-8")
        _git(repo, "add", "unverified.txt")
        _git(repo, "commit", "-q", "-m", "unverified branch content")
        test.raises(
            "COMMIT branch-authored descendant requires fresh verification",
            lambda: harness_cli.verify_v2_committed(
                contract, task_dir, allow_test_adapter=True
            ),
            "non-merge commit",
        )
        _git(repo, "reset", "--hard", "-q", catchup_head)
        clean_result = subprocess.run(
            [
                sys.executable,
                "-B",
                "-c",
                (
                    "import argparse,sys;"
                    f"sys.path.insert(0,{fixture_python!r});"
                    "import cli;"
                    f"sys.path.insert(0,{str(repo)!r});"
                    f"__import__('os').chdir({str(repo)!r});"
                    "cli.cmd_clean(argparse.Namespace("
                    "task_id='commit-crash',abandon=False,"
                    "_allow_test_adapter=True))"
                ),
            ],
            cwd=str(repo),
            text=True,
            capture_output=True,
            check=False,
        )
        test.equal(
            "CLEAN closes old committed evidence after verifier-policy catch-up",
            clean_result.returncode,
            0,
        )
        test.equal(
            "CLEAN records the old committed task as CLOSED",
            harness_cli.load_v2_state(task_dir)["status"],
            "CLOSED",
        )
        test.true(
            "CLEAN removes the linked committed worktree",
            not repo.exists(),
        )


def _v1_contract(
    *,
    task_id: str,
    repo: Path,
    common: Path,
    worktree: Path,
    base: str,
    branch: str,
) -> Dict[str, Any]:
    contract: Dict[str, Any] = {
        "schema_version": 1,
        "task_id": task_id,
        "description": "v1 import selftest",
        "kind": "harness",
        "base_sha": base,
        "contract_sha256": "",
        "instructions_sha256": "0" * 64,
        "dependency_revisions": {},
        "repo_realpath": str(repo.resolve()),
        "git_common_dir": str(common.resolve()),
        "worktree_path": str(worktree.resolve()),
        "branch": branch,
        "owned_paths": ["owned.txt"],
        "risk_flags": [],
        "writer": "fake",
        "reviewer": "fake",
        "max_repair_rounds": 2,
        "checks": [],
        "final_checks": [],
        "expected_change": True,
        "created_at": legacy.utc_now(),
    }
    contract["contract_sha256"] = legacy.contract_hash(contract)
    legacy.validate_schema(
        contract, legacy.load_schema("task"), label="v1 import selftest task"
    )
    return contract


def import_cases(test: Tests) -> None:
    with tempfile.TemporaryDirectory(prefix="murmur-v2-import-") as raw:
        root = Path(raw)
        repo = root / "repo"
        base = _init_repo(repo)
        common = Path(
            _git(repo, "rev-parse", "--path-format=absolute", "--git-common-dir")
        )
        task_id = "legacy-ghost"
        branch = f"agent/{task_id}"
        worktree = root / "task" / "meetnotes"
        worktree.parent.mkdir()
        _git(repo, "worktree", "add", "-q", "-b", branch, str(worktree), base)
        (worktree / "owned.txt").write_text("base\nlegacy bytes\n", encoding="utf-8")
        _git(worktree, "add", "owned.txt")
        tree = _git(worktree, "write-tree")
        snapshot = _git(
            repo,
            "-c",
            "user.name=QueaT",
            "-c",
            "user.email=kgm004a@gmail.com",
            "commit-tree",
            tree,
            "-p",
            base,
            "-m",
            "legacy archive",
        )
        _git(worktree, "reset", "--quiet", "HEAD", "--", ".")
        source_dir = common / "agent-harness" / "tasks" / task_id
        source_dir.mkdir(parents=True)
        contract = _v1_contract(
            task_id=task_id,
            repo=repo,
            common=common,
            worktree=worktree,
            base=base,
            branch=branch,
        )
        legacy.atomic_write_json(source_dir / "task.json", contract)
        legacy.append_jsonl(
            source_dir / "events.jsonl",
            {
                "at": legacy.utc_now(),
                "event": "state",
                "status": "INITIALIZED",
                "round": 0,
                "phase": "init",
            },
        )
        archive_ref = legacy.task_archive_ref(contract)
        _git(repo, "update-ref", archive_ref, snapshot)
        legacy.atomic_write_json(
            source_dir / "archive.json",
            {
                "schema_version": 1,
                "task_id": task_id,
                "archive_ref": archive_ref,
                "snapshot_sha": snapshot,
                "original_head_sha": base,
                "tree_sha": tree,
                "created_at": legacy.utc_now(),
            },
        )
        _git(repo, "worktree", "remove", "--force", str(worktree))
        source_before = harness_cli._directory_digest(source_dir)
        args = argparse.Namespace(
            task_id=task_id,
            invalidate_pass=False,
            claim=[],
            reviewer="fake",
        )
        previous_cwd = Path.cwd()
        previous_selftest = os.environ.get("MURMUR_HARNESS_SELFTEST")
        os.environ["MURMUR_HARNESS_SELFTEST"] = "1"
        try:
            os.chdir(repo)
            with contextlib.redirect_stdout(io.StringIO()):
                harness_cli.cmd_import_v1(args)
            target_dir = harness_cli.v2_task_dir(common, task_id)
            imported = legacy.load_json(target_dir / "imports" / "v1.json")
            test.equal(
                "IMPORT ghost state has no fabricated state hash",
                imported["source_state_sha256"],
                None,
            )
            test.equal(
                "IMPORT reconstructs archived missing worktree",
                harness_cli.load_v2_state(target_dir)["status"],
                "OPEN",
            )
            test.equal(
                "IMPORT reconstructed exact archived bytes",
                (worktree / "owned.txt").read_text(encoding="utf-8"),
                "base\nlegacy bytes\n",
            )
            test.equal(
                "IMPORT v1 task directory is byte-identical",
                harness_cli._directory_digest(source_dir),
                source_before,
            )
            with contextlib.redirect_stdout(io.StringIO()):
                harness_cli.cmd_import_v1(args)
            test.equal(
                "IMPORT repeat is idempotent and source remains byte-identical",
                harness_cli._directory_digest(source_dir),
                source_before,
            )

            missing_id = "legacy-missing"
            missing_worktree = root / "missing" / "meetnotes"
            missing_branch = f"agent/{missing_id}"
            missing_dir = common / "agent-harness" / "tasks" / missing_id
            missing_dir.mkdir(parents=True)
            missing_contract = _v1_contract(
                task_id=missing_id,
                repo=repo,
                common=common,
                worktree=missing_worktree,
                base=base,
                branch=missing_branch,
            )
            legacy.atomic_write_json(missing_dir / "task.json", missing_contract)
            legacy.append_jsonl(
                missing_dir / "events.jsonl",
                {
                    "at": legacy.utc_now(),
                    "event": "state",
                    "status": "BLOCKED",
                    "round": 1,
                    "phase": "writer",
                },
            )
            missing_before = harness_cli._directory_digest(missing_dir)
            with contextlib.redirect_stdout(io.StringIO()):
                harness_cli.cmd_import_v1(
                    argparse.Namespace(
                        task_id=missing_id,
                        invalidate_pass=False,
                        claim=[],
                        reviewer="fake",
                    )
                )
            missing_target = harness_cli.v2_task_dir(common, missing_id)
            test.equal(
                "IMPORT irrecoverable missing worktree is history-only",
                harness_cli.load_v2_state(missing_target)["status"],
                "NEEDS_EVIDENCE",
            )
            test.equal(
                "IMPORT history-only path preserves all v1 bytes",
                harness_cli._directory_digest(missing_dir),
                missing_before,
            )
        finally:
            os.chdir(previous_cwd)
            if previous_selftest is None:
                os.environ.pop("MURMUR_HARNESS_SELFTEST", None)
            else:
                os.environ["MURMUR_HARNESS_SELFTEST"] = previous_selftest


def plan_and_probe_cases(test: Tests) -> None:
    base = legacy.git(ROOT, "rev-parse", "HEAD")
    tree = legacy.git(ROOT, "rev-parse", "HEAD^{tree}")
    contract = {
        "task_id": "plan-selftest",
        "contract_sha256": "a" * 64,
        "claims": [],
        "reviewer": "claude",
        "created_at": "2026-01-01T00:00:00Z",
    }
    angular, _bundle = verifier.build_plan(
        contract,
        ROOT,
        ["src/app/features/detail/detail.ts"],
        b"angular diff",
        tree,
        legacy.load_config(),
    )
    angular_again, _bundle = verifier.build_plan(
        contract,
        ROOT,
        ["src/app/features/detail/detail.ts"],
        b"angular diff",
        tree,
        legacy.load_config(),
    )
    test.equal(
        "PLAN identical diff has stable plan hash",
        angular_again["plan_sha256"],
        angular["plan_sha256"],
    )
    test.equal(
        "PLAN frontend-only diff creates no sibling server",
        angular["server_required"],
        False,
    )
    rust, _bundle = verifier.build_plan(
        contract,
        ROOT,
        ["src-tauri/src/lib.rs"],
        b"rust diff",
        tree,
        legacy.load_config(),
    )
    test.equal(
        "PLAN Rust diff lazily requires sibling server",
        rust["server_required"],
        True,
    )
    changed, _bundle = verifier.build_plan(
        contract,
        ROOT,
        ["src/app/features/detail/detail.ts"],
        b"changed angular diff",
        tree,
        legacy.load_config(),
    )
    test.true(
        "PLAN changed diff invalidates attempt binding",
        verifier.attempt_id(changed) != verifier.attempt_id(angular),
    )
    test.equal("PLAN parent remains current immutable base", angular["base_sha"], base)

    test.raises(
        "PROBE broker rejects arbitrary shell ids",
        lambda: verifier.canonical_check("sh -c arbitrary", legacy.load_config()),
        "not allowlisted",
    )
    request = {
        "verdict": "PASS",
        "findings": [],
        "proof_gaps": [],
        "probe_requests": [
            {"probe_id": "rust-lib", "rationale": "need runner proof"}
        ],
    }
    test.equal(
        "PROBE typed request prevents immediate PASS",
        verifier.review_result_state(request),
        "NEEDS_EVIDENCE",
    )
    empty_hash = verifier.probe_evidence_hash([])
    probe_record = {
        "id": "rust-lib",
        "diff_sha256": angular["diff_sha256"],
        "plan_sha256": angular["plan_sha256"],
        "protocol_sha256": angular["protocol_sha256"],
        "evidence": {"passed": True},
    }
    test.true(
        "PROBE evidence changes the reviewer round binding",
        verifier.probe_evidence_hash([probe_record]) != empty_hash,
    )


def clean_cases(test: Tests) -> None:
    with tempfile.TemporaryDirectory(prefix="murmur-v2-clean-") as raw:
        root = Path(raw)
        repo = root / "repo"
        base = _init_repo(repo)
        common = Path(
            _git(repo, "rev-parse", "--path-format=absolute", "--git-common-dir")
        )
        worktree = root / "task" / "meetnotes"
        worktree.parent.mkdir()
        branch = "agent/v2/clean-selftest"
        _git(repo, "worktree", "add", "-q", "-b", branch, str(worktree), base)
        (worktree / "owned.txt").write_text("base\ndirty tracked\n", encoding="utf-8")
        (worktree / "untracked.txt").write_text("dirty untracked\n", encoding="utf-8")
        task_dir = harness_cli.v2_task_dir(common, "clean-selftest")
        task_dir.mkdir(parents=True)
        contract: Dict[str, Any] = {
            "schema_version": 2,
            "task_id": "clean-selftest",
            "description": "archive every dirty byte",
            "kind": "harness",
            "base_sha": base,
            "contract_sha256": "",
            "repo_realpath": str(repo.resolve()),
            "git_common_dir": str(common.resolve()),
            "worktree_path": str(worktree.resolve()),
            "branch": branch,
            "owned_paths": ["owned.txt", "untracked.txt"],
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
        legacy.atomic_write_json(
            task_dir / "runtime.json",
            {
                "schema_version": 2,
                "task_root": str(worktree.parent),
                "shared_node_modules": None,
                "server_worktree": None,
                "server_source": str(root / "murmur-server"),
                "server_revision": None,
            },
        )
        harness_cli.set_v2_state(task_dir, "OPEN", phase="open")
        previous = Path.cwd()
        try:
            os.chdir(repo)
            with contextlib.redirect_stdout(io.StringIO()):
                harness_cli.cmd_clean(
                    argparse.Namespace(task_id="clean-selftest", abandon=True)
                )
        finally:
            os.chdir(previous)
        state = harness_cli.load_v2_state(task_dir)
        archive_ref = state["archive_ref"]
        test.equal("CLEAN dirty task becomes ABANDONED", state["status"], "ABANDONED")
        test.true("CLEAN removes only task worktree", not worktree.exists())
        test.equal(
            "CLEAN archive preserves tracked dirty bytes",
            _git(repo, "show", f"{archive_ref}:owned.txt"),
            "base\ndirty tracked",
        )
        test.equal(
            "CLEAN archive preserves untracked dirty bytes",
            _git(repo, "show", f"{archive_ref}:untracked.txt"),
            "dirty untracked",
        )


def main() -> int:
    test = Tests()
    open_branch_ownership_cases(test)
    standalone_driver_open_cases(test)
    profile_cases(test)
    reviewer_tool_guard_cases(test)
    verdict_cases(test)
    retry_cases(test)
    guardian_and_artifact_cases(test)
    readonly_review_wall_timeout_cases(test)
    state_and_lock_cases(test)
    standalone_driver_lane_cases(test)
    checkpoint_cases(test)
    protocol_and_runtime_cases(test)
    commit_recovery_cases(test)
    import_cases(test)
    plan_and_probe_cases(test)
    clean_cases(test)
    if test.failures:
        print("v2 selftest: FAIL")
        for failure in test.failures:
            print(f"  - {failure}")
        return 1
    print(f"v2 selftest: PASS ({test.count} assertions)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
