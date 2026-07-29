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
from typing import Any, Dict, List, Mapping, Optional, Sequence

import cli as harness_cli
import runtime
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


def _seed_executing_protocol(repo: Path) -> str:
    """Commit the exact protocol imported by this selftest into a fixture."""

    for relative in verifier.protocol_relative_paths(ROOT):
        source = ROOT / relative
        target = repo / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        target.chmod(source.stat().st_mode & 0o777)
    _git(repo, "add", "--", *verifier.protocol_relative_paths(ROOT))
    _git(repo, "commit", "-q", "-m", "seed current harness protocol")
    return _git(repo, "rev-parse", "HEAD")


def _standalone_driver(
    root: Path,
    *,
    name: str = ".murmur-agent-driver",
    canonical_origin: bool = True,
    detached: bool = True,
    with_server_dependency: bool = False,
) -> tuple[Path, Path, str]:
    primary = root / "meetnotes"
    base = _init_repo(primary)
    if with_server_dependency:
        server = root / "murmur-server"
        _init_repo(server)
        protocol = server / "crates" / "murmur-protocol"
        (protocol / "src").mkdir(parents=True)
        (protocol / "Cargo.toml").write_text(
            "[package]\n"
            'name = "murmur-protocol"\n'
            'version = "0.0.0"\n'
            'edition = "2021"\n'
            "\n[lib]\n"
            'path = "src/lib.rs"\n',
            encoding="utf-8",
        )
        (protocol / "src" / "lib.rs").write_text(
            "pub fn pinned() {}\n",
            encoding="utf-8",
        )
        _git(server, "add", "crates/murmur-protocol")
        _git(server, "commit", "-q", "-m", "add protocol crate")
        server_revision = _git(server, "rev-parse", "HEAD")

        (primary / "src-tauri").mkdir()
        (primary / "src-tauri" / "Cargo.toml").write_text(
            "[package]\n"
            'name = "murmur-open-selftest"\n'
            'version = "0.0.0"\n'
            'edition = "2021"\n'
            "\n[dependencies]\n"
            'murmur-protocol = { path = "../../murmur-server/crates/murmur-protocol" }\n',
            encoding="utf-8",
        )
        (primary / ".murmur-server-revision").write_text(
            f"{server_revision}\n",
            encoding="utf-8",
        )
        _git(
            primary,
            "add",
            "src-tauri/Cargo.toml",
            ".murmur-server-revision",
        )
        _git(primary, "commit", "-q", "-m", "pin server dependency")
        base = _git(primary, "rev-parse", "HEAD")
    base = _seed_executing_protocol(primary)
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


def _invoke_open_json(
    driver: Path, args: argparse.Namespace
) -> tuple[int, Mapping[str, Any]]:
    previous = Path.cwd()
    output = io.StringIO()
    try:
        os.chdir(driver)
        with contextlib.redirect_stdout(output):
            result = harness_cli.cmd_open(args)
    finally:
        os.chdir(previous)
    return result, json.loads(output.getvalue())


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


def _tree_leaf_payloads(root: Path) -> Dict[str, bytes]:
    payloads: Dict[str, bytes] = {}
    for path in sorted(root.rglob("*")):
        relative = path.relative_to(root).as_posix()
        if path.is_symlink():
            payloads[relative] = os.readlink(path).encode(
                "utf-8", "surrogateescape"
            )
        elif path.is_file():
            payloads[relative] = path.read_bytes()
    return payloads


def _unique_git_control_payloads(
    repository: Path,
    reference: str,
    object_id: str,
) -> Dict[str, bytes]:
    git_dir = Path(
        _git(
            repository,
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
        )
    )
    relative_paths = (
        reference,
        f"logs/{reference}",
        f"objects/{object_id[:2]}/{object_id[2:]}",
    )
    payloads: Dict[str, bytes] = {}
    for relative in relative_paths:
        path = git_dir / relative
        if not path.is_file() or path.is_symlink():
            raise AssertionError(
                f"expected loose Git-control payload is absent: {relative}"
            )
        payloads[relative] = path.read_bytes()
    return payloads


def _archived_git_control_payloads(
    task_dir: Path,
    archive_role: str,
) -> Dict[str, bytes]:
    manifest = runtime.load_json(
        task_dir / f"clean-git-control-{archive_role}.json"
    )
    completion = runtime.load_json(
        task_dir
        / f"clean-git-control-{archive_role}-complete.json"
    )
    if (
        manifest.get("schema_version") != 2
        or manifest.get("archive_role") != archive_role
        or manifest.get("manifest_sha256")
        != verifier.document_hash(manifest, "manifest_sha256")
        or completion.get("schema_version") != 2
        or completion.get("archive_role") != archive_role
        or completion.get("manifest_sha256")
        != manifest["manifest_sha256"]
        or completion.get("complete_sha256")
        != verifier.document_hash(completion, "complete_sha256")
    ):
        raise AssertionError(
            f"Git-control archive binding mismatch: {archive_role}"
        )
    payloads: Dict[str, bytes] = {}
    for record in completion["payloads"]:
        if record["source"] != "control":
            continue
        payload_path = task_dir / str(record["payload"])
        payload = payload_path.read_bytes()
        if runtime.sha256_bytes(payload) != record["sha256"]:
            raise AssertionError(
                f"Git-control archive payload hash mismatch: {archive_role}"
            )
        payloads[str(record["path"])] = payload
    return payloads


def _git_control_payloads_recoverable(
    task_dir: Path,
    archive_roles: Sequence[str],
    expected: Mapping[str, bytes],
) -> bool:
    return all(
        all(
            archived.get(relative) == payload
            for relative, payload in expected.items()
        )
        for archived in (
            _archived_git_control_payloads(task_dir, role)
            for role in archive_roles
        )
    )


def _git_object_available(repository: Path, object_id: str) -> bool:
    completed = subprocess.run(
        [
            "git",
            "--no-replace-objects",
            "cat-file",
            "-e",
            f"{object_id}^{{object}}",
        ],
        cwd=str(repository),
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    return completed.returncode == 0


def _restore_archived_git_repository(
    task_dir: Path,
    archive_role: str,
    destination: Path,
    *,
    linked_admin: bool = False,
) -> set[str]:
    manifest = runtime.load_json(
        task_dir / f"clean-git-control-{archive_role}.json"
    )
    completion = runtime.load_json(
        task_dir
        / f"clean-git-control-{archive_role}-complete.json"
    )
    # Reuse the strict schema/hash/payload validation before restoring bytes.
    _archived_git_control_payloads(task_dir, archive_role)
    records = {
        (str(record["source"]), str(record["path"])): record
        for record in completion["payloads"]
    }
    destination.mkdir()
    _git(destination, "init", "-q", "--object-format=sha1")
    git_dir = destination / ".git"
    for entry in manifest["entries"]:
        relative = str(entry["path"])
        if linked_admin and relative in {"commondir", "gitdir"}:
            continue
        target = git_dir / relative
        if entry["kind"] == "directory":
            target.mkdir(parents=True, exist_ok=True)
            continue
        record = records[("control", relative)]
        payload = (task_dir / str(record["payload"])).read_bytes()
        target.parent.mkdir(parents=True, exist_ok=True)
        if entry["kind"] == "regular":
            target.write_bytes(payload)
            target.chmod(int(entry["mode"]))
        elif entry["kind"] == "symlink":
            if target.exists() or target.is_symlink():
                target.unlink()
            os.symlink(
                payload.decode("utf-8", "surrogateescape"),
                target,
            )
        else:
            raise AssertionError(
                f"unsupported archived Git-control entry: {relative}"
            )

    pack_record = records[("object-pack", "objects.pack")]
    index_record = records[("object-pack", "objects.idx")]
    pack = (task_dir / str(pack_record["payload"])).read_bytes()
    index = (task_dir / str(index_record["payload"])).read_bytes()
    if (
        len(pack) < 20
        or runtime.sha256_bytes(pack) != pack_record["sha256"]
        or runtime.sha256_bytes(index) != index_record["sha256"]
    ):
        raise AssertionError(
            f"archived Git-control object pack is malformed: {archive_role}"
        )
    pack_name = f"pack-{pack[-20:].hex()}"
    pack_dir = git_dir / "objects" / "pack"
    pack_dir.mkdir(parents=True, exist_ok=True)
    restored_pack = pack_dir / f"{pack_name}.pack"
    restored_index = pack_dir / f"{pack_name}.idx"
    restored_pack.write_bytes(pack)
    restored_index.write_bytes(index)
    verified = _git(
        destination,
        "verify-pack",
        "-v",
        str(restored_index),
    )
    return {
        line.split()[0]
        for line in verified.splitlines()
        if line and runtime.SHA1_RE.fullmatch(line.split()[0])
    }


def _open_args(task_id: str, base: str, branch: str) -> argparse.Namespace:
    return argparse.Namespace(
        task_id=task_id,
        prompt="open branch ownership selftest",
        prompt_file=None,
        owned=["owned.txt"],
        claim=[],
        reviewer=None,
        allow_same_vendor_high_risk=False,
        base=base,
        branch=branch,
        kind="harness",
        expected_change=True,
    )


def open_protocol_base_cases(test: Tests) -> None:
    with tempfile.TemporaryDirectory(prefix="murmur-v2-open-protocol-") as raw:
        root = Path(raw)
        _primary, driver, current_base = _standalone_driver(root)
        common = (driver / ".git").resolve()

        missing_base = _git(
            driver,
            "rev-list",
            "--max-parents=0",
            current_base,
        )
        missing_task = "missing-base-protocol"
        missing_branch = f"agent/v2/{missing_task}"
        test.raises(
            "OPEN rejects a base missing the executing protocol before mutation",
            lambda: _invoke_open(
                driver,
                _open_args(missing_task, missing_base, missing_branch),
            ),
            "missing required protocol file",
        )
        test.true(
            "OPEN missing-protocol refusal creates no task bytes",
            not harness_cli.v2_task_dir(common, missing_task).exists()
            and not (
                root
                / ".murmur-agent-tasks"
                / "v2"
                / missing_task
            ).exists()
            and harness_cli._local_branch_oid(driver, missing_branch) is None,
        )

        extra_module = driver / ".agents" / "harness" / "json.py"
        extra_module.write_text(
            "raise RuntimeError('must never shadow the standard library')\n",
            encoding="utf-8",
        )
        _git(driver, "add", ".agents/harness/json.py")
        _git(driver, "commit", "-q", "-m", "add shadowing protocol module")
        shadowed_base = _git(driver, "rev-parse", "HEAD")
        shadowed_task = "shadowed-base-protocol"
        shadowed_branch = f"agent/v2/{shadowed_task}"
        test.raises(
            "OPEN rejects an extra base-side executable protocol module",
            lambda: _invoke_open(
                driver,
                _open_args(shadowed_task, shadowed_base, shadowed_branch),
            ),
            "base protocol differs",
        )
        test.true(
            "OPEN extra-module refusal creates no task bytes",
            not harness_cli.v2_task_dir(common, shadowed_task).exists()
            and not (
                root
                / ".murmur-agent-tasks"
                / "v2"
                / shadowed_task
            ).exists()
            and harness_cli._local_branch_oid(driver, shadowed_branch) is None,
        )
        _git(driver, "rm", "-q", ".agents/harness/json.py")
        _git(driver, "commit", "-q", "-m", "restore protocol path set")

        package = driver / ".agents" / "harness" / "json" / "__init__.py"
        package.parent.mkdir()
        package.write_text(
            "raise RuntimeError('package must never shadow stdlib json')\n",
            encoding="utf-8",
        )
        _git(driver, "add", ".agents/harness/json/__init__.py")
        _git(driver, "commit", "-q", "-m", "add shadowing protocol package")
        package_base = _git(driver, "rev-parse", "HEAD")
        package_task = "package-shadowed-base-protocol"
        package_branch = f"agent/v2/{package_task}"
        test.raises(
            "OPEN rejects an extra base-side protocol package",
            lambda: _invoke_open(
                driver,
                _open_args(package_task, package_base, package_branch),
            ),
            "base protocol differs",
        )
        test.true(
            "OPEN extra-package refusal creates no task bytes",
            not harness_cli.v2_task_dir(common, package_task).exists()
            and not (
                root
                / ".murmur-agent-tasks"
                / "v2"
                / package_task
            ).exists()
            and harness_cli._local_branch_oid(driver, package_branch) is None,
        )
        _git(driver, "rm", "-q", "-r", ".agents/harness/json")
        _git(driver, "commit", "-q", "-m", "restore protocol package set")

        wrapper = driver / "scripts" / "agent-harness"
        wrapper.chmod(0o644)
        _git(driver, "add", "scripts/agent-harness")
        _git(driver, "commit", "-q", "-m", "drop harness executable bit")
        mode_base = _git(driver, "rev-parse", "HEAD")
        mode_task = "mode-drifted-base-protocol"
        mode_branch = f"agent/v2/{mode_task}"
        test.raises(
            "OPEN rejects executable mode drift in the selected base",
            lambda: _invoke_open(
                driver,
                _open_args(mode_task, mode_base, mode_branch),
            ),
            "base protocol differs",
        )
        test.true(
            "OPEN mode-drift refusal creates no task bytes",
            not harness_cli.v2_task_dir(common, mode_task).exists()
            and not (
                root
                / ".murmur-agent-tasks"
                / "v2"
                / mode_task
            ).exists()
            and harness_cli._local_branch_oid(driver, mode_branch) is None,
        )
        wrapper.chmod(0o755)
        _git(driver, "add", "scripts/agent-harness")
        _git(driver, "commit", "-q", "-m", "restore harness executable bit")

        config = driver / ".agents" / "harness" / "config.json"
        exact_blob = _git(
            driver,
            "rev-parse",
            f"{current_base}:.agents/harness/config.json",
        )
        config.write_bytes(config.read_bytes() + b"\n")
        _git(driver, "add", ".agents/harness/config.json")
        _git(driver, "commit", "-q", "-m", "drift protocol fixture")
        drifted_base = _git(driver, "rev-parse", "HEAD")
        drifted_blob = _git(
            driver,
            "rev-parse",
            f"{drifted_base}:.agents/harness/config.json",
        )
        _git(driver, "replace", drifted_blob, exact_blob)
        test.true(
            "OPEN immutable base bundle ignores a masking Git replacement",
            verifier.protocol_bundle_at_commit(driver, drifted_base)
            != verifier.protocol_bundle(verifier.SOURCE_ROOT),
        )
        replaced_task = "replaced-base-protocol"
        replaced_branch = f"agent/v2/{replaced_task}"
        test.raises(
            "OPEN rejects Git replacement refs before mutation",
            lambda: _invoke_open(
                driver,
                _open_args(replaced_task, drifted_base, replaced_branch),
            ),
            "forbids Git replacement refs",
        )
        test.true(
            "OPEN replacement-ref refusal creates no task bytes",
            not harness_cli.v2_task_dir(common, replaced_task).exists()
            and not (
                root
                / ".murmur-agent-tasks"
                / "v2"
                / replaced_task
            ).exists()
            and harness_cli._local_branch_oid(driver, replaced_branch) is None,
        )
        _git(driver, "replace", "-d", drifted_blob)
        drifted_task = "drifted-base-protocol"
        drifted_branch = f"agent/v2/{drifted_task}"
        test.raises(
            "OPEN rejects a drifted base protocol before mutation",
            lambda: _invoke_open(
                driver,
                _open_args(drifted_task, drifted_base, drifted_branch),
            ),
            "base protocol differs",
        )
        test.true(
            "OPEN drifted-protocol refusal creates no task bytes",
            not harness_cli.v2_task_dir(common, drifted_task).exists()
            and not (
                root
                / ".murmur-agent-tasks"
                / "v2"
                / drifted_task
            ).exists()
            and harness_cli._local_branch_oid(driver, drifted_branch) is None,
        )

        config.write_bytes(
            (verifier.SOURCE_ROOT / ".agents/harness/config.json").read_bytes()
        )
        attributes = driver / ".gitattributes"
        attributes.write_text(
            ".agents/harness/config.json filter=corrupt\n",
            encoding="utf-8",
        )
        _git(driver, "add", ".agents/harness/config.json", ".gitattributes")
        _git(driver, "commit", "-q", "-m", "add checkout smudge fixture")
        smudge_base = _git(driver, "rev-parse", "HEAD")
        _git(driver, "config", "filter.corrupt.clean", "cat")
        _git(driver, "config", "filter.corrupt.smudge", "sed '1s/$/ /'")
        test.equal(
            "OPEN smudge fixture keeps the raw base protocol exact",
            verifier.protocol_bundle_at_commit(driver, smudge_base),
            verifier.protocol_bundle(verifier.SOURCE_ROOT),
        )
        smudge_task = "smudged-checkout-protocol"
        smudge_branch = f"agent/v2/{smudge_task}"
        test.raises(
            "OPEN rejects checkout bytes changed by a smudge filter",
            lambda: _invoke_open(
                driver,
                _open_args(smudge_task, smudge_base, smudge_branch),
            ),
            "raw checkout bytes differ",
        )
        test.true(
            "OPEN smudged-checkout refusal rolls back all task mutations",
            not harness_cli.v2_task_dir(common, smudge_task).exists()
            and not (
                root
                / ".murmur-agent-tasks"
                / "v2"
                / smudge_task
            ).exists()
            and harness_cli._local_branch_oid(driver, smudge_branch) is None,
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
        original_run_capture = runtime.run_capture

        def fail_worktree_add(
            argv: Sequence[str],
            cwd: Path | None = None,
            *,
            check: bool = True,
            env: Mapping[str, str] | None = None,
        ) -> subprocess.CompletedProcess:
            values = list(argv)
            if "worktree" in values and values[
                values.index("worktree") : values.index("worktree") + 2
            ] == ["worktree", "add"]:
                raise runtime.HarnessError("forced worktree-add failure")
            return original_run_capture(
                argv,
                cwd,
                check=check,
                env=env,
            )

        runtime.run_capture = fail_worktree_add
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
            runtime.run_capture = original_run_capture
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
        original_run_capture = runtime.run_capture

        def move_then_fail_worktree_add(
            argv: Sequence[str],
            cwd: Path | None = None,
            *,
            check: bool = True,
            env: Mapping[str, str] | None = None,
        ) -> subprocess.CompletedProcess:
            values = list(argv)
            if "worktree" in values and values[
                values.index("worktree") : values.index("worktree") + 2
            ] == ["worktree", "add"]:
                _git(
                    driver,
                    "update-ref",
                    f"refs/heads/{branch}",
                    valuable_oid,
                )
                raise runtime.HarnessError("forced moved-branch failure")
            return original_run_capture(
                argv,
                cwd,
                check=check,
                env=env,
            )

        runtime.run_capture = move_then_fail_worktree_add
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
            runtime.run_capture = original_run_capture
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
        opened, open_result = _invoke_open_json(
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
        contract = runtime.load_json(task_dir / "task.json")
        runtime_doc = runtime.load_json(task_dir / "runtime.json")
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
            "OPEN no-server response preserves its existing shape",
            (
                open_result["server_worktree"],
                "server_checkout_mode" in open_result,
            ),
            (None, False),
        )
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
            "OPEN binds the default cross-vendor policy",
            contract["allow_same_vendor_high_risk"],
            False,
        )
        test.equal(
            "OPEN defaults to Codex reviewer without spending Claude tokens",
            contract["reviewer"],
            "codex",
        )
        tampered_contract = dict(contract)
        tampered_contract["allow_same_vendor_high_risk"] = True
        test.true(
            "OPEN same-vendor policy is contract-hash-bound",
            verifier.document_hash(tampered_contract, "contract_sha256")
            != contract["contract_sha256"],
        )
        test.equal(
            "OPEN runtime records exact safe task root",
            runtime_doc["task_root"],
            str(task_root.resolve()),
        )
        test.true("OPEN writes valid task metadata and state", metadata_valid)
        test.equal(
            "OPEN leaves standalone driver HEAD detached",
            _git(driver, "branch", "--show-current"),
            "",
        )
        exception_task_id = "standalone-same-vendor"
        exception_args = _open_args(
            exception_task_id,
            base,
            f"agent/v2/{exception_task_id}",
        )
        exception_args.allow_same_vendor_high_risk = True
        _invoke_open(driver, exception_args)
        exception_contract = runtime.load_json(
            harness_cli.v2_task_dir(common, exception_task_id) / "task.json"
        )
        test.equal(
            "OPEN binds an explicit same-vendor high-risk exception",
            exception_contract["allow_same_vendor_high_risk"],
            True,
        )

    with tempfile.TemporaryDirectory(
        prefix="murmur-v2-driver-server-open-"
    ) as raw:
        root = Path(raw)
        _primary, driver, base = _standalone_driver(
            root,
            with_server_dependency=True,
        )
        source = root / "murmur-server"
        source_git_before = _git_tree_digest(source / ".git")
        task_id = "standalone-server-dependency"
        result, opened = _invoke_open_json(
            driver,
            _open_args(task_id, base, f"agent/v2/{task_id}"),
        )
        task_dir = harness_cli.v2_task_dir(
            (driver / ".git").resolve(), task_id
        )
        runtime_doc = runtime.load_json(task_dir / "runtime.json")
        server_worktree = (
            root
            / ".murmur-agent-tasks"
            / "v2"
            / task_id
            / "murmur-server"
        )
        revision = (
            root
            / ".murmur-agent-tasks"
            / "v2"
            / task_id
            / "meetnotes"
            / ".murmur-server-revision"
        ).read_text(encoding="utf-8").strip()

        test.equal(
            "OPEN provisions a pinned server dependency immediately",
            result,
            0,
        )
        test.equal(
            "OPEN prints the provisioned server checkout and mode",
            (
                opened["server_worktree"],
                opened["server_checkout_mode"],
            ),
            (
                str(server_worktree.resolve()),
                "local-shared-clone",
            ),
        )
        test.equal(
            "OPEN runtime records the local shared clone",
            (
                runtime_doc["server_worktree"],
                runtime_doc["server_revision"],
                runtime_doc["server_checkout_mode"],
            ),
            (
                str(server_worktree.resolve()),
                revision,
                "local-shared-clone",
            ),
        )
        test.equal(
            "OPEN checks out the exact pinned server revision",
            _git(server_worktree, "rev-parse", "HEAD"),
            revision,
        )
        test.equal(
            "OPEN server clone keeps Git metadata inside the task root",
            Path(
                _git(
                    server_worktree,
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-common-dir",
                )
            ).resolve(),
            (server_worktree / ".git").resolve(),
        )
        test.equal(
            "OPEN server provisioning does not mutate sibling Git metadata",
            _git_tree_digest(source / ".git"),
            source_git_before,
        )

        client_worktree = server_worktree.parent / "meetnotes"
        original_clean_intent_document = harness_cli._clean_intent_document
        raced_head = ""

        def commit_server_after_preflight(
            *intent_args: Any,
            **intent_kwargs: Any,
        ) -> Dict[str, Any]:
            nonlocal raced_head
            raced_file = server_worktree / "raced-operator-commit.txt"
            raced_file.write_bytes(b"valuable raced server bytes\n")
            _git(server_worktree, "add", "raced-operator-commit.txt")
            _git(
                server_worktree,
                "-c",
                "user.name=QueaT",
                "-c",
                "user.email=kgm004a@gmail.com",
                "commit",
                "-q",
                "-m",
                "raced clean server commit",
            )
            raced_head = _git(server_worktree, "rev-parse", "HEAD")
            return original_clean_intent_document(
                *intent_args,
                **intent_kwargs,
            )

        previous = Path.cwd()
        harness_cli._clean_intent_document = commit_server_after_preflight
        try:
            os.chdir(driver)
            test.raises(
                "CLEAN refuses a clean server commit racing preflight and intent",
                lambda: harness_cli.cmd_clean(
                    argparse.Namespace(
                        task_id=task_id,
                        abandon=True,
                    )
                ),
                "server revision changed",
            )
        finally:
            harness_cli._clean_intent_document = (
                original_clean_intent_document
            )
            os.chdir(previous)
        raced_intent = runtime.load_json(task_dir / "clean-intent.json")
        test.true(
            "CLEAN server race preserves client and raced server commit",
            client_worktree.is_dir()
            and server_worktree.is_dir()
            and raced_head != revision
            and _git(server_worktree, "rev-parse", "HEAD") == raced_head
            and (
                server_worktree / "raced-operator-commit.txt"
            ).read_bytes()
            == b"valuable raced server bytes\n",
        )
        test.true(
            "CLEAN server race performs no quarantine or terminal transition",
            not Path(raced_intent["quarantine_path"]).exists()
            and not Path(
                raced_intent["server_cleanup"]["quarantine_path"]
            ).exists()
            and not any(task_dir.glob("clean-delete-*.json"))
            and harness_cli.load_v2_state(task_dir)["status"]
            not in verifier.V2_TERMINAL_STATES,
        )

        clean_task_id = "standalone-server-clean-success"
        clean_result, _clean_opened = _invoke_open_json(
            driver,
            _open_args(
                clean_task_id,
                base,
                f"agent/v2/{clean_task_id}",
            ),
        )
        clean_task_dir = harness_cli.v2_task_dir(
            (driver / ".git").resolve(), clean_task_id
        )
        clean_runtime = runtime.load_json(
            clean_task_dir / "runtime.json"
        )
        clean_client = Path(
            runtime.load_json(clean_task_dir / "task.json")[
                "worktree_path"
            ]
        )
        clean_server = Path(str(clean_runtime["server_worktree"]))
        clean_client_revision = _git(clean_client, "rev-parse", "HEAD")
        _git(
            clean_client,
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "client reflog-only object closure",
        )
        client_reflog_commit = _git(clean_client, "rev-parse", "HEAD")
        _git(
            clean_client,
            "reset",
            "--soft",
            "-q",
            clean_client_revision,
        )
        index_blob_process = subprocess.run(
            ["git", "hash-object", "-w", "--stdin"],
            cwd=str(clean_client),
            input=b"client index-only object closure\n",
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if index_blob_process.returncode != 0:
            raise AssertionError(
                index_blob_process.stderr.decode(
                    "utf-8", "replace"
                ).strip()
            )
        client_index_blob = index_blob_process.stdout.decode(
            "ascii"
        ).strip()
        _git(
            clean_client,
            "update-index",
            "--cacheinfo",
            "100644",
            client_index_blob,
            "owned.txt",
        )
        test.true(
            "CLEAN linked client fixture keeps reflog-only and index-only objects",
            _git(clean_client, "rev-parse", "HEAD")
            == clean_client_revision
            and (clean_client / "owned.txt").read_bytes() == b"base\n"
            and client_reflog_commit != clean_client_revision
            and runtime.SHA1_RE.fullmatch(client_index_blob) is not None,
        )
        clean_server_revision = str(clean_runtime["server_revision"])
        server_saved_ref = "refs/selftest/archived-server-commit"
        _git(
            clean_server,
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "unique server Git-control archive payload",
        )
        server_saved_commit = _git(clean_server, "rev-parse", "HEAD")
        _git(
            clean_server,
            "update-ref",
            "--create-reflog",
            server_saved_ref,
            server_saved_commit,
        )
        _git(
            clean_server,
            "reset",
            "--hard",
            "-q",
            clean_server_revision,
        )
        server_control_payloads = _unique_git_control_payloads(
            clean_server,
            server_saved_ref,
            server_saved_commit,
        )
        test.equal(
            "CLEAN server Git-control fixture is visibly clean at its pin",
            (
                _git(clean_server, "rev-parse", "HEAD"),
                _git(clean_server, "status", "--porcelain"),
            ),
            (clean_server_revision, ""),
        )

        original_read_git_control_payload = (
            harness_cli._read_git_control_payload
        )
        injected_server_archive_failure = False

        def fail_during_server_control_payload(
            source_root: Path,
            entry: Mapping[str, Any],
        ) -> bytes:
            nonlocal injected_server_archive_failure
            payload = original_read_git_control_payload(
                source_root,
                entry,
            )
            if (
                not injected_server_archive_failure
                and source_root.resolve()
                == (clean_server / ".git").resolve()
                and entry["path"] == server_saved_ref
            ):
                injected_server_archive_failure = True
                raise runtime.HarnessError(
                    "forced Git-control payload archival failure"
                )
            return payload

        def clean_server_task() -> int:
            with contextlib.redirect_stdout(io.StringIO()):
                return harness_cli.cmd_clean(
                    argparse.Namespace(
                        task_id=clean_task_id,
                        abandon=True,
                    )
                )

        previous = Path.cwd()
        harness_cli._read_git_control_payload = (
            fail_during_server_control_payload
        )
        try:
            os.chdir(driver)
            test.raises(
                "CLEAN interruption occurs during server Git-control payload archival",
                clean_server_task,
                "forced Git-control payload archival failure",
            )
        finally:
            harness_cli._read_git_control_payload = (
                original_read_git_control_payload
            )
            os.chdir(previous)
        server_pre_manifest = (
            clean_task_dir / "clean-git-control-server-pre.json"
        )
        server_partial_payload = (
            clean_task_dir
            / "clean-git-control"
            / "server-pre"
            / "payload-00000000.bin"
        )
        test.true(
            "CLEAN interruption preserves roots and durable partial Git-control archive",
            injected_server_archive_failure
            and clean_client.is_dir()
            and clean_server.is_dir()
            and server_pre_manifest.is_file()
            and server_partial_payload.is_file()
            and not (
                clean_task_dir
                / "clean-git-control-server-pre-complete.json"
            ).exists(),
        )
        frozen_server_manifest = server_pre_manifest.read_bytes()
        frozen_partial_payload = server_partial_payload.read_bytes()
        frozen_partial_identity = (
            server_partial_payload.stat().st_ino,
            server_partial_payload.stat().st_mtime_ns,
        )
        previous = Path.cwd()
        try:
            os.chdir(driver)
            clean_exit = clean_server_task()
        finally:
            os.chdir(previous)
        test.equal(
            "CLEAN successful server fixture opens and exits green",
            (clean_result, clean_exit),
            (0, 0),
        )
        test.true(
            "CLEAN resume reuses immutable Git-control manifest and payload",
            server_pre_manifest.read_bytes() == frozen_server_manifest
            and server_partial_payload.read_bytes()
            == frozen_partial_payload
            and (
                server_partial_payload.stat().st_ino,
                server_partial_payload.stat().st_mtime_ns,
            )
            == frozen_partial_identity,
        )
        test.true(
            "CLEAN freezes all roots before removing client and server",
            not clean_client.exists()
            and not clean_server.exists()
            and harness_cli.load_v2_state(clean_task_dir)["status"]
            == "ABANDONED",
        )
        test.true(
            "CLEAN server pre/final archives recover hidden ref, reflog, and loose object",
            _git_control_payloads_recoverable(
                clean_task_dir,
                ("server-pre", "server-final"),
                server_control_payloads,
            ),
        )
        restored_server = root / "restored-server-control"
        restored_server_objects = _restore_archived_git_repository(
            clean_task_dir,
            "server-final",
            restored_server,
        )
        restored_server_fsck = subprocess.run(
            ["git", "fsck", "--full"],
            cwd=str(restored_server),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        test.true(
            "CLEAN standalone server archive reconstructs without its source owner",
            server_saved_commit in restored_server_objects
            and not _git_object_available(source, server_saved_commit)
            and _git(
                restored_server,
                "rev-parse",
                server_saved_ref,
            )
            == server_saved_commit
            and _git(
                restored_server,
                "cat-file",
                "-t",
                server_saved_commit,
            )
            == "commit"
            and restored_server_fsck.returncode == 0,
        )
        restored_client = root / "restored-client-control"
        restored_client_objects = _restore_archived_git_repository(
            clean_task_dir,
            "client-final",
            restored_client,
            linked_admin=True,
        )
        test.true(
            "CLEAN linked client pack hydrates reflog-only and index-only objects without owner access",
            {
                client_reflog_commit,
                client_index_blob,
            }.issubset(restored_client_objects)
            and _git(
                restored_client,
                "cat-file",
                "-t",
                client_reflog_commit,
            )
            == "commit"
            and _git(
                restored_client,
                "cat-file",
                "-t",
                client_index_blob,
            )
            == "blob",
        )

    with tempfile.TemporaryDirectory(
        prefix="murmur-v2-driver-server-rollback-"
    ) as raw:
        root = Path(raw)
        _primary, driver, base = _standalone_driver(
            root,
            with_server_dependency=True,
        )
        source = root / "murmur-server"
        source_git_before = _git_tree_digest(source / ".git")
        task_id = "standalone-server-rollback"
        branch = f"agent/v2/{task_id}"
        task_root = root / ".murmur-agent-tasks" / "v2" / task_id
        original_set_v2_state = harness_cli.set_v2_state

        def fail_after_server_provision(
            task_dir: Path, status: str, **updates: Any
        ) -> Dict[str, Any]:
            del task_dir, status, updates
            raise runtime.HarnessError(
                "forced failure after server provisioning"
            )

        harness_cli.set_v2_state = fail_after_server_provision
        try:
            test.raises(
                "OPEN reports a failure after server provisioning",
                lambda: _invoke_open(
                    driver,
                    _open_args(task_id, base, branch),
                ),
                "forced failure after server provisioning",
            )
        finally:
            harness_cli.set_v2_state = original_set_v2_state
        test.true(
            "OPEN rollback removes the provisioned server task root",
            not task_root.exists(),
        )
        test.equal(
            "OPEN rollback removes its unchanged client branch",
            harness_cli._local_branch_oid(driver, branch),
            None,
        )
        test.equal(
            "OPEN rollback leaves sibling Git metadata unchanged",
            _git_tree_digest(source / ".git"),
            source_git_before,
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


def clean_stage_barrier_cases(test: Tests) -> None:
    with tempfile.TemporaryDirectory(
        prefix="murmur-v2-clean-late-server-"
    ) as raw:
        root = Path(raw)
        _primary, driver, base = _standalone_driver(
            root,
            with_server_dependency=True,
        )
        task_id = "late-server-stage-mutation"
        opened, _document = _invoke_open_json(
            driver,
            _open_args(task_id, base, f"agent/v2/{task_id}"),
        )
        task_dir = harness_cli.v2_task_dir(
            (driver / ".git").resolve(), task_id
        )
        contract = runtime.load_json(task_dir / "task.json")
        client = Path(str(contract["worktree_path"]))
        client_owned = (client / "owned.txt").read_bytes()
        original_stage = harness_cli._stage_prepared_root_cleanup
        late_server_path: Optional[Path] = None

        def mutate_late_server(
            prepared: Mapping[str, Any],
        ) -> Dict[str, Any]:
            nonlocal late_server_path
            if prepared["role"] == "server" and late_server_path is None:
                late_server_path = (
                    Path(str(prepared["quarantine"]))
                    / "late-server-stage-mutation.txt"
                )
                late_server_path.write_bytes(
                    b"preserve late server bytes\n"
                )
            return original_stage(prepared)

        def clean_late_server() -> int:
            with contextlib.redirect_stdout(io.StringIO()):
                return harness_cli.cmd_clean(
                    argparse.Namespace(
                        task_id=task_id,
                        abandon=True,
                    )
                )

        previous = Path.cwd()
        harness_cli._stage_prepared_root_cleanup = mutate_late_server
        try:
            os.chdir(driver)
            test.raises(
                "CLEAN refuses a server mutation after all roots start staging",
                clean_late_server,
                "server quarantine gained unmanifested entries",
            )
        finally:
            harness_cli._stage_prepared_root_cleanup = original_stage
            os.chdir(previous)
        intent = runtime.load_json(task_dir / "clean-intent.json")
        client_manifest = runtime.load_json(
            task_dir / "clean-delete-client.json"
        )
        owned_entry = next(
            entry
            for entry in client_manifest["entries"]
            if entry["path"] == "owned.txt"
        )
        staged_owned = (
            Path(str(intent["delete_staging_path"]))
            / str(owned_entry["staging_name"])
        )
        test.true(
            "CLEAN late server refusal leaves the client staged and recoverable without purge",
            opened == 0
            and (task_dir / "clean-delete-all-started.json").is_file()
            and (task_dir / "clean-stage-client.json").is_file()
            and not (task_dir / "clean-delete-all-staged.json").exists()
            and staged_owned.read_bytes() == client_owned
            and Path(str(intent["quarantine_path"])).is_dir()
            and late_server_path is not None
            and late_server_path.read_bytes()
            == b"preserve late server bytes\n"
            and harness_cli.load_v2_state(task_dir)["status"]
            not in verifier.V2_TERMINAL_STATES,
        )
        if late_server_path is None:
            return
        late_server_path.unlink()
        previous = Path.cwd()
        try:
            os.chdir(driver)
            resumed = clean_late_server()
        finally:
            os.chdir(previous)
        test.equal(
            "CLEAN resumes staged roots after the late mutation is removed",
            (
                resumed,
                harness_cli.load_v2_state(task_dir)["status"],
            ),
            (0, "ABANDONED"),
        )

    with tempfile.TemporaryDirectory(
        prefix="murmur-v2-clean-linked-admin-"
    ) as raw:
        root = Path(raw)
        primary, driver, base = _standalone_driver(root)
        task_id = "linked-admin-stage-mutation"
        opened, _document = _invoke_open_json(
            driver,
            _open_args(task_id, base, f"agent/v2/{task_id}"),
        )
        task_dir = harness_cli.v2_task_dir(
            (driver / ".git").resolve(), task_id
        )
        original_admin_stage = harness_cli._stage_linked_admin
        saved_admin_payloads: Dict[str, bytes] = {}
        saved_admin_path: Optional[Path] = None
        saved_admin_staging: Optional[Path] = None

        def mutate_linked_admin(
            *,
            task_dir: Path,
            role: str,
            record: Mapping[str, Any],
            ready: Mapping[str, Any],
        ) -> Dict[str, Any]:
            nonlocal saved_admin_path, saved_admin_staging
            if role == "client" and saved_admin_path is None:
                saved_admin_path = Path(str(record["git_admin_path"]))
                saved_admin_staging = Path(
                    str(record["git_admin_staging_path"])
                )
                reference = (
                    "refs/worktree/selftest/late-admin-stage"
                )
                _git(
                    primary,
                    f"--git-dir={saved_admin_path}",
                    "update-ref",
                    "--create-reflog",
                    reference,
                    str(record["expected_revision"]),
                )
                for relative in (
                    reference,
                    f"logs/{reference}",
                ):
                    saved_admin_payloads[relative] = (
                        saved_admin_path / relative
                    ).read_bytes()
            return original_admin_stage(
                task_dir=task_dir,
                role=role,
                record=record,
                ready=ready,
            )

        previous = Path.cwd()
        harness_cli._stage_linked_admin = mutate_linked_admin
        try:
            os.chdir(driver)
            test.raises(
                "CLEAN refuses a non-HEAD linked-admin ref/reflog mutation before admin stage",
                lambda: harness_cli.cmd_clean(
                    argparse.Namespace(
                        task_id=task_id,
                        abandon=True,
                    )
                ),
                "linked Git admin metadata changed after delete-start",
            )
        finally:
            harness_cli._stage_linked_admin = original_admin_stage
            os.chdir(previous)
        test.true(
            "CLEAN linked-admin refusal preserves admin bytes and staged root without purge",
            opened == 0
            and saved_admin_path is not None
            and saved_admin_path.is_dir()
            and saved_admin_staging is not None
            and not saved_admin_staging.exists()
            and all(
                (saved_admin_path / relative).read_bytes() == payload
                for relative, payload in saved_admin_payloads.items()
            )
            and (task_dir / "clean-delete-all-started.json").is_file()
            and (task_dir / "clean-stage-client.json").is_file()
            and not (task_dir / "clean-delete-all-staged.json").exists()
            and harness_cli.load_v2_state(task_dir)["status"]
            not in verifier.V2_TERMINAL_STATES,
        )

    with tempfile.TemporaryDirectory(
        prefix="murmur-v2-sha256-rejection-"
    ) as raw:
        repository = Path(raw) / "sha256"
        initialized = subprocess.run(
            [
                "git",
                "init",
                "-q",
                "--object-format=sha256",
                str(repository),
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        test.equal(
            "CLEAN SHA-256 rejection fixture is supported by Git",
            initialized.returncode,
            0,
        )
        if initialized.returncode == 0:
            test.raises(
                "CLEAN explicitly rejects a SHA-256 Git-control repository",
                lambda: harness_cli._require_sha1_repository(
                    repository,
                    label="selftest Git-control repository",
                ),
                "must use Git SHA-1 object format; found sha256",
            )


def clean_global_validation_barrier_cases(test: Tests) -> None:
    def staged_root_payloads(
        task_dir: Path,
        intent: Mapping[str, Any],
        stages: Sequence[Mapping[str, Any]],
    ) -> tuple[Dict[tuple[str, str], Path], Dict[Path, bytes]]:
        records: Dict[str, Mapping[str, Any]] = {
            "client": {
                "delete_staging_path": intent["delete_staging_path"],
            }
        }
        if isinstance(intent.get("server_cleanup"), Mapping):
            records["server"] = intent["server_cleanup"]
        for index, record in enumerate(
            intent["verification_snapshots"]
        ):
            if record.get("present") is True:
                records[f"snapshot-{index:04d}"] = record
        paths: Dict[tuple[str, str], Path] = {}
        payloads: Dict[Path, bytes] = {}
        for stage in stages:
            role = str(stage["role"])
            manifest = runtime.load_json(
                task_dir / f"clean-delete-{role}.json"
            )
            entries = {
                str(entry["path"]): entry
                for entry in manifest["entries"]
            }
            staging = Path(
                str(records[role]["delete_staging_path"])
            )
            for relative in stage["root"]["staged"]:
                entry = entries[str(relative)]
                path = staging / str(entry["staging_name"])
                paths[(role, str(relative))] = path
                payloads[path] = (
                    os.readlink(path).encode(
                        "utf-8", "surrogateescape"
                    )
                    if path.is_symlink()
                    else path.read_bytes()
                )
        return paths, payloads

    with tempfile.TemporaryDirectory(
        prefix="murmur-v2-clean-global-server-"
    ) as raw:
        root = Path(raw)
        _primary, driver, base = _standalone_driver(
            root,
            with_server_dependency=True,
        )
        task_id = "global-server-stage-mutation"
        _invoke_open_json(
            driver,
            _open_args(task_id, base, f"agent/v2/{task_id}"),
        )
        task_dir = harness_cli.v2_task_dir(
            (driver / ".git").resolve(), task_id
        )
        original_bind = harness_cli._bind_all_roots_staged
        original_finalize = harness_cli._finalize_frozen_root_cleanup
        original_root_purge = harness_cli._purge_frozen_clean_tree
        original_admin_purge = harness_cli._purge_staged_git_admin
        finalized_roles: List[str] = []
        root_purges = 0
        admin_purges = 0
        staged_payloads: Dict[Path, bytes] = {}
        mutated_path: Optional[Path] = None
        mutated_metadata: Optional[tuple[int, int, int]] = None

        def inject_after_all_staged(
            **kwargs: Any,
        ) -> Dict[str, Any]:
            nonlocal mutated_path, mutated_metadata
            document = original_bind(**kwargs)
            intent = runtime.load_json(task_dir / "clean-intent.json")
            paths, payloads = staged_root_payloads(
                task_dir,
                intent,
                kwargs["stages"],
            )
            staged_payloads.update(payloads)
            mutated_path = paths[("server", "owned.txt")]
            metadata = mutated_path.stat()
            mutated_metadata = (
                stat.S_IMODE(metadata.st_mode),
                metadata.st_atime_ns,
                metadata.st_mtime_ns,
            )
            mutated_path.write_bytes(
                payloads[mutated_path] + b"post-barrier mutation\n"
            )
            return document

        def observe_finalize(
            prepared: Mapping[str, Any],
            stage: Mapping[str, Any],
        ) -> None:
            finalized_roles.append(str(prepared["role"]))
            original_finalize(prepared, stage)

        def observe_root_purge(*args: Any, **kwargs: Any) -> None:
            nonlocal root_purges
            root_purges += 1
            original_root_purge(*args, **kwargs)

        def observe_admin_purge(*args: Any, **kwargs: Any) -> None:
            nonlocal admin_purges
            admin_purges += 1
            original_admin_purge(*args, **kwargs)

        previous = Path.cwd()
        harness_cli._bind_all_roots_staged = inject_after_all_staged
        harness_cli._finalize_frozen_root_cleanup = observe_finalize
        harness_cli._purge_frozen_clean_tree = observe_root_purge
        harness_cli._purge_staged_git_admin = observe_admin_purge
        try:
            os.chdir(driver)
            test.raises(
                "CLEAN globally refuses a later server staged-payload mutation",
                lambda: harness_cli.cmd_clean(
                    argparse.Namespace(
                        task_id=task_id,
                        abandon=True,
                    )
                ),
            )
        finally:
            harness_cli._bind_all_roots_staged = original_bind
            harness_cli._finalize_frozen_root_cleanup = original_finalize
            harness_cli._purge_frozen_clean_tree = original_root_purge
            harness_cli._purge_staged_git_admin = original_admin_purge
            os.chdir(previous)
        test.true(
            "CLEAN global server refusal performs zero unlink and preserves every staged role",
            not finalized_roles
            and root_purges == 0
            and admin_purges == 0
            and (task_dir / "clean-delete-all-staged.json").is_file()
            and not (
                task_dir / "clean-delete-all-validated.json"
            ).exists()
            and mutated_path is not None
            and mutated_path.is_file()
            and all(
                path.is_file() or path.is_symlink()
                for path in staged_payloads
            )
            and all(
                path == mutated_path
                or (
                    os.readlink(path).encode(
                        "utf-8", "surrogateescape"
                    )
                    if path.is_symlink()
                    else path.read_bytes()
                )
                == payload
                for path, payload in staged_payloads.items()
            )
            and harness_cli.load_v2_state(task_dir)["status"]
            not in verifier.V2_TERMINAL_STATES,
        )
        if mutated_path is None or mutated_metadata is None:
            return
        mutated_path.write_bytes(staged_payloads[mutated_path])
        mutated_path.chmod(mutated_metadata[0])
        os.utime(
            mutated_path,
            ns=(mutated_metadata[1], mutated_metadata[2]),
        )
        previous = Path.cwd()
        try:
            os.chdir(driver)
            with contextlib.redirect_stdout(io.StringIO()):
                resumed = harness_cli.cmd_clean(
                    argparse.Namespace(
                        task_id=task_id,
                        abandon=True,
                    )
                )
        finally:
            os.chdir(previous)
        test.true(
            "CLEAN global server validation publishes only after repaired resume",
            resumed == 0
            and (
                task_dir / "clean-delete-all-validated.json"
            ).is_file()
            and harness_cli.load_v2_state(task_dir)["status"]
            == "ABANDONED",
        )

    with tempfile.TemporaryDirectory(
        prefix="murmur-v2-clean-global-admin-"
    ) as raw:
        root = Path(raw)
        _primary, driver, base = _standalone_driver(root)
        task_id = "global-client-admin-mutation"
        _invoke_open_json(
            driver,
            _open_args(task_id, base, f"agent/v2/{task_id}"),
        )
        task_dir = harness_cli.v2_task_dir(
            (driver / ".git").resolve(), task_id
        )
        original_bind = harness_cli._bind_all_roots_staged
        original_finalize = harness_cli._finalize_frozen_root_cleanup
        original_root_purge = harness_cli._purge_frozen_clean_tree
        original_admin_purge = harness_cli._purge_staged_git_admin
        finalized_roles: List[str] = []
        root_purges = 0
        admin_purges = 0
        staged_payloads: Dict[Path, bytes] = {}
        admin_payloads: Dict[str, bytes] = {}
        admin_staging: Optional[Path] = None
        admin_metadata: Optional[tuple[int, int]] = None
        extra_admin: Optional[Path] = None

        def inject_admin_after_all_staged(
            **kwargs: Any,
        ) -> Dict[str, Any]:
            nonlocal admin_staging, admin_metadata, extra_admin
            document = original_bind(**kwargs)
            intent = runtime.load_json(task_dir / "clean-intent.json")
            _paths, payloads = staged_root_payloads(
                task_dir,
                intent,
                kwargs["stages"],
            )
            staged_payloads.update(payloads)
            admin_staging = Path(
                str(intent["git_admin_staging_path"])
            )
            admin_payloads.update(
                _tree_leaf_payloads(admin_staging)
            )
            metadata = admin_staging.stat()
            admin_metadata = (
                metadata.st_atime_ns,
                metadata.st_mtime_ns,
            )
            extra_admin = admin_staging / "late-admin-entry"
            extra_admin.write_bytes(b"post-barrier admin mutation\n")
            return document

        def observe_finalize(
            prepared: Mapping[str, Any],
            stage: Mapping[str, Any],
        ) -> None:
            finalized_roles.append(str(prepared["role"]))
            original_finalize(prepared, stage)

        def observe_root_purge(*args: Any, **kwargs: Any) -> None:
            nonlocal root_purges
            root_purges += 1
            original_root_purge(*args, **kwargs)

        def observe_admin_purge(*args: Any, **kwargs: Any) -> None:
            nonlocal admin_purges
            admin_purges += 1
            original_admin_purge(*args, **kwargs)

        previous = Path.cwd()
        harness_cli._bind_all_roots_staged = (
            inject_admin_after_all_staged
        )
        harness_cli._finalize_frozen_root_cleanup = observe_finalize
        harness_cli._purge_frozen_clean_tree = observe_root_purge
        harness_cli._purge_staged_git_admin = observe_admin_purge
        try:
            os.chdir(driver)
            test.raises(
                "CLEAN globally refuses a linked-admin post-stage mutation",
                lambda: harness_cli.cmd_clean(
                    argparse.Namespace(
                        task_id=task_id,
                        abandon=True,
                    )
                ),
            )
        finally:
            harness_cli._bind_all_roots_staged = original_bind
            harness_cli._finalize_frozen_root_cleanup = original_finalize
            harness_cli._purge_frozen_clean_tree = original_root_purge
            harness_cli._purge_staged_git_admin = original_admin_purge
            os.chdir(previous)
        test.true(
            "CLEAN global admin refusal performs zero unlink and preserves root/admin staging",
            not finalized_roles
            and root_purges == 0
            and admin_purges == 0
            and (task_dir / "clean-delete-all-staged.json").is_file()
            and not (
                task_dir / "clean-delete-all-validated.json"
            ).exists()
            and all(
                (path.is_file() or path.is_symlink())
                and (
                    os.readlink(path).encode(
                        "utf-8", "surrogateescape"
                    )
                    if path.is_symlink()
                    else path.read_bytes()
                )
                == payload
                for path, payload in staged_payloads.items()
            )
            and admin_staging is not None
            and all(
                (admin_staging / relative).read_bytes() == payload
                for relative, payload in admin_payloads.items()
            )
            and extra_admin is not None
            and extra_admin.read_bytes()
            == b"post-barrier admin mutation\n"
            and harness_cli.load_v2_state(task_dir)["status"]
            not in verifier.V2_TERMINAL_STATES,
        )
        if (
            extra_admin is None
            or admin_staging is None
            or admin_metadata is None
        ):
            return
        extra_admin.unlink()
        os.utime(
            admin_staging,
            ns=(admin_metadata[0], admin_metadata[1]),
        )
        previous = Path.cwd()
        try:
            os.chdir(driver)
            with contextlib.redirect_stdout(io.StringIO()):
                resumed = harness_cli.cmd_clean(
                    argparse.Namespace(
                        task_id=task_id,
                        abandon=True,
                    )
                )
        finally:
            os.chdir(previous)
        test.true(
            "CLEAN global admin validation publishes only after repaired resume",
            resumed == 0
            and (
                task_dir / "clean-delete-all-validated.json"
            ).is_file()
            and harness_cli.load_v2_state(task_dir)["status"]
            == "ABANDONED",
        )

    with tempfile.TemporaryDirectory(
        prefix="murmur-v2-clean-global-archive-"
    ) as raw:
        root = Path(raw)
        _primary, driver, base = _standalone_driver(root)
        task_id = "global-client-archive-hardlink"
        _invoke_open_json(
            driver,
            _open_args(task_id, base, f"agent/v2/{task_id}"),
        )
        task_dir = harness_cli.v2_task_dir(
            (driver / ".git").resolve(), task_id
        )
        original_bind = harness_cli._bind_all_roots_staged
        original_finalize = harness_cli._finalize_frozen_root_cleanup
        original_root_purge = harness_cli._purge_frozen_clean_tree
        original_admin_purge = harness_cli._purge_staged_git_admin
        finalized_roles: List[str] = []
        root_purges = 0
        admin_purges = 0
        staged_payloads: Dict[Path, bytes] = {}
        admin_payloads: Dict[str, bytes] = {}
        admin_staging: Optional[Path] = None
        archive_payload: Optional[Path] = None
        archive_bytes = b""
        archive_mode = 0
        archive_root_mode = 0
        external_link: Optional[Path] = None

        def inject_archive_hardlink_after_all_staged(
            **kwargs: Any,
        ) -> Dict[str, Any]:
            nonlocal admin_staging, archive_payload
            nonlocal archive_bytes, archive_mode, archive_root_mode
            nonlocal external_link
            document = original_bind(**kwargs)
            intent = runtime.load_json(task_dir / "clean-intent.json")
            _paths, payloads = staged_root_payloads(
                task_dir,
                intent,
                kwargs["stages"],
            )
            staged_payloads.update(payloads)
            admin_staging = Path(
                str(intent["git_admin_staging_path"])
            )
            admin_payloads.update(
                _tree_leaf_payloads(admin_staging)
            )
            completion = runtime.load_json(
                task_dir
                / "clean-git-control-client-final-complete.json"
            )
            record = next(
                payload
                for payload in completion["payloads"]
                if payload["source"] == "control"
            )
            archive_payload = task_dir / str(record["payload"])
            archive_bytes = archive_payload.read_bytes()
            archive_mode = stat.S_IMODE(
                archive_payload.stat().st_mode
            )
            archive_root_mode = stat.S_IMODE(
                archive_payload.parent.stat().st_mode
            )
            external_link = root / "external-archive-payload"
            external_link.write_bytes(archive_bytes)
            external_link.chmod(archive_mode)
            archive_payload.parent.chmod(0o700)
            archive_payload.unlink()
            os.link(external_link, archive_payload)
            archive_payload.parent.chmod(archive_root_mode)
            return document

        def observe_finalize(
            prepared: Mapping[str, Any],
            stage: Mapping[str, Any],
        ) -> None:
            finalized_roles.append(str(prepared["role"]))
            original_finalize(prepared, stage)

        def observe_root_purge(*args: Any, **kwargs: Any) -> None:
            nonlocal root_purges
            root_purges += 1
            original_root_purge(*args, **kwargs)

        def observe_admin_purge(*args: Any, **kwargs: Any) -> None:
            nonlocal admin_purges
            admin_purges += 1
            original_admin_purge(*args, **kwargs)

        previous = Path.cwd()
        harness_cli._bind_all_roots_staged = (
            inject_archive_hardlink_after_all_staged
        )
        harness_cli._finalize_frozen_root_cleanup = observe_finalize
        harness_cli._purge_frozen_clean_tree = observe_root_purge
        harness_cli._purge_staged_git_admin = observe_admin_purge
        try:
            os.chdir(driver)
            test.raises(
                "CLEAN globally refuses a non-private client archive payload",
                lambda: harness_cli.cmd_clean(
                    argparse.Namespace(
                        task_id=task_id,
                        abandon=True,
                    )
                ),
                "not a private immutable file",
            )
        finally:
            harness_cli._bind_all_roots_staged = original_bind
            harness_cli._finalize_frozen_root_cleanup = original_finalize
            harness_cli._purge_frozen_clean_tree = original_root_purge
            harness_cli._purge_staged_git_admin = original_admin_purge
            os.chdir(previous)
        test.true(
            "CLEAN archive hardlink refusal performs zero unlink and preserves all staging",
            not finalized_roles
            and root_purges == 0
            and admin_purges == 0
            and (task_dir / "clean-delete-all-staged.json").is_file()
            and not (
                task_dir / "clean-delete-all-validated.json"
            ).exists()
            and archive_payload is not None
            and archive_payload.read_bytes() == archive_bytes
            and archive_payload.stat().st_nlink == 2
            and external_link is not None
            and external_link.read_bytes() == archive_bytes
            and all(
                (path.is_file() or path.is_symlink())
                and (
                    os.readlink(path).encode(
                        "utf-8", "surrogateescape"
                    )
                    if path.is_symlink()
                    else path.read_bytes()
                )
                == payload
                for path, payload in staged_payloads.items()
            )
            and admin_staging is not None
            and _tree_leaf_payloads(admin_staging)
            == admin_payloads
            and harness_cli.load_v2_state(task_dir)["status"]
            not in verifier.V2_TERMINAL_STATES,
        )
        if archive_payload is None or external_link is None:
            return
        archive_payload.parent.chmod(0o700)
        archive_payload.unlink()
        replacement = (
            archive_payload.parent / "payload-private-restore.tmp"
        )
        replacement.write_bytes(archive_bytes)
        replacement.chmod(archive_mode)
        os.replace(replacement, archive_payload)
        archive_payload.parent.chmod(archive_root_mode)
        external_link.unlink()
        test.true(
            "CLEAN archive payload repair restores an exact private inode",
            archive_payload.read_bytes() == archive_bytes
            and archive_payload.stat().st_nlink == 1,
        )
        previous = Path.cwd()
        try:
            os.chdir(driver)
            with contextlib.redirect_stdout(io.StringIO()):
                resumed = harness_cli.cmd_clean(
                    argparse.Namespace(
                        task_id=task_id,
                        abandon=True,
                    )
                )
        finally:
            os.chdir(previous)
        test.true(
            "CLEAN archive validation publishes only after private-payload resume",
            resumed == 0
            and (
                task_dir / "clean-delete-all-validated.json"
            ).is_file()
            and harness_cli.load_v2_state(task_dir)["status"]
            == "ABANDONED",
        )


def _profile(
    paths: Sequence[str], claims: Sequence[str] = ()
) -> tuple[List[str], List[str], List[str]]:
    checks, reviews, risks = verifier.derive_profile(
        list(paths),
        list(claims),
        runtime.load_config(),
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

    checks, _, _ = _profile(["package.json", "package-lock.json"])
    test.equal(
        "PROFILE package manifests run lock evidence before Angular gates",
        checks,
        ["npm-lock", "ng-lint", "ng-build"],
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
    _checks, default_reviews, _risks = verifier.derive_profile(
        ["src-tauri/src/storage/meeting_store.rs"],
        [],
        runtime.load_config(),
        reviewer="codex",
    )
    test.equal(
        "PROFILE Codex lock defaults to cross-vendor specialist",
        [review["vendor"] for review in default_reviews],
        ["codex", "claude"],
    )
    _checks, same_vendor_reviews, _risks = verifier.derive_profile(
        ["src-tauri/src/storage/meeting_store.rs"],
        [],
        runtime.load_config(),
        reviewer="codex",
        allow_same_vendor_high_risk=True,
    )
    test.equal(
        "PROFILE explicit Codex lock exception keeps both reviews on Codex",
        [review["vendor"] for review in same_vendor_reviews],
        ["codex", "codex"],
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
    harness_python = runtime.load_config()["canonical_checks"][
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
    canonical_v2_selftest = runtime.load_config()["canonical_checks"][
        "harness-v2-selftest"
    ]
    test.true(
        "PROFILE canonical v2 selftest inherits the outer sandbox",
        runtime.command_is_inherited_sandbox_meta_check(
            canonical_v2_selftest
        ),
    )
    test.equal(
        "PROFILE npm-lock check does not request the Sherpa archive",
        runtime.command_needs_sherpa_archive(
            "python3 -B .agents/harness/checks/npm-lock-evidence.py"
        ),
        False,
    )
    test.true(
        "PROFILE performance contracts request the Sherpa archive",
        runtime.command_needs_sherpa_archive(
            "bash .agents/harness/checks/perf-contracts.sh"
        ),
    )
    test.true(
        "PROFILE client Rust checks request the Sherpa archive",
        runtime.command_needs_sherpa_archive(
            "(cd src-tauri && cargo test --lib)"
        ),
    )
    review_schema = runtime.load_schema("v2-review")
    schema_probe_ids = set(
        review_schema["properties"]["probe_requests"]["items"]["properties"][
            "probe_id"
        ]["enum"]
    )
    test.equal(
        "PROFILE static probe vocabulary matches executable canonical ids",
        schema_probe_ids,
        verifier.ALLOWED_PROBES,
    )
    test.equal(
        "PROFILE every static probe id has exactly one canonical command",
        set(runtime.load_config()["canonical_checks"]),
        verifier.ALLOWED_PROBES,
    )


def npm_lock_evidence_cases(test: Tests) -> None:
    script = ROOT / ".agents" / "harness" / "checks" / "npm-lock-evidence.py"
    with tempfile.TemporaryDirectory(prefix="murmur-v2-npm-lock-valid-") as raw:
        repo = Path(raw) / "repo"
        _init_repo(repo)
        manifest = runtime.load_json(ROOT / "package.json")
        lock = runtime.load_json(ROOT / "package-lock.json")
        lock["version"] = manifest["version"]
        lock["packages"][""]["version"] = manifest["version"]
        manifest["optionalDependencies"] = {"nanoid": "^3.3.15"}
        manifest["peerDependencies"] = {"postcss": "^8.5.13"}
        manifest["peerDependenciesMeta"] = {
            "postcss": {"optional": False}
        }
        lock["packages"][""]["optionalDependencies"] = dict(
            manifest["optionalDependencies"]
        )
        lock["packages"][""]["peerDependencies"] = dict(
            manifest["peerDependencies"]
        )
        lock["packages"][""]["peerDependenciesMeta"] = copy.deepcopy(
            manifest["peerDependenciesMeta"]
        )
        runtime.atomic_write_json(repo / "package.json", manifest)
        runtime.atomic_write_json(repo / "package-lock.json", lock)
        _git(repo, "add", "package.json", "package-lock.json")
        _git(repo, "commit", "-q", "-m", "add coherent package fixture")
        base_sha = _git(repo, "rev-parse", "HEAD")
        before = runtime.git_bytes(repo, "status", "--porcelain", "-z")
        completed = subprocess.run(
            [sys.executable, str(script)],
            cwd=str(repo),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env={
                **os.environ,
                "MURMUR_HARNESS_BASE_SHA": base_sha,
            },
            check=False,
        )
        after = runtime.git_bytes(repo, "status", "--porcelain", "-z")
        try:
            evidence = json.loads(completed.stdout)
        except json.JSONDecodeError:
            evidence = {}
        test.equal(
            "NPM-LOCK coherent repository evidence is read-only and green",
            (completed.returncode, after == before),
            (0, True),
        )
        direct_names = {
            name
            for key in (
                "dependencies",
                "devDependencies",
                "optionalDependencies",
                "peerDependencies",
            )
            for name in evidence.get("direct", {}).get(key, {})
        }
        versions = evidence.get("versions_by_name", {})
        test.true(
            "NPM-LOCK evidence covers every direct logical version",
            bool(direct_names)
            and direct_names.issubset(set(versions))
            and all(versions.get(name) for name in direct_names),
        )
        test.true(
            "NPM-LOCK evidence covers optional and peer dependency classes",
            {
                "nanoid",
                "postcss",
            }.issubset(direct_names)
            and evidence.get("direct", {}).get("peerDependenciesMeta")
            == {"postcss": {"optional": False}},
        )
        test.true(
            "NPM-LOCK evidence remains compact and names its offline mode",
            len(completed.stdout) <= 65_536
            and evidence.get("mode") == "offline-package-lock-only",
        )

        committed_manifest = copy.deepcopy(manifest)
        committed_lock = copy.deepcopy(lock)
        committed_manifest["dependencies"] = {
            **committed_manifest.get("dependencies", {}),
            "base-binding-fixture": "1.0.0",
        }
        committed_lock["packages"][""]["dependencies"] = dict(
            committed_manifest["dependencies"]
        )
        committed_lock["packages"]["node_modules/base-binding-fixture"] = {
            "version": "1.0.0"
        }
        runtime.atomic_write_json(repo / "package.json", committed_manifest)
        runtime.atomic_write_json(repo / "package-lock.json", committed_lock)
        _git(repo, "add", "package.json", "package-lock.json")
        _git(repo, "commit", "-q", "-m", "commit task dependency change")
        committed_run = subprocess.run(
            [sys.executable, str(script)],
            cwd=str(repo),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env={
                **os.environ,
                "MURMUR_HARNESS_BASE_SHA": base_sha,
            },
            check=False,
        )
        try:
            committed_evidence = json.loads(committed_run.stdout)
        except json.JSONDecodeError:
            committed_evidence = {}
        changed_names = {
            row.get("name")
            for row in committed_evidence.get("changed_lock_entries", [])
        }
        test.true(
            "NPM-LOCK committed task diff remains bound to immutable base",
            committed_run.returncode == 0
            and committed_evidence.get("base_sha") == base_sha
            and "base-binding-fixture" in changed_names
            and committed_evidence.get("base_package_json_sha256")
            == hashlib.sha256(
                runtime.git_bytes(repo, "show", f"{base_sha}:package.json")
            ).hexdigest(),
        )

        removed_manifest = copy.deepcopy(committed_manifest)
        removed_lock = copy.deepcopy(committed_lock)
        removed_manifest["dependencies"].pop("rxjs")
        removed_lock["packages"][""]["dependencies"].pop("rxjs")
        rxjs_package_before = copy.deepcopy(
            lock["packages"]["node_modules/rxjs"]
        )
        test.equal(
            "NPM-LOCK removed direct fixture keeps package entry unchanged",
            removed_lock["packages"]["node_modules/rxjs"],
            rxjs_package_before,
        )
        runtime.atomic_write_json(repo / "package.json", removed_manifest)
        runtime.atomic_write_json(repo / "package-lock.json", removed_lock)
        removed_run = subprocess.run(
            [sys.executable, str(script)],
            cwd=str(repo),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env={
                **os.environ,
                "MURMUR_HARNESS_BASE_SHA": base_sha,
            },
            check=False,
        )
        try:
            removed_evidence = json.loads(removed_run.stdout)
        except json.JSONDecodeError:
            removed_evidence = {}
        test.true(
            "NPM-LOCK removed direct still reports transitive versions",
            removed_run.returncode == 0
            and "rxjs"
            in removed_evidence.get(
                "changed_manifest_dependency_names", []
            )
            and bool(
                removed_evidence.get("versions_by_name", {}).get("rxjs")
            ),
        )

    with tempfile.TemporaryDirectory(prefix="murmur-v2-npm-lock-") as raw:
        repo = Path(raw) / "repo"
        _init_repo(repo)
        manifest = {
            "name": "fixture",
            "version": "1.0.0",
            "dependencies": {"example": "1.0.0"},
            "devDependencies": {},
            "scripts": {},
        }
        lock = {
            "name": "fixture",
            "version": "1.0.0",
            "lockfileVersion": 3,
            "packages": {
                "": {
                    "name": "fixture",
                    "version": "1.0.0",
                    "dependencies": {"example": "1.0.0"},
                    "devDependencies": {},
                }
            },
        }
        runtime.atomic_write_json(repo / "package.json", manifest)
        runtime.atomic_write_json(repo / "package-lock.json", lock)
        _git(repo, "add", "package.json", "package-lock.json")
        _git(repo, "commit", "-q", "-m", "add package fixture")
        base_sha = _git(repo, "rev-parse", "HEAD")
        bound_environment = {
            **os.environ,
            "MURMUR_HARNESS_BASE_SHA": base_sha,
        }

        duplicate = (
            '{"name":"fixture","version":"1.0.0","version":"2.0.0",'
            '"dependencies":{"example":"1.0.0"},"devDependencies":{},'
            '"scripts":{}}\n'
        )
        (repo / "package.json").write_text(duplicate, encoding="utf-8")
        duplicate_run = subprocess.run(
            [sys.executable, str(script)],
            cwd=str(repo),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=bound_environment,
            check=False,
        )
        test.true(
            "NPM-LOCK duplicate JSON keys fail closed",
            duplicate_run.returncode != 0
            and b"duplicate JSON key" in duplicate_run.stderr,
        )

        runtime.atomic_write_json(repo / "package.json", manifest)
        broken_lock = copy.deepcopy(lock)
        broken_lock["packages"][""]["dependencies"] = {}
        runtime.atomic_write_json(repo / "package-lock.json", broken_lock)
        mismatch_run = subprocess.run(
            [sys.executable, str(script)],
            cwd=str(repo),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=bound_environment,
            check=False,
        )
        test.true(
            "NPM-LOCK manifest/root mismatch fails before npm execution",
            mismatch_run.returncode != 0
            and b"lock root dependencies differ" in mismatch_run.stderr,
        )

        optional_manifest = copy.deepcopy(manifest)
        optional_manifest["optionalDependencies"] = {
            "optional-example": "1.0.0"
        }
        runtime.atomic_write_json(repo / "package.json", optional_manifest)
        runtime.atomic_write_json(repo / "package-lock.json", lock)
        optional_mismatch_run = subprocess.run(
            [sys.executable, str(script)],
            cwd=str(repo),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=bound_environment,
            check=False,
        )
        test.true(
            "NPM-LOCK optional dependency mismatch fails closed",
            optional_mismatch_run.returncode != 0
            and b"lock root optionalDependencies differ"
            in optional_mismatch_run.stderr,
        )

        peer_manifest = copy.deepcopy(manifest)
        peer_manifest["peerDependencies"] = {"peer-example": "1.0.0"}
        peer_manifest["peerDependenciesMeta"] = {
            "peer-example": {"optional": True}
        }
        runtime.atomic_write_json(repo / "package.json", peer_manifest)
        runtime.atomic_write_json(repo / "package-lock.json", lock)
        peer_mismatch_run = subprocess.run(
            [sys.executable, str(script)],
            cwd=str(repo),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=bound_environment,
            check=False,
        )
        test.true(
            "NPM-LOCK peer dependency mismatch fails closed",
            peer_mismatch_run.returncode != 0
            and b"lock root peerDependencies differ"
            in peer_mismatch_run.stderr,
        )

        peer_lock = copy.deepcopy(lock)
        peer_lock["packages"][""]["peerDependencies"] = {
            "peer-example": "1.0.0"
        }
        runtime.atomic_write_json(repo / "package.json", peer_manifest)
        runtime.atomic_write_json(repo / "package-lock.json", peer_lock)
        peer_meta_mismatch_run = subprocess.run(
            [sys.executable, str(script)],
            cwd=str(repo),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=bound_environment,
            check=False,
        )
        test.true(
            "NPM-LOCK peer dependency metadata mismatch fails closed",
            peer_meta_mismatch_run.returncode != 0
            and b"lock root peerDependenciesMeta differ"
            in peer_meta_mismatch_run.stderr,
        )


def reviewer_tool_guard_cases(test: Tests) -> None:
    isolated_cwd = runtime.reviewer_execution_cwd()
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
        original_reviewer_cwd = runtime.REVIEWER_CWD
        runtime.REVIEWER_CWD = mutable_cwd
        try:
            test.raises(
                "REVIEWER mutable temporary cwd is rejected",
                runtime.reviewer_execution_cwd,
                "mutable by the invoking user",
            )
        finally:
            runtime.REVIEWER_CWD = original_reviewer_cwd

    codex_environment = runtime.reviewer_model_environment(
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

    decision = runtime.REVIEWER_TOOL_DENIAL
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
    command = runtime.reviewer_tool_guard_command()
    test.true(
        "REVIEWER inline guard has no mutable worktree dependency",
        command.startswith("/usr/bin/printf ")
        and "reviewer_tool_guard" not in command
        and str(runtime.HARNESS_ROOT) not in command,
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
            runtime.reviewer_tool_activity(
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
            in runtime.reviewer_tool_activity(missing_init, "claude"),
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
                bool(runtime.reviewer_tool_activity(tool_log, "claude")),
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
                for item in runtime.reviewer_tool_activity(
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
                for item in runtime.reviewer_tool_activity(
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
            runtime.reviewer_tool_activity(codex_prose, "codex"),
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
                bool(runtime.reviewer_tool_activity(codex_tool, "codex")),
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


def focused_review_evidence_cases(test: Tests) -> None:
    with tempfile.TemporaryDirectory(prefix="murmur-v2-focused-evidence-") as raw:
        task_dir = Path(raw)
        logs = task_dir / "logs"
        logs.mkdir()
        padding = "x" * (verifier.DEFAULT_REVIEW_STREAM_BYTES + 128)
        lock_lines = [
            "test tools::tests::org_read_requires_member ... ok",
            "test storage::tests::context_disabled_hides_org_item ... ok",
            "test storage::tests::tombstone_removes_org_item ... ok",
            "test mcp::tests::revocation_stops_locked_tail ... ok",
        ]
        egress_lines = [
            "test summarize::tests::remote_ollama_requires_consent ... ok",
            "test summarize::tests::egress_ledger_is_content_free ... ok",
        ]
        nonmatching = "test unrelated::tests::middle_secret_sentinel ... ok"
        stdout = logs / "rust.stdout.log"
        stdout.write_text(
            "HEAD\n"
            + padding
            + "\n"
            + nonmatching
            + "\n"
            + "\n".join(lock_lines + egress_lines)
            + "\n"
            + padding
            + "\nTAIL\n",
            encoding="utf-8",
        )
        stderr = logs / "rust.stderr.log"
        stderr.write_text("", encoding="utf-8")

        def item_for(path: Path) -> Dict[str, Any]:
            return {
                "id": "rust-lib",
                "command": "cargo test --lib",
                "evidence": {
                    "passed": True,
                    "outcome": "PASS",
                    "exit_code": 0,
                    "duration_ms": 1,
                    "resource_wait_ms": 0,
                    "log_sha256": "0" * 64,
                    "stdout_path": str(path),
                    "stdout_sha256": runtime.sha256_file(path),
                    "stderr_path": str(stderr),
                    "stderr_sha256": runtime.sha256_file(stderr),
                },
            }

        item = item_for(stdout)
        lock_summary = verifier._review_evidence_summary(
            item,
            task_dir,
            channel="planned-check",
            review_kind="lock-security",
        )
        lock_inventory = lock_summary["stdout"]["focused_test_inventory"]
        test.true(
            "FOCUSED EVIDENCE surfaces security tests from truncated middle",
            all(line in lock_inventory["excerpt"] for line in lock_lines)
            and all(line not in lock_summary["stdout"]["excerpt"] for line in lock_lines),
        )
        test.true(
            "FOCUSED EVIDENCE omits nonmatching middle output",
            nonmatching not in lock_inventory["excerpt"]
            and nonmatching not in lock_summary["stdout"]["excerpt"],
        )
        egress_inventory = verifier._review_evidence_summary(
            item,
            task_dir,
            channel="reviewer-probe",
            review_kind="egress-security",
        )["stdout"]["focused_test_inventory"]
        test.true(
            "FOCUSED EVIDENCE is specialist-specific",
            all(line in egress_inventory["excerpt"] for line in egress_lines)
            and lock_lines[0] not in egress_inventory["excerpt"],
        )

        many = logs / "many.stdout.log"
        many.write_text(
            "".join(
                f"test org::tests::case_{index:04d} ... ok\n"
                for index in range(
                    verifier.SPECIALIST_TEST_FOCUS_LINES_PER_TERM + 20
                )
            ),
            encoding="utf-8",
        )
        bounded = verifier._review_evidence_summary(
            item_for(many),
            task_dir,
            channel="planned-check",
            review_kind="lock-security",
        )["stdout"]["focused_test_inventory"]
        test.equal(
            "FOCUSED EVIDENCE bounds each term deterministically",
            (
                bounded["included_lines"],
                bounded["matching_lines_total"],
                bounded["truncated"],
            ),
            (
                verifier.SPECIALIST_TEST_FOCUS_LINES_PER_TERM,
                verifier.SPECIALIST_TEST_FOCUS_LINES_PER_TERM + 20,
                True,
            ),
        )

        overlapping = logs / "overlapping.stdout.log"
        overlapping.write_text(
            "".join(
                f"test org::tests::org_only_{index:04d} ... ok\n"
                for index in range(
                    verifier.SPECIALIST_TEST_FOCUS_LINES_PER_TERM
                )
            )
            + "".join(
                f"test org::member::tests::overlap_{index:04d} ... ok\n"
                for index in range(
                    verifier.SPECIALIST_TEST_FOCUS_LINES_PER_TERM + 4
                )
            ),
            encoding="utf-8",
        )
        overlapping_inventory = verifier._review_evidence_summary(
            item_for(overlapping),
            task_dir,
            channel="planned-check",
            review_kind="lock-security",
        )["stdout"]["focused_test_inventory"]
        test.true(
            "FOCUSED EVIDENCE overlapping terms retain independent hard caps",
            overlapping_inventory["per_term_included"]["org"]
            == verifier.SPECIALIST_TEST_FOCUS_LINES_PER_TERM
            and overlapping_inventory["per_term_included"]["member"]
            == verifier.SPECIALIST_TEST_FOCUS_LINES_PER_TERM
            and all(
                count <= verifier.SPECIALIST_TEST_FOCUS_LINES_PER_TERM
                for count in overlapping_inventory["per_term_included"].values()
            ),
        )

        byte_pressure = logs / "byte-pressure.stdout.log"
        long_name = "z" * 4_000
        byte_pressure.write_text(
            "".join(
                f"test org::tests::{long_name}_{index:02d} ... ok\n"
                for index in range(
                    verifier.SPECIALIST_TEST_FOCUS_LINES_PER_TERM
                )
            ),
            encoding="utf-8",
        )
        first_byte_summary = verifier._review_evidence_summary(
            item_for(byte_pressure),
            task_dir,
            channel="planned-check",
            review_kind="lock-security",
        )["stdout"]["focused_test_inventory"]
        second_byte_summary = verifier._review_evidence_summary(
            item_for(byte_pressure),
            task_dir,
            channel="planned-check",
            review_kind="lock-security",
        )["stdout"]["focused_test_inventory"]
        test.true(
            "FOCUSED EVIDENCE byte pressure is bounded complete and deterministic",
            first_byte_summary["matching_lines_total"]
            == verifier.SPECIALIST_TEST_FOCUS_LINES_PER_TERM
            and first_byte_summary["included_lines"]
            < first_byte_summary["matching_lines_total"]
            and first_byte_summary["included_bytes"]
            <= verifier.SPECIALIST_TEST_FOCUS_BYTES
            and first_byte_summary["truncated"]
            and first_byte_summary == second_byte_summary,
        )

        corrupt_item = item_for(stdout)
        corrupt_item["evidence"]["stdout_sha256"] = "f" * 64
        test.raises(
            "FOCUSED EVIDENCE remains bound to raw artifact hash",
            lambda: verifier._review_evidence_summary(
                corrupt_item,
                task_dir,
                channel="planned-check",
                review_kind="lock-security",
            ),
            "hash changed",
        )


def specialist_source_context_cases(test: Tests) -> None:
    base_sha = _git(ROOT, "rev-parse", "HEAD")
    plan: Dict[str, Any] = {
        "base_sha": base_sha,
        "changed_paths": [],
        "claims": [],
        "actual_risk_flags": ["egress"],
        "checks": [],
        "diff_sha256": "1" * 64,
        "plan_sha256": "2" * 64,
        "protocol_sha256": "3" * 64,
        "server_required": False,
    }
    context = verifier.specialist_source_context(
        ROOT, plan, "egress-security"
    )
    included = context["entries"][0]
    excerpt = included["excerpt"]
    source_blob = subprocess.run(
        [
            "git",
            "show",
            f"{base_sha}:src-tauri/src/summarize/mod.rs",
        ],
        cwd=str(ROOT),
        stdout=subprocess.PIPE,
        check=True,
    ).stdout
    excerpt_bytes = excerpt.encode("utf-8")
    test.true(
        "SOURCE CONTEXT exposes the complete canonical egress factory seam",
        included["status"] == "included"
        and not included["truncated"]
        and "egress_is_cloud(id, config)" in excerpt
        and "PROVIDER_CLAUDE_CODE" in excerpt
        and "PROVIDER_ANTHROPIC" in excerpt
        and "PROVIDER_OLLAMA" in excerpt
        and "PROVIDER_GATEWAY" in excerpt
        and "RedactingProvider::with_name_redactor_sink_and_model_admission"
        in excerpt
        and "active_sink()" in excerpt
        and excerpt.rstrip().endswith("}")
        and "Provider instances for the Settings UI" not in excerpt
        and "pub fn all_providers" not in excerpt,
    )
    test.equal(
        "SOURCE CONTEXT binds exact file selected and included byte counts",
        (
            included["file_bytes"],
            included["file_sha256"],
            included["selected_bytes"],
            included["selected_sha256"],
            included["included_bytes"],
            included["excerpt_sha256"],
        ),
        (
            len(source_blob),
            hashlib.sha256(source_blob).hexdigest(),
            len(excerpt_bytes),
            hashlib.sha256(excerpt_bytes).hexdigest(),
            len(excerpt_bytes),
            hashlib.sha256(excerpt_bytes).hexdigest(),
        ),
    )
    changed_plan = copy.deepcopy(plan)
    changed_plan["changed_paths"] = [
        "src-tauri/src/summarize/mod.rs"
    ]
    excluded = verifier.specialist_source_context(
        ROOT, changed_plan, "egress-security"
    )["entries"][0]
    test.equal(
        "SOURCE CONTEXT excludes a changed seam instead of showing stale base",
        (excluded["status"], "excerpt" in excluded),
        ("excluded_changed_path", False),
    )

    with tempfile.TemporaryDirectory(prefix="murmur-v2-source-context-") as raw:
        repo = Path(raw) / "repo"
        _init_repo(repo)
        source = repo / "src-tauri" / "src" / "summarize" / "mod.rs"
        source.parent.mkdir(parents=True)
        lexical_fixture = (
            "fn make_provider_resolved() {\n"
            '    let normal = "escaped quote: \\" braces } {";\n'
            '    let bytes = b\"} {\";\n'
            '    let raw = r###\"} { /* not a comment */\"###;\n'
            "    let character = '}';\n"
            "    let byte_character = b'{';\n"
            "    // } line-comment brace\n"
            "    /* { outer /* } nested */ still outer } */\n"
            "    if true { let nested = || { 1 }; nested(); }\n"
            "}\n"
            "pub fn following_item() {}\n"
        )
        source.write_text(lexical_fixture, encoding="utf-8")
        _git(repo, "add", source.relative_to(repo).as_posix())
        _git(repo, "commit", "-q", "-m", "lexical seam")
        lexical_plan = {
            **plan,
            "base_sha": _git(repo, "rev-parse", "HEAD"),
        }
        lexical = verifier.specialist_source_context(
            repo, lexical_plan, "egress-security"
        )["entries"][0]
        test.true(
            "SOURCE CONTEXT Rust scanner stops at the balanced function brace",
            lexical["status"] == "included"
            and lexical["excerpt"].rstrip().endswith("nested(); }\n}")
            and "following_item" not in lexical["excerpt"],
        )

        oversized_body = (
            '"\\\\🙂\t'
            * (verifier.SPECIALIST_SOURCE_CONTEXT_BYTES // 2)
        )
        source.write_text(
            "fn make_provider_resolved() {\n"
            "    let consent = true;\n"
            f"    // {oversized_body}\n"
            "    active_sink();\n"
            "}",
            encoding="utf-8",
        )
        _git(repo, "add", source.relative_to(repo).as_posix())
        _git(repo, "commit", "-q", "-m", "oversized seam")
        oversized_sha = _git(repo, "rev-parse", "HEAD")
        synthetic_plan = {
            **plan,
            "base_sha": oversized_sha,
        }
        first = verifier.specialist_source_context(
            repo, synthetic_plan, "egress-security"
        )
        second = verifier.specialist_source_context(
            repo, synthetic_plan, "egress-security"
        )
        bounded = first["entries"][0]
        test.true(
            "SOURCE CONTEXT full serialized section is bounded and deterministic",
            bounded["truncated"]
            and len(
                verifier.specialist_source_context_section(first).encode(
                    "utf-8"
                )
            )
            <= verifier.SPECIALIST_SOURCE_CONTEXT_BYTES
            and first == second
            and "\ufffd" not in bounded["excerpt"],
        )

        contract = {
            "task_id": "source-context-selftest",
            "description": "bind unchanged trust seam source",
        }
        first_prompt = verifier.combined_review_prompt(
            contract,
            synthetic_plan,
            b"",
            [],
            "egress-security",
            repo,
            repo,
            policy_text="policy",
        )
        source.write_text(
            source.read_text(encoding="utf-8").replace(
                "let consent = true", "let consent = false"
            ),
            encoding="utf-8",
        )
        _git(repo, "add", source.relative_to(repo).as_posix())
        _git(repo, "commit", "-q", "-m", "change seam")
        changed_sha = _git(repo, "rev-parse", "HEAD")
        changed_source_plan = {
            **synthetic_plan,
            "base_sha": changed_sha,
        }
        second_prompt = verifier.combined_review_prompt(
            contract,
            changed_source_plan,
            b"",
            [],
            "egress-security",
            repo,
            repo,
            policy_text="policy",
        )
        test.true(
            "SOURCE CONTEXT bytes change the bound review prompt",
            hashlib.sha256(first_prompt.encode("utf-8")).hexdigest()
            != hashlib.sha256(second_prompt.encode("utf-8")).hexdigest(),
        )

        source.write_text(
            "fn unrelated() {}\n",
            encoding="utf-8",
        )
        _git(repo, "add", source.relative_to(repo).as_posix())
        _git(repo, "commit", "-q", "-m", "remove anchor")
        missing_plan = {
            **synthetic_plan,
            "base_sha": _git(repo, "rev-parse", "HEAD"),
        }
        missing = verifier.specialist_source_context(
            repo, missing_plan, "egress-security"
        )["entries"][0]
        test.equal(
            "SOURCE CONTEXT reports a missing anchor without invented evidence",
            (missing["status"], "excerpt" in missing),
            ("start_anchor_missing", False),
        )

        source.write_text("fn make_provider_resolved();\n", encoding="utf-8")
        _git(repo, "add", source.relative_to(repo).as_posix())
        _git(repo, "commit", "-q", "-m", "remove opening brace")
        missing_open_plan = {
            **synthetic_plan,
            "base_sha": _git(repo, "rev-parse", "HEAD"),
        }
        missing_open = verifier.specialist_source_context(
            repo, missing_open_plan, "egress-security"
        )["entries"][0]
        test.equal(
            "SOURCE CONTEXT reports a missing opening brace without proof",
            (missing_open["status"], "excerpt" in missing_open),
            ("opening_brace_missing", False),
        )

        source.write_text(
            'fn make_provider_resolved() { let decoy = "}";\n',
            encoding="utf-8",
        )
        _git(repo, "add", source.relative_to(repo).as_posix())
        _git(repo, "commit", "-q", "-m", "remove closing brace")
        missing_close_plan = {
            **synthetic_plan,
            "base_sha": _git(repo, "rev-parse", "HEAD"),
        }
        missing_close = verifier.specialist_source_context(
            repo, missing_close_plan, "egress-security"
        )["entries"][0]
        test.equal(
            "SOURCE CONTEXT reports a missing closing brace without partial proof",
            (missing_close["status"], "excerpt" in missing_close),
            ("closing_brace_missing", False),
        )


def probe_precedence_flow_cases(test: Tests) -> None:
    """Pin review-defect precedence through the real v2 verify transition."""

    with tempfile.TemporaryDirectory(prefix="murmur-v2-probe-precedence-") as raw:
        root = Path(raw)
        repo = root / "repo"
        _init_repo(repo)
        for relative in verifier.protocol_relative_paths(ROOT):
            source = ROOT / relative
            destination = repo / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, destination)
        owned_relative = "src/design-tokens/probe-precedence.scss"
        owned_base = repo / owned_relative
        owned_base.parent.mkdir(parents=True, exist_ok=True)
        owned_base.write_text("/* base */\n", encoding="utf-8")
        package_manifest = {
            "name": "probe-precedence-fixture",
            "version": "1.0.0",
            "scripts": {},
            "dependencies": {},
            "devDependencies": {},
        }
        package_lock = {
            "name": "probe-precedence-fixture",
            "version": "1.0.0",
            "lockfileVersion": 3,
            "requires": True,
            "packages": {
                "": {
                    "name": "probe-precedence-fixture",
                    "version": "1.0.0",
                    "dependencies": {},
                    "devDependencies": {},
                }
            },
        }
        runtime.atomic_write_json(repo / "package.json", package_manifest)
        runtime.atomic_write_json(repo / "package-lock.json", package_lock)
        _git(repo, "add", ".")
        _git(repo, "commit", "-q", "-m", "add exact v2 protocol")
        base = _git(repo, "rev-parse", "HEAD")
        worktree = root / "task" / "meetnotes"
        worktree.parent.mkdir()
        branch = "agent/v2/probe-precedence"
        _git(
            repo,
            "worktree",
            "add",
            "-q",
            "-b",
            branch,
            str(worktree),
            base,
        )
        common = Path(
            _git(
                repo,
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            )
        )
        task_id = "probe-precedence"
        task_dir = harness_cli.v2_task_dir(common, task_id)
        task_dir.mkdir(parents=True)
        contract: Dict[str, Any] = {
            "schema_version": 2,
            "task_id": task_id,
            "description": "prove review defects outrank repeated typed probes",
            "kind": "docs",
            "base_sha": base,
            "contract_sha256": "",
            "repo_realpath": str(repo.resolve()),
            "git_common_dir": str(common.resolve()),
            "worktree_path": str(worktree.resolve()),
            "branch": branch,
            "owned_paths": [owned_relative],
            "claims": [],
            "reviewer": "fake",
            "expected_change": True,
            "created_at": runtime.utc_now(),
        }
        contract["contract_sha256"] = verifier.document_hash(
            contract, "contract_sha256"
        )
        runtime.validate_schema(
            contract,
            runtime.load_schema("v2-task"),
            label="v2 probe precedence contract",
        )
        runtime.atomic_write_json(task_dir / "task.json", contract)
        runtime.atomic_write_json(
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
        owned = worktree / owned_relative
        owned.write_text(
            "/* base */\n/* probe precedence */\n", encoding="utf-8"
        )

        saved_verdict = os.environ.get("MURMUR_HARNESS_FAKE_REVIEW_VERDICT")
        saved_probe = os.environ.get(
            "MURMUR_HARNESS_FAKE_REVIEW_PROBE_ID"
        )
        saved_probe_rationale = os.environ.get(
            "MURMUR_HARNESS_FAKE_REVIEW_PROBE_RATIONALE"
        )
        saved_proof_gaps = os.environ.get(
            "MURMUR_HARNESS_FAKE_REVIEW_PROOF_GAPS_JSON"
        )
        original_load_config = runtime.load_config
        selftest_config = copy.deepcopy(original_load_config())
        selftest_config["canonical_checks"]["ng-lint"] = (
            "python3 -c 'print(\"PLANNED_\" + \"NG_LINT_STREAM\")'"
        )
        selftest_config["canonical_checks"]["ng-build"] = (
            "python3 -c 'print(\"PLANNED_\" + \"NG_BUILD_STREAM\")'"
        )
        selftest_config["canonical_checks"]["npm-lock"] = (
            "python3 -c 'print(\"PLANNED_\" + \"NPM_LOCK_STREAM\")'"
        )
        runtime.load_config = lambda: copy.deepcopy(selftest_config)
        os.environ["MURMUR_HARNESS_FAKE_REVIEW_VERDICT"] = "PASS"
        os.environ[
            "MURMUR_HARNESS_FAKE_REVIEW_PROBE_ID"
        ] = "ng-lint"
        try:
            with contextlib.redirect_stdout(io.StringIO()):
                collected = harness_cli.verify_task(
                    contract,
                    task_dir,
                    allow_test_adapter=True,
                )
            state = harness_cli.load_v2_state(task_dir)
            attempt_dir = task_dir / "attempts" / str(state["attempt_id"])
            plan = runtime.load_json(attempt_dir / "plan.json")
            probe_path = attempt_dir / "probes" / "ng-lint.json"
            probe_record = runtime.load_json(probe_path)
            probe_before = probe_path.read_bytes()
            probe_mtime_before = probe_path.stat().st_mtime_ns
            test.equal(
                "PROBE flow first review collects allowlisted evidence",
                collected,
                "NEEDS_EVIDENCE",
            )
            verifier.validate_probe_checkpoint(
                probe_record,
                verifier.canonical_check("ng-lint", runtime.load_config()),
                plan,
                task_dir,
                allow_test_adapter=True,
            )
            test.equal(
                "PROBE flow binds reviewer rationale to fresh execution",
                (
                    probe_record["source"],
                    probe_record["request_contexts"][0]["rationale"],
                ),
                (
                    "reviewer-probe",
                    "selftest-only typed probe state transition",
                ),
            )
            combined_context = copy.deepcopy(
                probe_record["request_contexts"][0]
            )
            combined_context["review_kind"] = "combined"
            combined_context["context_sha256"] = "f" * 64
            specialist_context = copy.deepcopy(combined_context)
            specialist_context["review_kind"] = "specialist"
            specialist_context["context_sha256"] = "0" * 64
            test.equal(
                "PROBE multi-review contexts use validator canonical order",
                [
                    item["review_kind"]
                    for item in verifier.canonical_probe_request_contexts(
                        [specialist_context, combined_context]
                    )
                ],
                ["combined", "specialist"],
            )
            check_records = [
                runtime.load_json(path)
                for path in sorted((attempt_dir / "checks").glob("*.json"))
            ]
            bound_prompt = verifier.combined_review_prompt(
                contract,
                plan,
                verifier.snapshot_scoped_diff(
                    worktree, contract, task_dir
                )[1],
                [*check_records, probe_record],
                "combined",
                worktree,
                task_dir,
                probes=[probe_record],
            )
            test.true(
                "PROBE resume prompt exposes exact command output and rationale",
                "PLANNED_NG_LINT_STREAM" in bound_prompt
                and "selftest-only typed probe state transition" in bound_prompt
                and json.dumps(probe_record["command"])[1:-1]
                in bound_prompt,
            )
            test.true(
                "PROBE every planned check exposes a bounded excerpt",
                "PLANNED_NG_BUILD_STREAM" in bound_prompt
                and '"excerpt_included": true' in bound_prompt,
            )
            forged_channel_checks = copy.deepcopy(check_records)
            for item in forged_channel_checks:
                if item["id"] == "ng-build":
                    item["source"] = "reviewer-probe"
            forged_channel_prompt = verifier.combined_review_prompt(
                contract,
                plan,
                verifier.snapshot_scoped_diff(
                    worktree, contract, task_dir
                )[1],
                forged_channel_checks,
                "combined",
                worktree,
                task_dir,
                probes=[probe_record],
            )
            forged_ng_build = next(
                item
                for item in forged_channel_checks
                if item["id"] == "ng-build"
            )
            test.equal(
                "PROBE record field cannot forge the stream channel",
                (
                    verifier._review_evidence_summary(
                        forged_ng_build,
                        task_dir,
                        channel="planned-check",
                        review_kind="combined",
                    )["source"],
                    "PLANNED_NG_BUILD_STREAM" in forged_channel_prompt,
                ),
                ("planned-check", True),
            )
            provenance_free = copy.deepcopy(probe_record)
            provenance_free.pop("source")
            provenance_free.pop("request_contexts")
            test.raises(
                "PROBE provenance-free legacy token fails closed",
                lambda: verifier.validate_probe_checkpoint(
                    provenance_free,
                    verifier.canonical_check(
                        "ng-lint", runtime.load_config()
                    ),
                    plan,
                    task_dir,
                    allow_test_adapter=True,
                ),
                "provenance",
            )
            corrupt_stream = copy.deepcopy(probe_record)
            corrupt_stream["evidence"]["stdout_sha256"] = "f" * 64
            test.raises(
                "PROBE changed output hash fails before fresh review",
                lambda: verifier.validate_probe_checkpoint(
                    corrupt_stream,
                    verifier.canonical_check(
                        "ng-lint", runtime.load_config()
                    ),
                    plan,
                    task_dir,
                    allow_test_adapter=True,
                ),
                "hash changed",
            )
            forged_source = copy.deepcopy(probe_record)
            forged_context = forged_source["request_contexts"][0]
            forged_context["review_prompt_sha256"] = "f" * 64
            forged_context["source_review"]["prompt_sha256"] = "f" * 64
            forged_context["context_sha256"] = verifier.document_hash(
                forged_context, "context_sha256"
            )
            test.raises(
                "PROBE recomputed context cannot substitute source review",
                lambda: verifier.validate_probe_checkpoint(
                    forged_source,
                    verifier.canonical_check(
                        "ng-lint", runtime.load_config()
                    ),
                    plan,
                    task_dir,
                    allow_test_adapter=True,
                ),
                "prompt hash changed",
            )

            legacy_probe_record = copy.deepcopy(probe_record)
            legacy_probe_record.pop("execution_number")
            runtime.atomic_write_json(probe_path, legacy_probe_record)
            verifier.validate_probe_checkpoint(
                legacy_probe_record,
                verifier.canonical_check(
                    "ng-lint", runtime.load_config()
                ),
                plan,
                task_dir,
                allow_test_adapter=True,
            )
            test.true(
                "PROBE pre-cap checkpoint remains resume-compatible",
                "execution_number" not in legacy_probe_record,
            )
            probe_before = probe_path.read_bytes()
            os.environ[
                "MURMUR_HARNESS_FAKE_REVIEW_PROBE_RATIONALE"
            ] = "changed rationale requires fresh empirical evidence"
            proof_gap_a = {
                "claim": "alpha",
                "evidence_missing": "alpha evidence",
                "how_to_prove": "run the planned probe",
            }
            proof_gap_b = {
                "claim": "beta",
                "evidence_missing": "beta evidence",
                "how_to_prove": "run the planned probe",
            }
            os.environ[
                "MURMUR_HARNESS_FAKE_REVIEW_PROOF_GAPS_JSON"
            ] = json.dumps([proof_gap_a, proof_gap_b])
            first_probe_execution = probe_record["evidence"]["execution_id"]
            with contextlib.redirect_stdout(io.StringIO()):
                evidence_only = harness_cli.verify_task(
                    contract,
                    task_dir,
                    allow_test_adapter=True,
                )
            test.equal(
                "PROBE flow changed request context stays evidence-only",
                evidence_only,
                "NEEDS_EVIDENCE",
            )
            refreshed_probe = runtime.load_json(probe_path)
            test.true(
                "PROBE rephrased request reuses one diff-bound execution",
                refreshed_probe["evidence"]["execution_id"]
                == first_probe_execution
                and refreshed_probe["request_contexts"][0]["rationale"]
                == "selftest-only typed probe state transition",
            )
            test.equal(
                "PROBE green execution remains single-shot",
                refreshed_probe["execution_number"],
                1,
            )
            test.equal(
                "PROBE execution event is append-only and numbered",
                [
                    event["execution_number"]
                    for event in [
                        json.loads(line)
                        for line in (task_dir / "events.jsonl").read_text(
                            encoding="utf-8"
                        ).splitlines()
                    ]
                    if event.get("event") == "probe-checkpoint"
                    and event.get("attempt_id") == attempt_dir.name
                    and event.get("probe_id") == "ng-lint"
                ],
                [1],
            )
            probe_before = probe_path.read_bytes()
            probe_mtime_before = probe_path.stat().st_mtime_ns

            os.environ[
                "MURMUR_HARNESS_FAKE_REVIEW_PROOF_GAPS_JSON"
            ] = json.dumps([proof_gap_b, proof_gap_a, proof_gap_a])
            with contextlib.redirect_stdout(io.StringIO()):
                repeated_request = harness_cli.verify_task(
                    contract,
                    task_dir,
                    allow_test_adapter=True,
                )
            test.equal(
                "PROBE repeated request stays evidence-only",
                repeated_request,
                "NEEDS_EVIDENCE",
            )
            test.true(
                "PROBE repeated request cannot create a rerun loop",
                probe_path.read_bytes() == probe_before
                and probe_path.stat().st_mtime_ns == probe_mtime_before,
            )
            os.environ[
                "MURMUR_HARNESS_FAKE_REVIEW_PROBE_RATIONALE"
            ] = "third semantic request must stop at the hard cap"
            with contextlib.redirect_stdout(io.StringIO()):
                capped_request = harness_cli.verify_task(
                    contract,
                    task_dir,
                    allow_test_adapter=True,
                )
            test.equal(
                "PROBE third rephrasing stops at NEEDS_EVIDENCE",
                capped_request,
                "NEEDS_EVIDENCE",
            )
            test.true(
                "PROBE single-shot rule prevents adversarial rationale loop",
                probe_path.read_bytes() == probe_before
                and probe_path.stat().st_mtime_ns == probe_mtime_before,
            )
            os.environ.pop(
                "MURMUR_HARNESS_FAKE_REVIEW_PROOF_GAPS_JSON", None
            )

            (attempt_dir / "reviews" / "combined.json").unlink()
            os.environ["MURMUR_HARNESS_FAKE_REVIEW_VERDICT"] = "FAIL"
            with contextlib.redirect_stdout(io.StringIO()):
                failed = harness_cli.verify_task(
                    contract,
                    task_dir,
                    allow_test_adapter=True,
                )
            test.equal(
                "PROBE flow FAIL plus seen probe becomes NEEDS_FIX",
                failed,
                "NEEDS_FIX",
            )
            test.equal(
                "PROBE flow persists fix state ahead of probe state",
                harness_cli.load_v2_state(task_dir)["status"],
                "NEEDS_FIX",
            )
            test.true(
                "PROBE flow FAIL plus seen probe executes nothing again",
                probe_path.read_bytes() == probe_before
                and probe_path.stat().st_mtime_ns == probe_mtime_before,
            )

            # A new exact diff without a probe request must still use the
            # original evidence/checkpoint path, including a bound evidence
            # document and the terminal complete phase.
            owned.write_text(
                "/* base */\n/* probe-free fix evidence path */\n",
                encoding="utf-8",
            )
            os.environ.pop("MURMUR_HARNESS_FAKE_REVIEW_PROBE_ID", None)
            with contextlib.redirect_stdout(io.StringIO()):
                probe_free_failed = harness_cli.verify_task(
                    contract,
                    task_dir,
                    allow_test_adapter=True,
                )
            probe_free_state = harness_cli.load_v2_state(task_dir)
            probe_free_evidence = Path(
                str(probe_free_state["evidence_path"])
            )
            test.equal(
                "PROBE-free FAIL preserves complete evidence transition",
                (
                    probe_free_failed,
                    probe_free_state["status"],
                    probe_free_state["phase"],
                    probe_free_state["reason"],
                    probe_free_evidence.is_file(),
                ),
                (
                    "NEEDS_FIX",
                    "NEEDS_FIX",
                    "complete",
                    "a review has unresolved FAIL/MAJOR/BLOCKER findings",
                    True,
                ),
            )

            # Reproduce the observed laundering attempt: a package/Angular
            # reviewer asks for the globally-known but unrelated config audit.
            # The result is rejected before the broker can execute it.
            owned.write_text(
                "/* base */\n/* reject unrelated config audit */\n",
                encoding="utf-8",
            )
            os.environ["MURMUR_HARNESS_FAKE_REVIEW_VERDICT"] = "PASS"
            os.environ[
                "MURMUR_HARNESS_FAKE_REVIEW_PROBE_ID"
            ] = "config-audit"
            with contextlib.redirect_stdout(io.StringIO()):
                unrelated = harness_cli.verify_task(
                    contract,
                    task_dir,
                    allow_test_adapter=True,
                )
            unrelated_state = harness_cli.load_v2_state(task_dir)
            unrelated_attempt = (
                task_dir / "attempts" / str(unrelated_state["attempt_id"])
            )
            test.equal(
                "PROBE unrelated config audit stays evidence-only",
                (unrelated, unrelated_state["status"]),
                ("NEEDS_EVIDENCE", "NEEDS_EVIDENCE"),
            )
            test.true(
                "PROBE unrelated config audit executes no green token",
                not (unrelated_attempt / "probes" / "config-audit.json").exists(),
            )
            unrelated_review_path = (
                unrelated_attempt / "reviews" / "combined.json"
            )
            test.equal(
                "PROBE fresh invalid request is durably checkpointed",
                (
                    unrelated_review_path.is_file(),
                    runtime.load_json(unrelated_review_path)["result"][
                        "probe_requests"
                    ][0]["probe_id"],
                    unrelated_state["reason"],
                ),
                (
                    True,
                    "config-audit",
                    (
                        "review requested probes outside the exact plan; "
                        "no command was executed: config-audit"
                    ),
                ),
            )

            # Faithfully replay the production incident through verify_task:
            # only package.json/package-lock.json change, the exact profile
            # schedules npm-lock + Angular checks, and a reviewer requests the
            # globally-known but plan-ineligible config-audit token.
            package_worktree = root / "package-task" / "meetnotes"
            package_worktree.parent.mkdir()
            package_branch = "agent/v2/package-probe-precedence"
            _git(
                repo,
                "worktree",
                "add",
                "-q",
                "-b",
                package_branch,
                str(package_worktree),
                base,
            )
            package_task_id = "package-probe-precedence"
            package_task_dir = harness_cli.v2_task_dir(
                common, package_task_id
            )
            package_task_dir.mkdir(parents=True)
            package_contract: Dict[str, Any] = {
                **contract,
                "task_id": package_task_id,
                "description": (
                    "replay package-only unrelated-probe laundering"
                ),
                "contract_sha256": "",
                "worktree_path": str(package_worktree.resolve()),
                "branch": package_branch,
                "owned_paths": ["package.json", "package-lock.json"],
                "created_at": runtime.utc_now(),
            }
            package_contract["contract_sha256"] = verifier.document_hash(
                package_contract, "contract_sha256"
            )
            runtime.validate_schema(
                package_contract,
                runtime.load_schema("v2-task"),
                label="v2 package probe precedence contract",
            )
            runtime.atomic_write_json(
                package_task_dir / "task.json", package_contract
            )
            runtime.atomic_write_json(
                package_task_dir / "runtime.json",
                {
                    "schema_version": 2,
                    "task_root": str(package_worktree.parent),
                    "shared_node_modules": None,
                    "server_worktree": None,
                    "server_source": str(root / "murmur-server"),
                    "server_revision": None,
                },
            )
            harness_cli.set_v2_state(
                package_task_dir, "OPEN", phase="open"
            )
            package_manifest["version"] = "1.0.1"
            package_lock["version"] = "1.0.1"
            package_lock["packages"][""]["version"] = "1.0.1"
            runtime.atomic_write_json(
                package_worktree / "package.json", package_manifest
            )
            runtime.atomic_write_json(
                package_worktree / "package-lock.json", package_lock
            )
            with contextlib.redirect_stdout(io.StringIO()):
                package_unrelated = harness_cli.verify_task(
                    package_contract,
                    package_task_dir,
                    allow_test_adapter=True,
                )
            package_state = harness_cli.load_v2_state(package_task_dir)
            package_attempt = (
                package_task_dir
                / "attempts"
                / str(package_state["attempt_id"])
            )
            package_plan = runtime.load_json(
                package_attempt / "plan.json"
            )
            test.equal(
                "PROBE package-only flow schedules exact dependency evidence",
                (
                    package_plan["changed_paths"],
                    [item["id"] for item in package_plan["checks"]],
                ),
                (
                    ["package-lock.json", "package.json"],
                    ["npm-lock", "ng-lint", "ng-build"],
                ),
            )
            npm_record = runtime.load_json(
                package_attempt / "checks" / "npm-lock.json"
            )
            package_check_records = [
                runtime.load_json(path)
                for path in sorted(
                    (package_attempt / "checks").glob("*.json")
                )
            ]
            package_prompt = verifier.combined_review_prompt(
                package_contract,
                package_plan,
                verifier.snapshot_scoped_diff(
                    package_worktree, package_contract, package_task_dir
                )[1],
                package_check_records,
                "combined",
                package_worktree,
                package_task_dir,
            )
            test.true(
                "PROBE every package check exposes bounded output",
                "PLANNED_NPM_LOCK_STREAM" in package_prompt
                and "PLANNED_NG_BUILD_STREAM" in package_prompt
                and "PLANNED_NG_LINT_STREAM" in package_prompt,
            )
            test.equal(
                "PROBE npm-lock execution binds the immutable task base",
                npm_record["evidence"]["bound_environment"],
                {"MURMUR_HARNESS_BASE_SHA": base},
            )
            tampered_npm_record = copy.deepcopy(npm_record)
            tampered_npm_record["evidence"]["bound_environment"][
                "MURMUR_HARNESS_BASE_SHA"
            ] = "f" * 40
            test.raises(
                "PROBE changed npm-lock base binding fails closed",
                lambda: verifier.validate_check_checkpoint(
                    tampered_npm_record,
                    package_plan["checks"][0],
                    package_plan,
                    package_task_dir,
                ),
                "runner-bound environment changed",
            )
            test.equal(
                "PROBE package-only config audit stays evidence-only",
                (package_unrelated, package_state["status"]),
                ("NEEDS_EVIDENCE", "NEEDS_EVIDENCE"),
            )
            test.true(
                "PROBE package-only flow executes no config-audit token",
                not (
                    package_attempt
                    / "probes"
                    / "config-audit.json"
                ).exists(),
            )
            package_invalid_review = (
                package_attempt / "reviews" / "combined.json"
            )
            test.equal(
                "PROBE package-only fresh invalid review remains auditable",
                (
                    package_invalid_review.is_file(),
                    runtime.load_json(package_invalid_review)["result"][
                        "probe_requests"
                    ][0]["probe_id"],
                    package_state["reason"],
                ),
                (
                    True,
                    "config-audit",
                    (
                        "review requested probes outside the exact plan; "
                        "no command was executed: config-audit"
                    ),
                ),
            )

            # A retryable probe checkpoint is excluded from green reviewer
            # evidence, but it must still consume the same per-ID execution
            # budget. Otherwise every resume forgets the timeout/BLOCKED
            # attempt and loops forever at execution_number=1.
            owned.write_text(
                "/* base */\n/* retryable probe cap */\n",
                encoding="utf-8",
            )
            os.environ["MURMUR_HARNESS_FAKE_REVIEW_VERDICT"] = "PASS"
            os.environ[
                "MURMUR_HARNESS_FAKE_REVIEW_PROBE_ID"
            ] = "ng-lint"
            os.environ[
                "MURMUR_HARNESS_FAKE_REVIEW_PROBE_RATIONALE"
            ] = "retryable probe must consume its bounded budget"
            original_run_check = runtime.run_check
            retryable_probe_runs = 0

            def retryable_probe_run_check(
                check_worktree: Path,
                check_task_dir: Path,
                check: Mapping[str, Any],
                phase: str,
                *,
                bound_environment: Optional[
                    Mapping[str, str]
                ] = None,
            ) -> Dict[str, Any]:
                nonlocal retryable_probe_runs
                evidence = original_run_check(
                    check_worktree,
                    check_task_dir,
                    check,
                    phase,
                    bound_environment=bound_environment,
                )
                if phase.endswith("-probe"):
                    retryable_probe_runs += 1
                    return {
                        **evidence,
                        "passed": False,
                        "outcome": "BLOCKED",
                        "timed_out": True,
                        "blocked_reason": "synthetic retryable probe",
                    }
                return evidence

            runtime.run_check = retryable_probe_run_check
            try:
                with contextlib.redirect_stdout(io.StringIO()):
                    retryable_first = harness_cli.verify_task(
                        contract,
                        task_dir,
                        allow_test_adapter=True,
                    )
                retryable_first_state = harness_cli.load_v2_state(task_dir)
                retryable_attempt = (
                    task_dir
                    / "attempts"
                    / str(retryable_first_state["attempt_id"])
                )
                retryable_projection = (
                    retryable_attempt / "probes" / "ng-lint.json"
                )
                rolled_back_projection = runtime.load_json(
                    retryable_projection
                )
                (
                    retryable_attempt / "reviews" / "combined.json"
                ).unlink()
                os.environ.pop(
                    "MURMUR_HARNESS_FAKE_REVIEW_PROBE_ID", None
                )
                with contextlib.redirect_stdout(io.StringIO()):
                    retryable_second = harness_cli.verify_task(
                        contract,
                        task_dir,
                        allow_test_adapter=True,
                    )
                os.environ[
                    "MURMUR_HARNESS_FAKE_REVIEW_PROBE_ID"
                ] = "ng-lint"
                # Reproduce the replacement bypass: restore execution 1 and
                # remove its legacy-optional number after execution 2 exists.
                # The append-only event must repair the projection and refuse
                # a third process.
                rolled_back_projection.pop("execution_number")
                runtime.atomic_write_json(
                    retryable_projection, rolled_back_projection
                )
                with contextlib.redirect_stdout(io.StringIO()):
                    retryable_capped = harness_cli.verify_task(
                        contract,
                        task_dir,
                        allow_test_adapter=True,
                    )
                numberless_latest = runtime.load_json(
                    retryable_projection
                )
                numberless_latest.pop("execution_number")
                runtime.atomic_write_json(
                    retryable_projection, numberless_latest
                )
                with contextlib.redirect_stdout(io.StringIO()):
                    retryable_capped_again = harness_cli.verify_task(
                        contract,
                        task_dir,
                        allow_test_adapter=True,
                    )
            finally:
                runtime.run_check = original_run_check
            retryable_state = harness_cli.load_v2_state(task_dir)
            retryable_attempt = (
                task_dir
                / "attempts"
                / str(retryable_state["attempt_id"])
            )
            retryable_record = runtime.load_json(
                retryable_attempt / "probes" / "ng-lint.json"
            )
            retryable_events = []
            for line in (task_dir / "events.jsonl").read_text(
                encoding="utf-8"
            ).splitlines():
                event = json.loads(line)
                if (
                    event.get("event") == "probe-checkpoint"
                    and event.get("attempt_id") == retryable_attempt.name
                    and event.get("probe_id") == "ng-lint"
                    and event.get("execution_number") is not None
                ):
                    retryable_events.append(event)
            test.equal(
                "PROBE retryable outcomes consume two bounded executions",
                (
                    retryable_first,
                    retryable_second,
                    retryable_probe_runs,
                    retryable_record["execution_number"],
                    [
                        event["execution_number"]
                        for event in retryable_events
                    ],
                ),
                (
                    "PAUSED_RETRYABLE",
                    "PAUSED_RETRYABLE",
                    2,
                    2,
                    [1, 2],
                ),
            )
            test.equal(
                "PROBE rollback and number deletion cannot reset retry budget",
                (
                    retryable_capped,
                    retryable_capped_again,
                    retryable_probe_runs,
                    retryable_state["status"],
                    retryable_record["execution_number"],
                ),
                (
                    "NEEDS_EVIDENCE",
                    "NEEDS_EVIDENCE",
                    2,
                    "NEEDS_EVIDENCE",
                    2,
                ),
            )
            retryable_projection.unlink()
            runtime.run_check = retryable_probe_run_check
            try:
                with contextlib.redirect_stdout(io.StringIO()):
                    retryable_without_projection = (
                        harness_cli.verify_task(
                            contract,
                            task_dir,
                            allow_test_adapter=True,
                        )
                    )
            finally:
                runtime.run_check = original_run_check
            test.equal(
                "PROBE deleting latest projection cannot lower event high-water",
                (
                    retryable_without_projection,
                    retryable_probe_runs,
                    harness_cli.load_v2_state(task_dir)["status"],
                    runtime.load_json(retryable_projection)[
                        "execution_number"
                    ],
                ),
                ("NEEDS_EVIDENCE", 2, "NEEDS_EVIDENCE", 2),
            )

            # Reserve the bounded slot before starting the command. An
            # exception after the real process exits but before its result can
            # be checkpointed must not permit unbounded re-execution.
            owned.write_text(
                "/* base */\n/* probe pre-run reservation */\n",
                encoding="utf-8",
            )
            post_probe_crashes = 0

            def crash_after_probe_run(
                check_worktree: Path,
                check_task_dir: Path,
                check: Mapping[str, Any],
                phase: str,
                *,
                bound_environment: Optional[
                    Mapping[str, str]
                ] = None,
            ) -> Dict[str, Any]:
                nonlocal post_probe_crashes
                evidence = original_run_check(
                    check_worktree,
                    check_task_dir,
                    check,
                    phase,
                    bound_environment=bound_environment,
                )
                if phase.endswith("-probe"):
                    post_probe_crashes += 1
                    raise runtime.HarnessError(
                        "synthetic post-probe checkpoint crash"
                    )
                return evidence

            def verify_after_probe_crash() -> str:
                with contextlib.redirect_stdout(io.StringIO()):
                    return harness_cli.verify_task(
                        contract,
                        task_dir,
                        allow_test_adapter=True,
                    )

            runtime.run_check = crash_after_probe_run
            try:
                test.raises(
                    "PROBE first post-process crash consumes reservation one",
                    verify_after_probe_crash,
                    "synthetic post-probe checkpoint crash",
                )
                crash_attempt = str(
                    harness_cli.load_v2_state(task_dir)["attempt_id"]
                )
                (
                    task_dir
                    / "attempts"
                    / crash_attempt
                    / "reviews"
                    / "combined.json"
                ).unlink()
                os.environ.pop(
                    "MURMUR_HARNESS_FAKE_REVIEW_PROBE_ID", None
                )
                test.raises(
                    "PROBE prior request survives missing fresh review request",
                    verify_after_probe_crash,
                    "synthetic post-probe checkpoint crash",
                )
                with contextlib.redirect_stdout(io.StringIO()):
                    crash_capped = harness_cli.verify_task(
                        contract,
                        task_dir,
                        allow_test_adapter=True,
                    )
            finally:
                runtime.run_check = original_run_check
            crash_reservations = []
            for line in (task_dir / "events.jsonl").read_text(
                encoding="utf-8"
            ).splitlines():
                event = json.loads(line)
                if (
                    event.get("event") == "probe-execution-reserved"
                    and event.get("attempt_id") == crash_attempt
                    and event.get("probe_id") == "ng-lint"
                ):
                    crash_reservations.append(event["execution_number"])
            test.equal(
                "PROBE interrupted executions stop at the same hard cap",
                (
                    crash_capped,
                    post_probe_crashes,
                    crash_reservations,
                    harness_cli.load_v2_state(task_dir)["status"],
                ),
                ("NEEDS_EVIDENCE", 2, [1, 2], "NEEDS_EVIDENCE"),
            )

            # The state projection witnesses the high-water. Removing the
            # latest reservation from the ledger while leaving its state event
            # must be detected as a rewind, not interpreted as free budget.
            full_event_documents = [
                json.loads(line)
                for line in (task_dir / "events.jsonl").read_text(
                    encoding="utf-8"
                ).splitlines()
            ]
            truncate_at = next(
                index
                for index, event in enumerate(full_event_documents)
                if (
                    event.get("event") == "probe-execution-reserved"
                    and event.get("attempt_id") == crash_attempt
                    and event.get("probe_id") == "ng-lint"
                    and event.get("execution_number") == 2
                )
            )
            event_documents = full_event_documents[:truncate_at]
            runtime.atomic_write_bytes(
                task_dir / "events.jsonl",
                b"".join(
                    runtime.canonical_json(event) + b"\n"
                    for event in event_documents
                ),
            )
            test.raises(
                "PROBE witnessed event-tail rewind fails closed",
                verify_after_probe_crash,
                "newer than",
            )
            test.equal(
                "PROBE witnessed rewind starts no third process",
                post_probe_crashes,
                2,
            )
            runtime.atomic_write_bytes(
                task_dir / "events.jsonl",
                b"".join(
                    runtime.canonical_json(event) + b"\n"
                    for event in full_event_documents
                ),
            )

            # A deterministic failed probe remains NEEDS_FIX on every resume;
            # it can never be omitted from aggregate evidence and briefly
            # laundered into PASSED.
            owned.write_text(
                "/* base */\n/* deterministic probe failure */\n",
                encoding="utf-8",
            )
            os.environ[
                "MURMUR_HARNESS_FAKE_REVIEW_PROBE_ID"
            ] = "ng-lint"
            deterministic_probe_runs = 0

            def deterministic_probe_failure(
                check_worktree: Path,
                check_task_dir: Path,
                check: Mapping[str, Any],
                phase: str,
                *,
                bound_environment: Optional[
                    Mapping[str, str]
                ] = None,
            ) -> Dict[str, Any]:
                nonlocal deterministic_probe_runs
                evidence = original_run_check(
                    check_worktree,
                    check_task_dir,
                    check,
                    phase,
                    bound_environment=bound_environment,
                )
                if phase.endswith("-probe"):
                    deterministic_probe_runs += 1
                    return {
                        **evidence,
                        "passed": False,
                        "outcome": "FAIL",
                        "exit_code": 1,
                        "blocked_reason": "synthetic deterministic failure",
                    }
                return evidence

            runtime.run_check = deterministic_probe_failure
            try:
                with contextlib.redirect_stdout(io.StringIO()):
                    deterministic_first = harness_cli.verify_task(
                        contract,
                        task_dir,
                        allow_test_adapter=True,
                    )
            finally:
                runtime.run_check = original_run_check
            os.environ.pop("MURMUR_HARNESS_FAKE_REVIEW_PROBE_ID", None)
            with contextlib.redirect_stdout(io.StringIO()):
                deterministic_resume = harness_cli.verify_task(
                    contract,
                    task_dir,
                    allow_test_adapter=True,
                )
            test.equal(
                "PROBE deterministic failure cannot degrade to evidence-only",
                (
                    deterministic_first,
                    deterministic_resume,
                    deterministic_probe_runs,
                    harness_cli.load_v2_state(task_dir)["status"],
                ),
                ("NEEDS_FIX", "NEEDS_FIX", 1, "NEEDS_FIX"),
            )

            # Upgrade compatibility: an old numberless event proves that a
            # process already ran. If its mutable projection disappeared, the
            # new runner must fail closed rather than inventing slot zero.
            owned.write_text(
                "/* base */\n/* legacy probe event */\n",
                encoding="utf-8",
            )
            os.environ[
                "MURMUR_HARNESS_FAKE_REVIEW_PROBE_ID"
            ] = "ng-lint"
            with contextlib.redirect_stdout(io.StringIO()):
                legacy_collected = harness_cli.verify_task(
                    contract,
                    task_dir,
                    allow_test_adapter=True,
                )
            legacy_state = harness_cli.load_v2_state(task_dir)
            legacy_attempt = str(legacy_state["attempt_id"])
            legacy_projection = (
                task_dir
                / "attempts"
                / legacy_attempt
                / "probes"
                / "ng-lint.json"
            )
            rewritten_events = []
            last_state_document: Optional[Dict[str, Any]] = None
            for line in (task_dir / "events.jsonl").read_text(
                encoding="utf-8"
            ).splitlines():
                event = json.loads(line)
                if event.get("event") == "state":
                    event["state"].pop("probe_high_water", None)
                    last_state_document = event["state"]
                if (
                    event.get("attempt_id") == legacy_attempt
                    and event.get("probe_id") == "ng-lint"
                    and event.get("event")
                    == "probe-execution-reserved"
                ):
                    continue
                if (
                    event.get("attempt_id") == legacy_attempt
                    and event.get("probe_id") == "ng-lint"
                    and event.get("event") == "probe-checkpoint"
                ):
                    event = {
                        "at": event["at"],
                        "event": "probe-checkpoint",
                        "attempt_id": legacy_attempt,
                        "probe_id": "ng-lint",
                        "record_path": str(legacy_projection),
                        "passed": True,
                    }
                rewritten_events.append(event)
            runtime.atomic_write_bytes(
                task_dir / "events.jsonl",
                b"".join(
                    runtime.canonical_json(event) + b"\n"
                    for event in rewritten_events
                ),
            )
            if last_state_document is None:
                raise AssertionError("missing state fixture")
            runtime.atomic_write_json(
                task_dir / "state.json", last_state_document
            )
            unexpected_legacy_probe_runs = 0

            def count_unexpected_legacy_probe(
                check_worktree: Path,
                check_task_dir: Path,
                check: Mapping[str, Any],
                phase: str,
                *,
                bound_environment: Optional[
                    Mapping[str, str]
                ] = None,
            ) -> Dict[str, Any]:
                nonlocal unexpected_legacy_probe_runs
                if phase.endswith("-probe"):
                    unexpected_legacy_probe_runs += 1
                return original_run_check(
                    check_worktree,
                    check_task_dir,
                    check,
                    phase,
                    bound_environment=bound_environment,
                )

            legacy_events_snapshot = (
                task_dir / "events.jsonl"
            ).read_bytes()
            legacy_state_snapshot = (task_dir / "state.json").read_bytes()
            legacy_projection_snapshot = legacy_projection.read_bytes()
            original_reserve_probe = (
                harness_cli._reserve_probe_execution
            )
            migration_reservations = 0

            def crash_after_first_migration_reservation(
                *args: Any, **kwargs: Any
            ) -> None:
                nonlocal migration_reservations
                original_reserve_probe(*args, **kwargs)
                migration_reservations += 1
                if migration_reservations == 1:
                    raise runtime.HarnessError(
                        "synthetic migration reservation crash"
                    )

            runtime.run_check = count_unexpected_legacy_probe
            try:
                harness_cli._reserve_probe_execution = (
                    crash_after_first_migration_reservation
                )
                test.raises(
                    "PROBE legacy migration crash preserves reservation",
                    verify_after_probe_crash,
                    "synthetic migration reservation crash",
                )
                harness_cli._reserve_probe_execution = (
                    original_reserve_probe
                )
                with contextlib.redirect_stdout(io.StringIO()):
                    migrated_resume = harness_cli.verify_task(
                        contract,
                        task_dir,
                        allow_test_adapter=True,
                    )
            finally:
                harness_cli._reserve_probe_execution = (
                    original_reserve_probe
                )
                runtime.run_check = original_run_check
            test.equal(
                "PROBE legacy migration resumes without a second process",
                (
                    migrated_resume,
                    migration_reservations,
                    unexpected_legacy_probe_runs,
                ),
                ("NEEDS_EVIDENCE", 1, 0),
            )

            runtime.atomic_write_bytes(
                task_dir / "events.jsonl", legacy_events_snapshot
            )
            runtime.atomic_write_bytes(
                task_dir / "state.json", legacy_state_snapshot
            )
            runtime.atomic_write_bytes(
                legacy_projection, legacy_projection_snapshot
            )
            ambiguous_events = [
                json.loads(line)
                for line in legacy_events_snapshot.decode(
                    "utf-8"
                ).splitlines()
            ]
            ambiguous_events.append(
                {
                    "at": runtime.utc_now(),
                    "event": "probe-checkpoint",
                    "attempt_id": legacy_attempt,
                    "probe_id": "ng-lint",
                    "record_path": str(legacy_projection),
                    "passed": False,
                }
            )
            runtime.atomic_write_bytes(
                task_dir / "events.jsonl",
                b"".join(
                    runtime.canonical_json(event) + b"\n"
                    for event in ambiguous_events
                ),
            )
            runtime.run_check = count_unexpected_legacy_probe
            try:
                test.raises(
                    "PROBE ambiguous multi-event legacy history fails closed",
                    verify_after_probe_crash,
                    "multiple legacy probe events cannot be migrated safely",
                )
            finally:
                runtime.run_check = original_run_check
            test.equal(
                "PROBE failed legacy tail cannot be replaced by green projection",
                unexpected_legacy_probe_runs,
                0,
            )

            runtime.atomic_write_bytes(
                task_dir / "events.jsonl", legacy_events_snapshot
            )
            runtime.atomic_write_bytes(
                task_dir / "state.json", legacy_state_snapshot
            )
            runtime.atomic_write_bytes(
                legacy_projection, legacy_projection_snapshot
            )
            legacy_projection.unlink()
            runtime.run_check = count_unexpected_legacy_probe
            try:
                test.raises(
                    "PROBE numberless event without projection fails closed",
                    verify_after_probe_crash,
                    "legacy probe event has no recoverable projection",
                )
            finally:
                runtime.run_check = original_run_check
            test.equal(
                "PROBE legacy missing projection never resets to slot zero",
                (
                    legacy_collected,
                    unexpected_legacy_probe_runs,
                    harness_cli.load_v2_state(task_dir)["status"],
                ),
                ("NEEDS_EVIDENCE", 0, "NEEDS_EVIDENCE"),
            )

            # Essential closure path: the source review that requested a probe
            # remains immutable while a fresh review consumes that evidence
            # and advances the per-kind checkpoint to PASS.
            closure_worktree = root / "closure-task" / "meetnotes"
            closure_worktree.parent.mkdir()
            closure_branch = "agent/v2/probe-closure"
            _git(
                repo,
                "worktree",
                "add",
                "-q",
                "-b",
                closure_branch,
                str(closure_worktree),
                base,
            )
            closure_task_id = "probe-closure"
            closure_task_dir = harness_cli.v2_task_dir(
                common, closure_task_id
            )
            closure_task_dir.mkdir(parents=True)
            closure_contract = {
                **contract,
                "task_id": closure_task_id,
                "description": (
                    "prove request to probe to fresh acceptance closure"
                ),
                "contract_sha256": "",
                "worktree_path": str(closure_worktree.resolve()),
                "branch": closure_branch,
                "created_at": runtime.utc_now(),
            }
            closure_contract["contract_sha256"] = verifier.document_hash(
                closure_contract, "contract_sha256"
            )
            runtime.validate_schema(
                closure_contract,
                runtime.load_schema("v2-task"),
                label="v2 probe closure contract",
            )
            runtime.atomic_write_json(
                closure_task_dir / "task.json", closure_contract
            )
            runtime.atomic_write_json(
                closure_task_dir / "runtime.json",
                {
                    "schema_version": 2,
                    "task_root": str(closure_worktree.parent),
                    "shared_node_modules": None,
                    "server_worktree": None,
                    "server_source": str(root / "murmur-server"),
                    "server_revision": None,
                },
            )
            harness_cli.set_v2_state(
                closure_task_dir, "OPEN", phase="open"
            )
            (
                closure_worktree / owned_relative
            ).write_text(
                "/* base */\n/* probe closure */\n",
                encoding="utf-8",
            )
            os.environ["MURMUR_HARNESS_FAKE_REVIEW_VERDICT"] = "PASS"
            os.environ[
                "MURMUR_HARNESS_FAKE_REVIEW_PROBE_ID"
            ] = "ng-lint"
            os.environ.pop(
                "MURMUR_HARNESS_FAKE_REVIEW_PROOF_GAPS_JSON", None
            )
            os.environ.pop(
                "MURMUR_HARNESS_FAKE_REVIEW_PROBE_RATIONALE", None
            )
            with contextlib.redirect_stdout(io.StringIO()):
                closure_collected = harness_cli.verify_task(
                    closure_contract,
                    closure_task_dir,
                    allow_test_adapter=True,
                )
            closure_state = harness_cli.load_v2_state(
                closure_task_dir
            )
            closure_attempt = (
                closure_task_dir
                / "attempts"
                / str(closure_state["attempt_id"])
            )
            closure_probe = runtime.load_json(
                closure_attempt / "probes" / "ng-lint.json"
            )
            source_result_path = Path(
                str(
                    closure_probe["request_contexts"][0][
                        "review_result_path"
                    ]
                )
            )
            source_result_bytes = source_result_path.read_bytes()
            source_result_sha256 = runtime.sha256_file(
                source_result_path
            )
            os.environ.pop(
                "MURMUR_HARNESS_FAKE_REVIEW_PROBE_ID", None
            )
            with contextlib.redirect_stdout(io.StringIO()):
                closure_passed = harness_cli.verify_task(
                    closure_contract,
                    closure_task_dir,
                    allow_test_adapter=True,
                )
            closure_final_state = harness_cli.load_v2_state(
                closure_task_dir
            )
            closure_final_review = runtime.load_json(
                closure_attempt / "reviews" / "combined.json"
            )
            verified_closure = verifier.verify_v2_evidence(
                closure_contract,
                closure_task_dir,
                allow_test_adapter=True,
            )
            test.equal(
                "PROBE closure reaches exact verified PASS",
                (
                    closure_collected,
                    closure_passed,
                    closure_final_state["status"],
                    verified_closure["verdict"],
                ),
                (
                    "NEEDS_EVIDENCE",
                    "PASSED",
                    "PASSED",
                    "PASSED",
                ),
            )
            test.true(
                "PROBE source review artifact stays immutable across fresh PASS",
                closure_final_review["result_path"]
                != str(source_result_path)
                and source_result_path.read_bytes() == source_result_bytes
                and runtime.sha256_file(source_result_path)
                == source_result_sha256
                and closure_probe["request_contexts"][0][
                    "review_result_sha256"
                ]
                == source_result_sha256,
            )
        finally:
            runtime.load_config = original_load_config
            if saved_verdict is None:
                os.environ.pop("MURMUR_HARNESS_FAKE_REVIEW_VERDICT", None)
            else:
                os.environ["MURMUR_HARNESS_FAKE_REVIEW_VERDICT"] = saved_verdict
            if saved_probe is None:
                os.environ.pop("MURMUR_HARNESS_FAKE_REVIEW_PROBE_ID", None)
            else:
                os.environ[
                    "MURMUR_HARNESS_FAKE_REVIEW_PROBE_ID"
                ] = saved_probe
            if saved_probe_rationale is None:
                os.environ.pop(
                    "MURMUR_HARNESS_FAKE_REVIEW_PROBE_RATIONALE", None
                )
            else:
                os.environ[
                    "MURMUR_HARNESS_FAKE_REVIEW_PROBE_RATIONALE"
                ] = saved_probe_rationale
            if saved_proof_gaps is None:
                os.environ.pop(
                    "MURMUR_HARNESS_FAKE_REVIEW_PROOF_GAPS_JSON", None
                )
            else:
                os.environ[
                    "MURMUR_HARNESS_FAKE_REVIEW_PROOF_GAPS_JSON"
                ] = saved_proof_gaps


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
    while runtime._pid_is_alive(pid) and time.monotonic() < deadline:
        time.sleep(0.05)
    return not runtime._pid_is_alive(pid)


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
        leaked = runtime.run_logged_process(
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
            "import runtime;"
            f"runtime.run_logged_process([sys.executable,'-c',{parent_kill_managed!r}],"
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
                    runtime.load_json(guardian_results[0])
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
        first_logged = runtime.run_logged_process(
            [sys.executable, "-c", "print('first')"],
            cwd=root,
            timeout_seconds=5,
            log_path=log_base,
        )
        first_logged_bytes = {
            key: Path(str(first_logged[key])).read_bytes()
            for key in ("log", "guardian_path")
        }
        second_logged = runtime.run_logged_process(
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
        runtime.atomic_write_json(
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
        background_check = runtime.run_check(
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
        first_check = runtime.run_check(repo, task_dir, declared, "v2-resume")
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
        second_check = runtime.run_check(repo, task_dir, declared, "v2-resume")
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
        first_model = runtime.invoke_model(
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
        second_model = runtime.invoke_model(
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
        original_load_config = runtime.load_config
        base_config = original_load_config()

        def timeout_config() -> Dict[str, Any]:
            return {
                **base_config,
                "reviewer_timeout_seconds": 1,
                "review_retry_max_delay_seconds": 0,
            }

        runtime.load_config = timeout_config
        os.environ["PATH"] = str(fake_bin) + os.pathsep + (original_path or "")
        try:
            record = verifier.invoke_readonly_review(
                contract=contract,
                plan=plan,
                worktree=repo,
                task_dir=root,
                attempt_dir=attempt_dir,
                diff=b"diff --git a/owned.txt b/owned.txt\n",
                checks=[],
                review={"kind": "combined", "vendor": "claude"},
                probe_evidence_sha256=verifier.probe_evidence_hash([]),
                sleep=lambda _delay: None,
            )
        finally:
            runtime.load_config = original_load_config
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
        background_process_result = runtime.run_logged_process(
            [sys.executable, "-c", background_leader],
            cwd=root,
            timeout_seconds=5,
            log_path=root / "model-background.log",
            term_grace_seconds=0.1,
        )
        original_run_logged_process = runtime.run_logged_process
        runtime.run_logged_process = (
            lambda *_args, **_kwargs: dict(background_process_result)
        )
        os.environ["PATH"] = str(fake_bin) + os.pathsep + (original_path or "")
        try:
            test.raises(
                "GUARDIAN model cannot accept structured PASS with background descendant",
                lambda: runtime.invoke_model(
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
            runtime.run_logged_process = original_run_logged_process
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
            runtime.load_json(task_dir / "state.json"),
            recovered,
        )
        stale_projection = {
            **recovered,
            "status": "VERIFYING",
            "updated_at": "2000-01-01T00:00:00Z",
        }
        stale_projection.pop("state_revision")
        runtime.atomic_write_json(task_dir / "state.json", stale_projection)
        test.equal(
            "STATE stale divergent projection is repaired from ledger",
            harness_cli.load_v2_state(task_dir),
            recovered,
        )
        future_projection = {
            **recovered,
            "updated_at": "2999-01-01T00:00:00Z",
            "state_revision": recovered["state_revision"] + 1,
        }
        runtime.atomic_write_json(task_dir / "state.json", future_projection)
        test.raises(
            "STATE projection newer than ledger fails closed",
            lambda: harness_cli.load_v2_state(task_dir),
            "newer than",
        )
        same_revision_projection = {
            **recovered,
            "status": "VERIFYING",
        }
        runtime.atomic_write_json(
            task_dir / "state.json", same_revision_projection
        )
        test.raises(
            "STATE equal timestamp divergence fails closed by revision",
            lambda: harness_cli.load_v2_state(task_dir),
            "same revision",
        )
        runtime.atomic_write_json(task_dir / "state.json", recovered)

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
        primary_root = runtime.shared_resource_root_for_task(primary_task)
        driver_root = runtime.shared_resource_root_for_task(driver_task)
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
            "import runtime;"
            "lease=runtime.acquire_cargo_lane("
            "pathlib.Path(sys.argv[1]),2,command='standalone-driver-holder');"
            "print('ready',flush=True);"
            "marker=pathlib.Path(sys.argv[2]);deadline=time.monotonic()+10;"
            "\nwhile not marker.exists() and time.monotonic()<deadline:"
            "\n time.sleep(0.01)"
            "\n"
            "runtime.release_cargo_lane(lease)"
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
            test.true(
                "LANE visible wait reports FIFO position and enqueue time",
                "queue_position=1/1" in blocked.stderr
                and "queued_since=" in blocked.stderr,
            )
            test.equal(
                "LANE timed-out waiter removes its FIFO ticket",
                list(
                    (
                        driver_root
                        / runtime.resource_lane.QUEUE_DIRECTORY
                    ).glob(
                        f"*{runtime.resource_lane.TICKET_SUFFIX}"
                    )
                ),
                [],
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


def fifo_lane_cases(test: Tests) -> None:
    """Mixed lane clients must enter in ticket order and reap dead waiters."""

    with tempfile.TemporaryDirectory(prefix="murmur-v2-fifo-lane-") as raw:
        root = Path(raw)
        repo = root / "meetnotes"
        _init_repo(repo)
        common = Path(
            _git(
                repo,
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            )
        )
        task_dirs: Dict[str, Path] = {}
        for label in ("holder", "older", "newer"):
            task_dir = (
                common / "agent-harness" / "v2" / "tasks" / label
            )
            task_dir.mkdir(parents=True)
            task_dirs[label] = task_dir
        resource_root = runtime.shared_resource_root_for_task(
            task_dirs["holder"]
        )
        queue_root = (
            resource_root / runtime.resource_lane.QUEUE_DIRECTORY
        )
        runner = ROOT / "scripts" / "agent-resource-run"
        library = str(Path(__file__).resolve().parent)

        holder_code = (
            "import pathlib,sys,time;"
            f"sys.path.insert(0,{library!r});"
            "import runtime;"
            "lease=runtime.acquire_cargo_lane("
            "pathlib.Path(sys.argv[1]),5,command='fifo-holder');"
            "print('ready',flush=True);"
            "marker=pathlib.Path(sys.argv[2]);deadline=time.monotonic()+10;"
            "\nwhile not marker.exists() and time.monotonic()<deadline:"
            "\n time.sleep(0.01)"
            "\nruntime.release_cargo_lane(lease)"
        )
        task_waiter_code = (
            "import os,pathlib,sys,time;"
            f"sys.path.insert(0,{library!r});"
            "import runtime;"
            "lease=runtime.acquire_cargo_lane("
            "pathlib.Path(sys.argv[1]),5,command='fifo-'+sys.argv[3]);"
            "fd=os.open(sys.argv[2],os.O_WRONLY|os.O_CREAT|os.O_APPEND,0o600);"
            "os.write(fd,(sys.argv[3]+'\\n').encode());os.close(fd);"
            "time.sleep(0.05);runtime.release_cargo_lane(lease)"
        )
        append_code = (
            "import os,sys,time;"
            "fd=os.open(sys.argv[1],os.O_WRONLY|os.O_CREAT|os.O_APPEND,0o600);"
            "os.write(fd,(sys.argv[2]+'\\n').encode());os.close(fd);"
            "time.sleep(0.05)"
        )

        def queue_depth() -> int:
            if not queue_root.is_dir():
                return 0
            return len(
                list(
                    queue_root.glob(
                        f"*{runtime.resource_lane.TICKET_SUFFIX}"
                    )
                )
            )

        def wait_for_depth(expected: int) -> bool:
            deadline = time.monotonic() + 3
            while time.monotonic() < deadline:
                if queue_depth() == expected:
                    return True
                time.sleep(0.01)
            return queue_depth() == expected

        def resource_waiter(order: Path, label: str) -> subprocess.Popen[str]:
            environment = os.environ.copy()
            environment["MURMUR_HARNESS_TASK"] = f"fifo-{label}"
            return subprocess.Popen(
                [
                    str(runner),
                    "--deadline-seconds",
                    "5",
                    "--",
                    sys.executable,
                    "-c",
                    append_code,
                    str(order),
                    label,
                ],
                cwd=str(repo),
                env=environment,
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )

        def task_waiter(order: Path, label: str) -> subprocess.Popen[str]:
            return subprocess.Popen(
                [
                    sys.executable,
                    "-c",
                    task_waiter_code,
                    str(task_dirs[label]),
                    str(order),
                    label,
                ],
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )

        def run_order_case(
            name: str,
            older_factory: Any,
            newer_factory: Any,
        ) -> None:
            marker = root / f"release-{name}"
            order = root / f"order-{name}"
            holder = subprocess.Popen(
                [
                    sys.executable,
                    "-c",
                    holder_code,
                    str(task_dirs["holder"]),
                    str(marker),
                ],
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            older: Optional[subprocess.Popen[str]] = None
            newer: Optional[subprocess.Popen[str]] = None
            try:
                ready = (
                    holder.stdout.readline().strip()
                    if holder.stdout
                    else ""
                )
                older = older_factory(order, "older")
                older_joined = wait_for_depth(1)
                newer = newer_factory(order, "newer")
                newer_joined = wait_for_depth(2)
                marker.write_text("release\n", encoding="utf-8")
                older_result = older.wait(timeout=5)
                newer_result = newer.wait(timeout=5)
                holder_result = holder.wait(timeout=5)
                test.equal(f"FIFO {name} holder acquired", ready, "ready")
                test.true(f"FIFO {name} older ticket is visible", older_joined)
                test.true(f"FIFO {name} newer ticket is visible", newer_joined)
                test.equal(f"FIFO {name} older exits green", older_result, 0)
                test.equal(f"FIFO {name} newer exits green", newer_result, 0)
                test.equal(f"FIFO {name} holder exits green", holder_result, 0)
                test.equal(
                    f"FIFO {name} preserves admission order",
                    order.read_text(encoding="utf-8").splitlines(),
                    ["older", "newer"],
                )
            finally:
                marker.write_text("release\n", encoding="utf-8")
                for process in (newer, older, holder):
                    if process is not None and process.poll() is None:
                        process.terminate()
                        process.wait(timeout=3)

        run_order_case(
            "resource-before-harness",
            resource_waiter,
            task_waiter,
        )
        run_order_case(
            "harness-before-resource",
            task_waiter,
            resource_waiter,
        )

        marker = root / "release-stale-holder"
        stale_order = root / "order-stale"
        holder = subprocess.Popen(
            [
                sys.executable,
                "-c",
                holder_code,
                str(task_dirs["holder"]),
                str(marker),
            ],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        stale_code = (
            "import pathlib,sys,time;"
            f"sys.path.insert(0,{library!r});"
            "import runtime;"
            "root=runtime.shared_resource_root_for_task("
            "pathlib.Path(sys.argv[1]));"
            "ticket=runtime.resource_lane.join_lane_queue("
            "root,task='stale',command='killed-waiter');"
            "print('queued',flush=True);"
            "\nwhile True:\n time.sleep(1)"
        )
        stale: Optional[subprocess.Popen[str]] = None
        survivor: Optional[subprocess.Popen[str]] = None
        try:
            holder_ready = (
                holder.stdout.readline().strip() if holder.stdout else ""
            )
            stale = subprocess.Popen(
                [
                    sys.executable,
                    "-c",
                    stale_code,
                    str(task_dirs["older"]),
                ],
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
            stale_ready = (
                stale.stdout.readline().strip() if stale.stdout else ""
            )
            survivor = resource_waiter(stale_order, "newer")
            survivor_joined = wait_for_depth(2)
            stale.kill()
            stale.wait(timeout=3)
            marker.write_text("release\n", encoding="utf-8")
            survivor_result = survivor.wait(timeout=5)
            holder_result = holder.wait(timeout=5)
            test.equal("FIFO stale holder acquired", holder_ready, "ready")
            test.equal("FIFO stale waiter armed", stale_ready, "queued")
            test.true(
                "FIFO live waiter queues behind stale candidate",
                survivor_joined,
            )
            test.equal(
                "FIFO SIGKILL stale waiter does not block successor",
                survivor_result,
                0,
            )
            test.equal("FIFO stale holder exits green", holder_result, 0)
            test.equal(
                "FIFO successor runs exactly once after stale reap",
                stale_order.read_text(encoding="utf-8").splitlines(),
                ["newer"],
            )
            test.equal(
                "FIFO stale ticket is removed",
                queue_depth(),
                0,
            )
        finally:
            marker.write_text("release\n", encoding="utf-8")
            for process in (survivor, stale, holder):
                if process is not None and process.poll() is None:
                    process.terminate()
                    process.wait(timeout=3)


def sherpa_workspace_cache_cases(test: Tests) -> None:
    with tempfile.TemporaryDirectory(prefix="murmur-v2-sherpa-cache-") as raw:
        root = Path(raw)
        primary, driver, _base = _standalone_driver(root)
        snapshot = root / ".murmur-agent-tasks" / "v2" / "sherpa-test" / "verify"
        snapshot.parent.mkdir(parents=True)
        _git(
            driver,
            "clone",
            "-q",
            "--local",
            "--no-hardlinks",
            str(driver),
            str(snapshot),
        )
        common = Path(
            _git(
                driver,
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            )
        )
        task_dir = common / "agent-harness" / "v2" / "tasks" / "sherpa-test"
        task_dir.mkdir(parents=True)
        task_worktree = (
            root
            / ".murmur-agent-tasks"
            / "v2"
            / "sherpa-test"
            / "meetnotes"
        )
        runtime.atomic_write_json(
            task_dir / "task.json",
            {
                "task_id": "sherpa-test",
                "worktree_path": str(task_worktree),
            },
        )

        fixture = b"checksum-pinned-sherpa-fixture\n"
        fixture_sha = hashlib.sha256(fixture).hexdigest()
        machine = runtime.platform.machine().lower()
        architecture = {
            "aarch64": "arm64",
            "arm64": "arm64",
            "x86_64": "x86_64",
        }[machine]
        filename = f"sherpa-{architecture}.tar.bz2"
        legacy_directory = primary / "target" / "sherpa-onnx-prebuilt"
        legacy_directory.mkdir(parents=True)
        legacy_candidate = legacy_directory / filename
        legacy_candidate.write_bytes(fixture)
        config = copy.deepcopy(runtime.load_config())
        config["shared_artifacts"]["sherpa_onnx"] = {
            "directory": "target/sherpa-onnx-prebuilt",
            "archives": {
                architecture: {
                    "filename": filename,
                    "sha256": fixture_sha,
                }
            },
        }
        original_load_config = runtime.load_config
        try:
            runtime.load_config = lambda: copy.deepcopy(config)
            resolved, actual_sha = runtime.verified_sherpa_archive(
                snapshot,
                task_dir=task_dir,
            )
            shared_directory = (
                root
                / ".murmur-agent-tasks"
                / ".resources"
                / "target"
                / "sherpa-onnx-prebuilt"
            )
            shared_candidate = shared_directory / filename
            test.equal(
                "SHERPA isolated snapshot resolves workspace shared cache",
                resolved,
                shared_directory.resolve(),
            )
            test.equal(
                "SHERPA promoted archive remains checksum-bound",
                actual_sha,
                fixture_sha,
            )
            test.equal(
                "SHERPA runner promotes the verified legacy archive byte-identically",
                shared_candidate.read_bytes(),
                fixture,
            )
            test.true(
                "SHERPA snapshot requires no manual archive seed",
                not (
                    snapshot
                    / "target"
                    / "sherpa-onnx-prebuilt"
                    / filename
                ).exists(),
            )
            shared_candidate.write_bytes(b"corrupt\n")
            test.raises(
                "SHERPA corrupt shared cache fails closed despite valid legacy source",
                lambda: runtime.verified_sherpa_archive(
                    snapshot,
                    task_dir=task_dir,
                ),
                "checksum mismatch",
            )
            shared_candidate.unlink()
            shared_candidate.symlink_to(legacy_candidate)
            test.raises(
                "SHERPA symlinked shared archive fails closed",
                lambda: runtime.verified_sherpa_archive(
                    snapshot,
                    task_dir=task_dir,
                ),
                "not a regular file",
            )
            shared_candidate.unlink()
            shared_directory.rmdir()
            shared_directory.symlink_to(legacy_directory, target_is_directory=True)
            test.raises(
                "SHERPA symlinked shared cache directory fails closed",
                lambda: runtime.verified_sherpa_archive(
                    snapshot,
                    task_dir=task_dir,
                ),
                "cache directory is symlinked",
            )
            shared_directory.unlink()
            shared_directory.mkdir()
            legacy_candidate.unlink()
            snapshot_candidate = (
                snapshot
                / "target"
                / "sherpa-onnx-prebuilt"
                / filename
            )
            snapshot_candidate.parent.mkdir(parents=True)
            snapshot_candidate.write_bytes(fixture)
            test.raises(
                "SHERPA never promotes a manually seeded verification snapshot",
                lambda: runtime.verified_sherpa_archive(
                    snapshot,
                    task_dir=task_dir,
                ),
                "workspace shared cache",
            )
        finally:
            runtime.load_config = original_load_config


def verification_snapshot_cases(test: Tests) -> None:
    """A check snapshot must resolve Git objects inside its own Seatbelt scope."""

    with tempfile.TemporaryDirectory(prefix="murmur-v2-snapshot-") as raw:
        root = Path(raw)
        primary = root / "primary"
        base = _init_repo(primary)
        worktree = root / "task" / "meetnotes"
        worktree.parent.mkdir()
        _git(
            primary,
            "worktree",
            "add",
            "-q",
            "--detach",
            str(worktree),
            base,
        )
        (worktree / "owned.txt").write_text(
            "base\nsnapshot change\n", encoding="utf-8"
        )
        common = Path(
            _git(
                primary,
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            )
        )
        task_id = "snapshot-self-contained"
        task_dir = harness_cli.v2_task_dir(common, task_id)
        attempt_dir = task_dir / "attempts" / ("b" * 64)
        attempt_dir.mkdir(parents=True)
        contract: Dict[str, Any] = {
            "task_id": task_id,
            "contract_sha256": "c" * 64,
            "base_sha": base,
            "repo_realpath": str(primary.resolve()),
            "worktree_path": str(worktree.resolve()),
            "owned_paths": ["owned.txt"],
            "expected_change": True,
            "created_at": "2026-01-01T00:00:00Z",
        }
        runtime.atomic_write_json(task_dir / "task.json", contract)
        paths, diff, tree = verifier.snapshot_scoped_diff(
            worktree, contract, task_dir
        )
        plan = {
            "base_sha": base,
            "changed_paths": paths,
            "diff_sha256": runtime.sha256_bytes(diff),
            "tree_sha": tree,
            "plan_sha256": "d" * 64,
        }

        def has_no_object_alternates(repository: Path) -> bool:
            alternates = (
                repository / ".git" / "objects" / "info" / "alternates"
            )
            try:
                metadata = alternates.lstat()
            except FileNotFoundError:
                return True
            return (
                stat.S_ISREG(metadata.st_mode)
                and not stat.S_ISLNK(metadata.st_mode)
                and alternates.read_bytes() == b""
            )

        snapshot = harness_cli._ensure_verification_snapshot(
            contract, task_dir, plan, attempt_dir
        )
        alternates = snapshot / ".git" / "objects" / "info" / "alternates"
        test.true(
            "SNAPSHOT has no nonempty object alternates dependency",
            has_no_object_alternates(snapshot),
        )
        alternates.parent.mkdir(parents=True, exist_ok=True)
        alternates.write_text(
            str(primary / ".git" / "objects") + "\n", encoding="utf-8"
        )
        resumed_snapshot = harness_cli._ensure_verification_snapshot(
            contract, task_dir, plan, attempt_dir
        )
        test.true(
            "SNAPSHOT resume reconstructs a claimed repo with object alternates",
            resumed_snapshot == snapshot
            and has_no_object_alternates(resumed_snapshot),
        )
        resumed_paths, resumed_diff, resumed_tree = verifier.snapshot_scoped_diff(
            resumed_snapshot, contract, task_dir
        )
        test.equal(
            "SNAPSHOT resume preserves the exact base, diff, and tree",
            (
                _git(resumed_snapshot, "rev-parse", "HEAD"),
                resumed_paths,
                runtime.sha256_bytes(resumed_diff),
                resumed_tree,
            ),
            (
                plan["base_sha"],
                plan["changed_paths"],
                plan["diff_sha256"],
                plan["tree_sha"],
            ),
        )

        base_tree = _git(primary, "rev-parse", f"{base}^{{tree}}")
        evidence = runtime.run_check(
            resumed_snapshot,
            task_dir,
            {
                "id": "snapshot-head-tree",
                "command": (
                    "test \"$(git rev-parse 'HEAD^{tree}')\" = "
                    f"'{base_tree}'"
                ),
                "timeout_seconds": 10,
            },
            "snapshot-self-contained",
        )
        expected_sandbox_mode = (
            "inherited"
            if runtime.inherited_outer_sandbox_is_active()
            else "direct"
        )
        test.equal(
            "SNAPSHOT runner-owned Seatbelt check resolves HEAD^{tree}",
            (evidence["sandbox_mode"], evidence["passed"]),
            (expected_sandbox_mode, True),
        )
        harness_cli._discard_claimed_verification_snapshot(
            contract, attempt_dir, resumed_snapshot
        )
        test.true(
            "SNAPSHOT cleanup removes only the claimed verification repo",
            not resumed_snapshot.exists()
            and worktree.is_dir()
            and primary.is_dir(),
        )


def snapshot_node_modules_manifest_cases(test: Tests) -> None:
    """Exercise physical dependency-owner metadata in a standalone snapshot."""

    with tempfile.TemporaryDirectory(
        prefix="murmur-v2-snapshot-node-modules-"
    ) as raw:
        root = Path(raw)
        primary = root / "primary"
        _init_repo(primary)
        (primary / ".gitignore").write_text("/node_modules/\n", encoding="utf-8")
        physical_manifest = primary / "package.json"
        physical_lock = primary / "package-lock.json"
        physical_manifest.write_text(
            '{"name":"snapshot-physical-owner"}\n', encoding="utf-8"
        )
        physical_lock.write_text(
            '{"name":"snapshot-physical-owner","lockfileVersion":3}\n',
            encoding="utf-8",
        )
        dependency = primary / "node_modules" / "fixture" / "package.json"
        dependency.parent.mkdir(parents=True)
        dependency.write_text('{"name":"fixture"}\n', encoding="utf-8")
        _git(primary, "add", ".gitignore", "package.json", "package-lock.json")
        _git(primary, "commit", "-q", "-m", "add dependency owner manifests")
        base = _git(primary, "rev-parse", "HEAD")
        worktree = root / "task" / "meetnotes"
        worktree.parent.mkdir()
        _git(
            primary,
            "worktree",
            "add",
            "-q",
            "--detach",
            str(worktree),
            base,
        )
        (worktree / "owned.txt").write_text(
            "base\nmanifest snapshot change\n", encoding="utf-8"
        )
        common = Path(
            _git(
                primary,
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            )
        )
        task_dir = harness_cli.v2_task_dir(
            common, "snapshot-node-modules-owner"
        )
        attempt_dir = task_dir / "attempts" / ("e" * 64)
        attempt_dir.mkdir(parents=True)
        contract: Dict[str, Any] = {
            "task_id": "snapshot-node-modules-owner",
            "contract_sha256": "c" * 64,
            "base_sha": base,
            "repo_realpath": str(primary.resolve()),
            "worktree_path": str(worktree.resolve()),
            "owned_paths": ["owned.txt"],
            "expected_change": True,
            "created_at": "2026-01-01T00:00:00Z",
        }
        runtime.atomic_write_json(task_dir / "task.json", contract)
        paths, diff, tree = verifier.snapshot_scoped_diff(
            worktree, contract, task_dir
        )
        plan = {
            "base_sha": base,
            "changed_paths": paths,
            "diff_sha256": runtime.sha256_bytes(diff),
            "tree_sha": tree,
            "plan_sha256": "d" * 64,
        }
        snapshot = harness_cli._ensure_verification_snapshot(
            contract, task_dir, plan, attempt_dir
        )
        test.equal(
            "SNAPSHOT dependency link resolves to its physical owner",
            (snapshot / "node_modules").resolve(strict=True),
            (primary / "node_modules").resolve(strict=True),
        )
        command = (
            f"{json.dumps(sys.executable)} -c "
            + json.dumps(
                "import json,pathlib;"
                f"p=pathlib.Path({str(physical_manifest)!r});"
                f"lock=pathlib.Path({str(physical_lock)!r});"
                "assert json.loads(p.read_text())['name']"
                "=='snapshot-physical-owner';"
                "assert json.loads(lock.read_text())['lockfileVersion']==3"
            )
        )
        evidence = runtime.run_check(
            snapshot,
            task_dir,
            {
                "id": "snapshot-physical-manifests",
                "command": command,
                "timeout_seconds": 10,
            },
            "snapshot-node-modules-owner",
        )
        expected_sandbox_mode = (
            "inherited"
            if runtime.inherited_outer_sandbox_is_active()
            else "direct"
        )
        test.equal(
            "SNAPSHOT Seatbelt reads both physical owner manifests",
            (evidence["sandbox_mode"], evidence["passed"]),
            (expected_sandbox_mode, True),
        )
        # The profile is emitted even when an outer sandbox is inherited.
        # Inspect it unconditionally so this scope assertion can never turn
        # vacuous merely because the selftest itself is sandboxed.
        profile_text = Path(str(evidence["sandbox_profile_path"])).read_text(
            encoding="utf-8"
        )
        owner_literal = json.dumps(str(primary.resolve()))
        manifest_literal = json.dumps(str(physical_manifest.resolve()))
        lock_literal = json.dumps(str(physical_lock.resolve()))
        test.true(
            "SNAPSHOT Seatbelt grants manifest leaves, not owner subtree",
            f"(literal {manifest_literal})" in profile_text
            and f"(literal {lock_literal})" in profile_text
            and f"(subpath {owner_literal})" not in profile_text,
        )

        physical_manifest.unlink()
        physical_manifest.symlink_to(physical_lock.name)
        test.raises(
            "SNAPSHOT rejects a symlinked physical owner manifest",
            lambda: runtime.run_check(
                snapshot,
                task_dir,
                {
                    "id": "snapshot-symlinked-manifest",
                    "command": command,
                    "timeout_seconds": 10,
                },
                "snapshot-node-modules-owner",
            ),
            "shared node_modules manifest is not a real regular file",
        )
        harness_cli._discard_claimed_verification_snapshot(
            contract, attempt_dir, snapshot
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
        runtime.atomic_write_json(task_dir / "task.json", contract)
        paths, diff, tree = verifier.snapshot_scoped_diff(repo, contract, task_dir)
        plan = {
            "changed_paths": paths,
            "diff_sha256": runtime.sha256_bytes(diff),
            "tree_sha": tree,
            "plan_sha256": "1" * 64,
            "protocol_sha256": "2" * 64,
        }
        declared = {
            "id": "checkpoint",
            "command": "test -f owned.txt",
            "timeout_seconds": 10,
        }
        runtime.atomic_write_json(task_dir / "selftest-contract.json", contract)
        runtime.atomic_write_json(task_dir / "selftest-plan.json", plan)
        runtime.atomic_write_json(task_dir / "selftest-check.json", declared)
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

        retryable = runtime.load_json(record_path)
        retryable["evidence"]["passed"] = False
        retryable["evidence"]["outcome"] = "BLOCKED"
        retryable["evidence"]["timed_out"] = True
        runtime.atomic_write_json(record_path, retryable)
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
            "import runtime;"
            "lock=runtime.acquire_cargo_lane("
            "pathlib.Path(sys.argv[1]),2,command='lane-holder');"
            "print('ready',flush=True);"
            "marker=pathlib.Path(sys.argv[2]);deadline=time.monotonic()+10;"
            "\nwhile not marker.exists() and time.monotonic()<deadline:"
            "\n time.sleep(0.01)"
            "\nif marker.exists():"
            "\n time.sleep(0.05)"
            "\n"
            "runtime.release_cargo_lane(lock)"
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
                waited = runtime.run_check(
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
            "cwd": str(runtime.reviewer_execution_cwd()),
            "wall_timeout_seconds": 30,
            "removed_env_names": [],
            "created_at": runtime.utc_now(),
        }
        runtime.atomic_write_json(invocation_path, invocation)
        runtime.verify_model_invocation(
            evidence_root,
            vendor="claude",
            role="reviewer",
            label="review-combined-try-1",
            session_id="session-real",
            model="claude-test",
            cli_version="2.1.214",
            invocation_path_raw=str(invocation_path),
            invocation_sha256=runtime.sha256_file(invocation_path),
            expected_path=invocation_path,
            instructions_sha256="a" * 64,
            require_cwd_binding=True,
        )
        tampered = {**invocation, "role": "writer"}
        runtime.atomic_write_json(invocation_path, tampered)
        test.raises(
            "PROVENANCE forged reviewer invocation role is rejected",
            lambda: runtime.verify_model_invocation(
                evidence_root,
                vendor="claude",
                role="reviewer",
                label="review-combined-try-1",
                session_id="session-real",
                model="claude-test",
                cli_version="2.1.214",
                invocation_path_raw=str(invocation_path),
                invocation_sha256=runtime.sha256_file(invocation_path),
                expected_path=invocation_path,
                instructions_sha256="a" * 64,
                require_cwd_binding=True,
            ),
            "role",
        )
        tampered_cwd = {**invocation, "cwd": str(evidence_root.resolve())}
        runtime.atomic_write_json(invocation_path, tampered_cwd)
        test.raises(
            "PROVENANCE reviewer invocation outside isolated cwd is rejected",
            lambda: runtime.verify_model_invocation(
                evidence_root,
                vendor="claude",
                role="reviewer",
                label="review-combined-try-1",
                session_id="session-real",
                model="claude-test",
                cli_version="2.1.214",
                invocation_path_raw=str(invocation_path),
                invocation_sha256=runtime.sha256_file(invocation_path),
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
            runtime.extract_model_metadata(
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
            "created_at": runtime.utc_now(),
        }
        contract["contract_sha256"] = verifier.document_hash(
            contract, "contract_sha256"
        )
        runtime.validate_schema(
            contract,
            runtime.load_schema("v2-task"),
            label="v2 commit crash contract",
        )
        runtime.atomic_write_json(task_dir / "task.json", contract)
        runtime.atomic_write_json(
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
        original_evidence = runtime.load_json(evidence_path)
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
        runtime.atomic_write_json(evidence_path, forged_summary)
        test.raises(
            "EVIDENCE rehashed top-level findings cannot diverge from review records",
            lambda: verifier.verify_v2_evidence(
                contract, task_dir, allow_test_adapter=True
            ),
            "findings differs from its bound review records",
        )
        runtime.atomic_write_json(evidence_path, original_evidence)

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
        runtime.atomic_write_json(evidence_path, forged_probe_summary)
        test.raises(
            "EVIDENCE rehashed top-level probe requests cannot diverge from review records",
            lambda: verifier.verify_v2_evidence(
                contract, task_dir, allow_test_adapter=True
            ),
            "probe_requests differs from its bound review records",
        )
        runtime.atomic_write_json(evidence_path, original_evidence)

        original_review = original_evidence["reviews"][0]
        original_result_path = Path(str(original_review["result_path"]))
        original_review_result = runtime.load_json(original_result_path)
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
            runtime.atomic_write_json(original_result_path, severe_result)
            severe_evidence = copy.deepcopy(original_evidence)
            severe_evidence["reviews"][0]["result"] = severe_result
            severe_evidence["reviews"][0][
                "result_sha256"
            ] = runtime.sha256_file(original_result_path)
            outcomes = verifier.aggregate_review_outcomes(
                severe_evidence["reviews"]
            )
            severe_evidence.update(outcomes)
            severe_evidence["evidence_sha256"] = verifier.document_hash(
                severe_evidence, "evidence_sha256"
            )
            runtime.atomic_write_json(evidence_path, severe_evidence)
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
        runtime.atomic_write_json(original_result_path, gap_result)
        gap_evidence = copy.deepcopy(original_evidence)
        gap_evidence["reviews"][0]["result"] = gap_result
        gap_evidence["reviews"][0][
            "result_sha256"
        ] = runtime.sha256_file(original_result_path)
        gap_evidence.update(
            verifier.aggregate_review_outcomes(gap_evidence["reviews"])
        )
        gap_evidence["evidence_sha256"] = verifier.document_hash(
            gap_evidence, "evidence_sha256"
        )
        runtime.atomic_write_json(evidence_path, gap_evidence)
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
        runtime.atomic_write_json(original_result_path, probe_result)
        probe_evidence = copy.deepcopy(original_evidence)
        probe_evidence["reviews"][0]["result"] = probe_result
        probe_evidence["reviews"][0][
            "result_sha256"
        ] = runtime.sha256_file(original_result_path)
        probe_evidence.update(
            verifier.aggregate_review_outcomes(probe_evidence["reviews"])
        )
        probe_evidence["evidence_sha256"] = verifier.document_hash(
            probe_evidence, "evidence_sha256"
        )
        runtime.atomic_write_json(evidence_path, probe_evidence)
        test.raises(
            "EVIDENCE bound probe request cannot verify PASS",
            lambda: verifier.verify_v2_evidence(
                contract, task_dir, allow_test_adapter=True
            ),
            "unresolved proof gaps",
        )
        runtime.atomic_write_json(
            original_result_path, original_review_result
        )
        runtime.atomic_write_json(evidence_path, original_evidence)

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
                    "import runtime,verifier;"
                    f"doc=runtime.load_json(pathlib.Path({str(task_dir / 'task.json')!r}));"
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
                    "import cli,runtime;"
                    f"task=pathlib.Path({str(task_dir)!r});"
                    "contract=runtime.load_json(task/'task.json');"
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

        cleanup_snapshots = harness_cli._verification_snapshots_for_cleanup(
            contract, task_dir
        )
        present_snapshots = [
            path
            for path, _reference, _commit, _head in cleanup_snapshots
            if path.is_dir()
        ]
        test.true(
            "CLEAN fixture retains a materialized verification snapshot",
            bool(present_snapshots),
        )
        if not present_snapshots:
            return
        raced_snapshot = present_snapshots[0]
        expected_snapshot_head = next(
            head
            for path, _reference, _commit, head in cleanup_snapshots
            if path == raced_snapshot
        )
        _git(
            raced_snapshot,
            "-c",
            "user.name=QueaT",
            "-c",
            "user.email=kgm004a@gmail.com",
            "commit",
            "--allow-empty",
            "-q",
            "-m",
            "raced same-tree verification snapshot commit",
        )
        raced_snapshot_head = _git(
            raced_snapshot, "rev-parse", "HEAD"
        )
        snapshot_race_clean = subprocess.run(
            [
                sys.executable,
                "-B",
                "-c",
                (
                    "import argparse,sys;"
                    f"sys.path.insert(0,{fixture_python!r});"
                    "import cli;"
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
        test.true(
            "CLEAN refuses a same-tree verification snapshot HEAD race",
            snapshot_race_clean.returncode != 0
            and "verification snapshot HEAD changed"
            in snapshot_race_clean.stderr,
        )
        test.true(
            "CLEAN snapshot HEAD race preserves client and raced commit",
            repo.is_dir()
            and raced_snapshot.is_dir()
            and raced_snapshot_head != expected_snapshot_head
            and _git(raced_snapshot, "rev-parse", "HEAD")
            == raced_snapshot_head
            and not (task_dir / "clean-intent.json").exists(),
        )
        snapshot_saved_ref = "refs/selftest/raced-snapshot-head"
        _git(
            raced_snapshot,
            "update-ref",
            "--create-reflog",
            snapshot_saved_ref,
            raced_snapshot_head,
        )
        _git(raced_snapshot, "reset", "--soft", "-q", expected_snapshot_head)
        snapshot_control_payloads = _unique_git_control_payloads(
            raced_snapshot,
            snapshot_saved_ref,
            raced_snapshot_head,
        )
        test.equal(
            "CLEAN snapshot Git-control fixture resets HEAD to its pin",
            _git(raced_snapshot, "rev-parse", "HEAD"),
            expected_snapshot_head,
        )

        snapshot_exclude = present_snapshots[0] / ".git" / "info" / "exclude"
        snapshot_exclude.write_text(
            snapshot_exclude.read_text(encoding="utf-8")
            + "\n.DS_Store\n",
            encoding="utf-8",
        )
        protected_snapshot_byte = present_snapshots[0] / ".DS_Store"
        protected_snapshot_byte.write_bytes(
            b"operator-owned snapshot metadata\n"
        )
        blocked_clean = subprocess.run(
            [
                sys.executable,
                "-B",
                "-c",
                (
                    "import argparse,sys;"
                    f"sys.path.insert(0,{fixture_python!r});"
                    "import cli;"
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
        test.true(
            "CLEAN refuses ignored bytes in an auxiliary verification snapshot",
            blocked_clean.returncode != 0
            and "verification snapshot has ignored bytes"
            in blocked_clean.stderr,
        )
        test.true(
            "CLEAN auxiliary refusal preserves client and snapshot bytes",
            repo.is_dir()
            and protected_snapshot_byte.read_bytes()
            == b"operator-owned snapshot metadata\n"
            and not (task_dir / "clean-intent.json").exists(),
        )
        protected_snapshot_byte.unlink()

        source_manifests = [
            runtime.load_json(path)
            for path in sorted((task_dir / "attempts").glob("*/snapshot.json"))
        ]
        source_manifest = next(
            manifest
            for manifest in source_manifests
            if Path(str(manifest["path"])) == present_snapshots[0]
        )
        absent_attempt_id = hashlib.sha256(
            b"absent-cleanup-selftest"
        ).hexdigest()
        absent_attempt = task_dir / "attempts" / absent_attempt_id
        absent_attempt.mkdir()
        absent_path = harness_cli._verification_snapshot_path(
            contract, absent_attempt
        )
        absent_ref = harness_cli._verification_snapshot_ref(
            task_id, absent_attempt_id
        )
        absent_commit = str(source_manifest["snapshot_commit"])
        runtime.run_capture(
            [
                "git",
                "update-ref",
                absent_ref,
                absent_commit,
                "0" * 40,
            ],
            primary,
        )
        absent_manifest = {
            **source_manifest,
            "path": str(absent_path),
            "snapshot_ref": absent_ref,
            "snapshot_commit": absent_commit,
            "snapshot_sha256": "",
        }
        absent_manifest["snapshot_sha256"] = verifier.document_hash(
            absent_manifest, "snapshot_sha256"
        )
        runtime.atomic_write_json(
            absent_attempt / "snapshot.json", absent_manifest
        )
        sibling = repo.parent / "verify-sibling-sentinel"
        sibling.mkdir()
        sibling_byte = sibling / "operator.txt"
        sibling_byte.write_bytes(b"preserve exact sibling\n")

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
            (clean_result.returncode, clean_result.stderr.strip()),
            (0, ""),
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
        test.true(
            "CLEAN removes materialized verification snapshots",
            all(not path.exists() for path in present_snapshots),
        )
        clean_intent = runtime.load_json(task_dir / "clean-intent.json")
        snapshot_archive_index = next(
            index
            for index, record in enumerate(
                clean_intent["verification_snapshots"]
            )
            if Path(str(record["path"])) == raced_snapshot
        )
        snapshot_archive_role = f"snapshot-{snapshot_archive_index:04d}"
        test.true(
            "CLEAN snapshot pre/final archives recover hidden ref, reflog, and loose object",
            _git_control_payloads_recoverable(
                task_dir,
                (
                    f"{snapshot_archive_role}-pre",
                    f"{snapshot_archive_role}-final",
                ),
                snapshot_control_payloads,
            ),
        )
        restored_snapshot = root / "restored-snapshot-control"
        restored_snapshot_objects = _restore_archived_git_repository(
            task_dir,
            f"{snapshot_archive_role}-final",
            restored_snapshot,
        )
        restored_snapshot_fsck = subprocess.run(
            ["git", "fsck", "--full"],
            cwd=str(restored_snapshot),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        test.true(
            "CLEAN standalone snapshot archive reconstructs its unique commit and ref",
            raced_snapshot_head in restored_snapshot_objects
            and _git(
                restored_snapshot,
                "rev-parse",
                snapshot_saved_ref,
            )
            == raced_snapshot_head
            and _git(
                restored_snapshot,
                "cat-file",
                "-t",
                raced_snapshot_head,
            )
            == "commit"
            and restored_snapshot_fsck.returncode == 0,
        )
        test.equal(
            "CLEAN deletes the exact ref for an already-absent snapshot",
            runtime.git(
                primary,
                "rev-parse",
                "--verify",
                absent_ref,
                check=False,
            ),
            "",
        )
        test.equal(
            "CLEAN preserves an unrelated verification-named sibling",
            sibling_byte.read_bytes(),
            b"preserve exact sibling\n",
        )


def plan_and_probe_cases(test: Tests) -> None:
    base = runtime.git(ROOT, "rev-parse", "HEAD")
    tree = runtime.git(ROOT, "rev-parse", "HEAD^{tree}")
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
        runtime.load_config(),
    )
    angular_again, _bundle = verifier.build_plan(
        contract,
        ROOT,
        ["src/app/features/detail/detail.ts"],
        b"angular diff",
        tree,
        runtime.load_config(),
    )
    legacy_sensitive, _bundle = verifier.build_plan(
        contract,
        ROOT,
        ["src-tauri/src/storage/meeting_store.rs"],
        b"legacy sensitive diff",
        tree,
        runtime.load_config(),
    )
    test.equal(
        "PLAN contract without same-vendor field retains cross-vendor semantics",
        [review["vendor"] for review in legacy_sensitive["reviews"]],
        ["claude", "codex"],
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
        runtime.load_config(),
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
        runtime.load_config(),
    )
    test.true(
        "PLAN changed diff invalidates attempt binding",
        verifier.attempt_id(changed) != verifier.attempt_id(angular),
    )
    test.equal("PLAN parent remains current immutable base", angular["base_sha"], base)
    package_plan, _bundle = verifier.build_plan(
        contract,
        ROOT,
        ["package.json", "package-lock.json"],
        b"package diff",
        tree,
        runtime.load_config(),
    )
    test.equal(
        "PROBE package plan exposes only its own canonical checks",
        verifier.allowed_probe_ids(package_plan),
        ["npm-lock", "ng-lint", "ng-build"],
    )
    test.true(
        "PROBE package plan excludes unrelated config and Rust checks",
        "config-audit" not in verifier.allowed_probe_ids(package_plan)
        and "rust-lib" not in verifier.allowed_probe_ids(package_plan),
    )
    harness_plan, _bundle = verifier.build_plan(
        contract,
        ROOT,
        [".agents/harness/cli.py"],
        b"harness diff",
        tree,
        runtime.load_config(),
    )
    test.true(
        "PROBE harness plan contextually permits config-audit",
        "config-audit" in verifier.allowed_probe_ids(harness_plan),
    )

    test.raises(
        "PROBE broker rejects arbitrary shell ids",
        lambda: verifier.canonical_check("sh -c arbitrary", runtime.load_config()),
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
        (repo / ".gitignore").write_text(
            "/.angular/\n"
            "/dist/\n"
            "/target/\n"
            "/test-results/\n"
            "/src-tauri/binaries/murmur-brain\n"
            "/private-cache/\n",
            encoding="utf-8",
        )
        _git(repo, "add", ".gitignore")
        _git(repo, "commit", "-q", "-m", "ignore generated artifacts")
        base = _git(repo, "rev-parse", "HEAD")
        common = Path(
            _git(repo, "rev-parse", "--path-format=absolute", "--git-common-dir")
        )
        worktree = root / "task" / "meetnotes"
        worktree.parent.mkdir()
        branch = "agent/v2/clean-selftest"
        _git(repo, "worktree", "add", "-q", "-b", branch, str(worktree), base)
        (worktree / "owned.txt").write_text("base\ndirty tracked\n", encoding="utf-8")
        (worktree / "untracked.txt").write_text("dirty untracked\n", encoding="utf-8")
        disposable_paths = (
            ".angular/cache/compiler.db",
            "dist/meetnotes/app.js",
            "target/CACHEDIR.TAG",
            "target/debug/app",
            "target/debug/app-hardlink",
            "test-results/.last-run.json",
            "src-tauri/binaries/murmur-brain",
        )
        for relative in disposable_paths:
            artifact = worktree / relative
            artifact.parent.mkdir(parents=True, exist_ok=True)
            artifact.write_bytes(b"reproducible build artifact\n")
        (worktree / "target" / "CACHEDIR.TAG").write_bytes(
            b"Signature: 8a477f597d28d172789f06886806bc55\n"
        )
        (worktree / "src-tauri" / "target").mkdir(parents=True)
        (worktree / "src-tauri" / "target" / "CACHEDIR.TAG").write_bytes(
            b"Signature: 8a477f597d28d172789f06886806bc55\n"
        )
        protected_bundle_layouts = (
            "target/release/bundle/dmg/Murmur.dmg",
            "target/universal-apple-darwin/release/bundle/macos/Murmur.app",
            "src-tauri/target/release/bundle/dmg/Murmur.dmg",
            (
                "src-tauri/target/aarch64-apple-darwin/release/"
                "bundle/macos/Murmur.app"
            ),
        )
        test.true(
            "CLEAN never classifies any Tauri release bundle layout as disposable",
            all(
                not harness_cli._disposable_ignored_path(worktree, path)
                for path in protected_bundle_layouts
            ),
        )
        (worktree / "target/debug/app-hardlink").unlink()
        os.link(
            worktree / "target/debug/app",
            worktree / "target/debug/app-hardlink",
        )
        unknown_ignored = worktree / ".angular" / "operator-note.txt"
        unknown_ignored.write_bytes(b"operator-owned ignored bytes\n")
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
            "created_at": runtime.utc_now(),
        }
        contract["contract_sha256"] = verifier.document_hash(
            contract, "contract_sha256"
        )
        runtime.atomic_write_json(task_dir / "task.json", contract)
        runtime.atomic_write_json(
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
        checks_root = task_dir / "runtime" / "checks"
        for name in ("cargo-target", "cargo-home", "npm-cache"):
            disposable = checks_root / name
            disposable.mkdir(parents=True)
            (disposable / "cache.bin").write_bytes(b"private cache\n")
        profile = checks_root / "profiles" / "check.sb"
        profile.parent.mkdir(parents=True)
        profile.write_text("(version 1)\n", encoding="utf-8")
        evidence = task_dir / "attempts" / "evidence.json"
        evidence.parent.mkdir()
        evidence.write_text('{"passed":true}\n', encoding="utf-8")
        shared_cache = (
            root
            / ".murmur-agent-tasks"
            / ".resources"
            / "target"
            / "sherpa-onnx-prebuilt"
            / "archive.tar.bz2"
        )
        shared_cache.parent.mkdir(parents=True)
        shared_cache.write_bytes(b"shared pinned archive\n")
        harness_cli.set_v2_state(task_dir, "OPEN", phase="open")
        previous = Path.cwd()
        try:
            os.chdir(repo)
            test.raises(
                "CLEAN refuses unknown ignored bytes before archiving or removal",
                lambda: harness_cli.cmd_clean(
                    argparse.Namespace(task_id="clean-selftest", abandon=True)
                ),
                "ignored task bytes are not archived",
            )
            test.true(
                "CLEAN unknown ignored refusal preserves task and valuable bytes",
                worktree.is_dir()
                and unknown_ignored.read_bytes() == b"operator-owned ignored bytes\n"
                and (worktree / ".angular/cache/compiler.db").is_file()
                and not (task_dir / "archive.json").exists(),
            )
            unknown_ignored.unlink()
            signed_bundle = (
                worktree
                / "target"
                / "universal-apple-darwin"
                / "release"
                / "bundle"
                / "dmg"
                / "Murmur_1.0.2_universal.dmg"
            )
            signed_bundle.parent.mkdir(parents=True)
            signed_bundle.write_bytes(
                b"signed notarized stapled operator artifact\n"
            )
            test.raises(
                "CLEAN refuses Tauri release bundles under Cargo target",
                lambda: harness_cli.cmd_clean(
                    argparse.Namespace(
                        task_id="clean-selftest", abandon=True
                    )
                ),
                "Murmur_1.0.2_universal.dmg",
            )
            test.true(
                "CLEAN preserves a refused signed release bundle",
                signed_bundle.read_bytes()
                == b"signed notarized stapled operator artifact\n"
                and worktree.is_dir()
                and not (task_dir / "archive.json").exists(),
            )
            signed_bundle.unlink()

            _git(
                repo,
                "config",
                "filter.scrub.clean",
                "sed 's/SECRET//g'",
            )
            attributes = worktree / ".gitattributes"
            filtered = worktree / "filter-victim.txt"
            attributes.write_text(
                "filter-victim.txt filter=scrub\n",
                encoding="utf-8",
            )
            filtered.write_bytes(b"SECRET operator exact bytes\n")
            test.raises(
                "CLEAN refuses a Git clean filter that transforms raw bytes",
                lambda: harness_cli.cmd_clean(
                    argparse.Namespace(
                        task_id="clean-selftest", abandon=True
                    )
                ),
                "archive transformed raw bytes",
            )
            test.true(
                "CLEAN preserves exact filter-transformed source bytes",
                filtered.read_bytes()
                == b"SECRET operator exact bytes\n"
                and worktree.is_dir()
                and not (task_dir / "archive.json").exists()
                and not (task_dir / "clean-intent.json").exists(),
            )
            attributes.unlink()
            filtered.unlink()
            _git(repo, "config", "--unset-all", "filter.scrub.clean")

            holder = subprocess.Popen(
                [
                    sys.executable,
                    "-c",
                    (
                        "import os,sys;"
                        "os.chdir(sys.argv[1]);"
                        "print('ready',flush=True);"
                        "sys.stdin.read()"
                    ),
                    str(worktree),
                ],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
            try:
                if holder.stdout is None:
                    raise AssertionError("missing cwd holder stdout")
                test.equal(
                    "CLEAN cwd-holder fixture is ready",
                    holder.stdout.readline().strip(),
                    "ready",
                )
                test.raises(
                    "CLEAN refuses before mutation when another shell cwd uses the task",
                    lambda: harness_cli.cmd_clean(
                        argparse.Namespace(
                            task_id="clean-selftest", abandon=True
                        )
                    ),
                    "still used by a process",
                )
                test.true(
                    "CLEAN cwd refusal leaves original path and no intent",
                    worktree.is_dir()
                    and not (task_dir / "archive.json").exists()
                    and not (task_dir / "clean-intent.json").exists(),
                )
            finally:
                if holder.stdin is not None:
                    holder.stdin.close()
                holder.wait(timeout=5)

            _git(
                repo,
                "config",
                "filter.scrub.clean",
                "sed 's/SECRET//g'",
            )
            attributes.write_text(
                "filter-victim.txt filter=scrub\n",
                encoding="utf-8",
            )
            filtered.write_bytes(b" operator exact bytes\n")

            original_execute_clean_intent = harness_cli._execute_clean_intent

            def fail_after_intent_publication(
                _contract: Mapping[str, Any],
                _task_dir: Path,
                _intent: Mapping[str, Any],
            ) -> None:
                raise runtime.HarnessError(
                    "forced crash after durable clean intent"
                )

            harness_cli._execute_clean_intent = fail_after_intent_publication
            try:
                test.raises(
                    "CLEAN exposes a crash after intent publication",
                    lambda: harness_cli.cmd_clean(
                        argparse.Namespace(
                            task_id="clean-selftest", abandon=True
                        )
                    ),
                    "forced crash after durable clean intent",
                )
            finally:
                harness_cli._execute_clean_intent = original_execute_clean_intent
            test.true(
                "CLEAN publishes immutable intent before quarantine",
                (task_dir / "clean-intent.json").is_file()
                and (task_dir / "archive.json").is_file()
                and worktree.is_dir(),
            )

            clean_intent = runtime.load_json(task_dir / "clean-intent.json")
            expected_client_head = str(
                clean_intent["worktree_revision"]
            )
            _git(
                worktree,
                "-c",
                "user.name=QueaT",
                "-c",
                "user.email=kgm004a@gmail.com",
                "commit",
                "--allow-empty",
                "-q",
                "-m",
                "raced same-tree client commit",
            )
            raced_client_head = _git(worktree, "rev-parse", "HEAD")
            test.raises(
                "CLEAN refuses a same-tree client HEAD race after intent",
                lambda: harness_cli.cmd_clean(
                    argparse.Namespace(
                        task_id="clean-selftest", abandon=True
                    )
                ),
                "client revision changed",
            )
            test.true(
                "CLEAN client HEAD race preserves the original worktree",
                worktree.is_dir()
                and raced_client_head != expected_client_head
                and not Path(clean_intent["quarantine_path"]).exists()
                and not (task_dir / "clean-delete-client.json").exists(),
            )
            _git(
                repo,
                "update-ref",
                "refs/selftest/raced-client-head",
                raced_client_head,
            )
            _git(worktree, "reset", "--soft", expected_client_head)

            filtered.write_bytes(b"SECRET operator exact bytes\n")
            test.raises(
                "CLEAN freeze refuses post-intent raw bytes hidden by a clean filter",
                lambda: harness_cli.cmd_clean(
                    argparse.Namespace(
                        task_id="clean-selftest", abandon=True
                    )
                ),
                "client freeze transformed raw bytes",
            )
            quarantined = Path(clean_intent["quarantine_path"])
            quarantined_filtered = quarantined / "filter-victim.txt"
            test.true(
                "CLEAN post-intent filter refusal preserves exact raw bytes",
                not worktree.exists()
                and quarantined_filtered.read_bytes()
                == b"SECRET operator exact bytes\n"
                and not (task_dir / "clean-delete-client.json").exists(),
            )
            quarantined_filtered.write_bytes(b" operator exact bytes\n")

            late_unknown = (
                quarantined / ".angular" / "operator-after-intent.txt"
            )
            late_unknown.write_bytes(b"late operator-owned bytes\n")
            test.raises(
                "CLEAN durable intent still refuses a late unknown ignored file",
                lambda: harness_cli.cmd_clean(
                    argparse.Namespace(
                        task_id="clean-selftest", abandon=True
                    )
                ),
                "ignored bytes are not archived",
            )
            quarantined_unknown = (
                quarantined / ".angular" / "operator-after-intent.txt"
            )
            test.true(
                "CLEAN late unknown refusal preserves worktree and durable archive",
                not worktree.exists()
                and quarantined.is_dir()
                and quarantined_unknown.read_bytes()
                == b"late operator-owned bytes\n"
                and (task_dir / "archive.json").is_file()
                and (task_dir / "clean-intent.json").is_file(),
            )
            quarantined_unknown.unlink()
            original_visible_tree = harness_cli._visible_tree
            visible_calls = 0
            race_unknown = (
                quarantined / ".angular" / "operator-after-tree.txt"
            )

            def inject_unknown_after_tree(
                visible_worktree: Path, runtime_dir: Path
            ) -> str:
                nonlocal visible_calls
                result = original_visible_tree(
                    visible_worktree, runtime_dir
                )
                if visible_worktree == quarantined:
                    visible_calls += 1
                    if visible_calls == 2:
                        race_unknown.write_bytes(
                            b"post-tree operator bytes\n"
                        )
                return result

            harness_cli._visible_tree = inject_unknown_after_tree
            try:
                test.raises(
                    "CLEAN catches a file racing the final visible-tree proof",
                    lambda: harness_cli.cmd_clean(
                        argparse.Namespace(
                            task_id="clean-selftest", abandon=True
                        )
                    ),
                    "filesystem changed during freeze",
                )
            finally:
                harness_cli._visible_tree = original_visible_tree
            test.true(
                "CLEAN post-tree race remains recoverable in quarantine",
                race_unknown.read_bytes() == b"post-tree operator bytes\n"
                and quarantined.is_dir()
                and not (task_dir / "clean-delete-client.json").exists(),
            )
            race_unknown.unlink()
            late_cache = (
                quarantined / ".angular" / "cache" / "late-cache.db"
            )
            late_cache.write_bytes(b"late reproducible cache\n")
            original_rmdir = harness_cli.os.rmdir
            staging = Path(clean_intent["delete_staging_path"])

            def fail_after_root_removal(
                path: Any, *rmdir_args: Any, **rmdir_kwargs: Any
            ) -> None:
                if (
                    not rmdir_args
                    and not rmdir_kwargs
                    and Path(path) == staging
                ):
                    raise runtime.HarnessError(
                        "forced crash before staging finalization"
                    )
                original_rmdir(path, *rmdir_args, **rmdir_kwargs)

            harness_cli.os.rmdir = fail_after_root_removal
            try:
                test.raises(
                    "CLEAN exposes a crash after root removal",
                    lambda: harness_cli.cmd_clean(
                        argparse.Namespace(
                            task_id="clean-selftest", abandon=True
                        )
                    ),
                    "forced crash before staging finalization",
                )
            finally:
                harness_cli.os.rmdir = original_rmdir
            test.true(
                "CLEAN crash retains only empty manifest-backed staging",
                not quarantined.exists()
                and staging.is_dir()
                and not any(staging.iterdir())
                and staging.parent.is_dir(),
            )
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
        archived_paths = set(
            _git(repo, "ls-tree", "-r", "--name-only", archive_ref).splitlines()
        )
        test.true(
            "CLEAN discards only allowlisted reproducible ignored artifacts",
            all(path not in archived_paths for path in disposable_paths),
        )
        test.equal(
            "CLEAN records first-pass disposable runtime removal",
            state["runtime_removed"],
            ["cargo-home", "cargo-target", "npm-cache"],
        )
        test.true(
            "CLEAN removes task-private Cargo and npm caches",
            all(
                not (checks_root / name).exists()
                for name in ("cargo-target", "cargo-home", "npm-cache")
            ),
        )
        test.true(
            "CLEAN preserves attested sandbox profiles",
            profile.is_file(),
        )
        test.true("CLEAN preserves task evidence", evidence.is_file())
        test.equal(
            "CLEAN preserves workspace shared pinned cache",
            shared_cache.read_bytes(),
            b"shared pinned archive\n",
        )

        late_cache = checks_root / "cargo-target"
        late_cache.mkdir()
        (late_cache / "late.bin").write_bytes(b"late\n")
        previous = Path.cwd()
        try:
            os.chdir(repo)
            with contextlib.redirect_stdout(io.StringIO()):
                harness_cli.cmd_clean(
                    argparse.Namespace(
                        task_id="clean-selftest",
                        abandon=False,
                    )
                )
        finally:
            os.chdir(previous)
        test.true(
            "CLEAN terminal retry prunes late private runtime",
            not late_cache.exists(),
        )
        test.true(
            "CLEAN terminal retry still preserves profiles and evidence",
            profile.is_file() and evidence.is_file(),
        )
        test.true(
            "CLEAN terminal retry preserves shared pinned cache",
            shared_cache.is_file(),
        )


def lock_review_scope_prompt_cases(test: Tests) -> None:
    prompt = (
        ROOT / ".agents" / "harness" / "prompts" / "lock-security-reviewer.md"
    ).read_text(encoding="utf-8")
    begin = "<!-- LOCK_REVIEW_POLICY_V1_BEGIN -->"
    end = "<!-- LOCK_REVIEW_POLICY_V1_END -->"
    test.equal("LOCK REVIEW policy has one start marker", prompt.count(begin), 1)
    test.equal("LOCK REVIEW policy has one end marker", prompt.count(end), 1)

    policy_text = prompt.split(begin, 1)[1].split(end, 1)[0]
    policy: Dict[str, Dict[str, Any]] = {}
    for raw_line in policy_text.splitlines():
        line = raw_line.strip()
        if not line:
            continue
        columns = line.split("|")
        if len(columns) != 4:
            raise AssertionError(f"invalid lock review policy row: {line}")
        property_id = columns[0]
        if property_id in policy:
            raise AssertionError(f"duplicate lock review policy row: {property_id}")
        fields: Dict[str, str] = {}
        for column in columns[1:]:
            key, separator, value = column.partition("=")
            if not separator or not key or not value or key in fields:
                raise AssertionError(f"invalid lock review policy field: {column}")
            fields[key] = value
        if set(fields) != {"applies", "requires", "missing"}:
            raise AssertionError(f"incomplete lock review policy row: {line}")
        policy[property_id] = {
            "applies": fields["applies"],
            "requires": tuple(fields["requires"].split(",")),
            "missing": fields["missing"],
        }

    expected_policy = {
        "LOCKED_READ": {
            "applies": "new_or_changed_folder_lock_read_or_export",
            "requires": (
                "session_unlock_gate",
                "negative_non_disclosure_each_changed_sink",
            ),
            "missing": "BLOCKED",
        },
        "CHANGED_SEAL": {
            "applies": (
                "new_or_changed_seal_encryption_or_destructive_"
                "plaintext_replacement_semantics"
            ),
            "requires": (
                "verify_before_destroy_failure",
                "byte_identical_round_trip",
            ),
            "missing": "BLOCKED",
        },
        "UNCHANGED_SEAL": {
            "applies": (
                "no_changed_seal_encryption_or_destructive_"
                "plaintext_replacement_semantics"
            ),
            "requires": ("justified_na",),
            "missing": "BLOCKED",
        },
        "ORG_READ": {
            "applies": "new_or_changed_org_shared_brain_read_or_sink",
            "requires": (
                "local_membership",
                "member_gated_import_or_authorized_disclosure",
                "context_enabled",
                "tombstones",
                "result_bounds",
                "changed_sink_non_disclosure",
            ),
            "missing": "BLOCKED",
        },
    }
    test.equal("LOCK REVIEW parsed policy is exact", policy, expected_policy)

    legacy_unconditional_clauses = (
        "A PASS requires evidence that every new content read/export",
        "every seal is verify-before-destroy",
        "Missing negative-path or byte-identity evidence means BLOCKED",
    )
    test.equal(
        "LOCK REVIEW rejects legacy unconditional requirements",
        [clause for clause in legacy_unconditional_clauses if clause in prompt],
        [],
    )
    test.true(
        "LOCK REVIEW distinguishes outbound org consent from read authorization",
        "`org_egress_consented` is an outbound publish gate" in prompt
        and "not a receiver-side read gate" in prompt
        and "separate per-read consent requirement" in prompt,
    )
    test.true(
        "LOCK REVIEW policy has no obsolete receiver consent requirement",
        "consent" not in policy["ORG_READ"]["requires"],
    )

    def evaluate(property_id: str, evidence: Sequence[str]) -> str:
        row = policy[property_id]
        missing = set(row["requires"]) - set(evidence)
        return row["missing"] if missing else "EVIDENCE_COMPLETE"

    changed_sink_requirement = "negative_non_disclosure_each_changed_sink"

    def evaluate_locked(
        changed_sinks: Sequence[str],
        proved_sinks: Sequence[str],
        *,
        has_unlock_gate: bool = True,
    ) -> str:
        evidence = ["session_unlock_gate"] if has_unlock_gate else []
        if set(changed_sinks).issubset(set(proved_sinks)):
            evidence.append(changed_sink_requirement)
        return evaluate("LOCKED_READ", evidence)

    possible_sinks = ("ui", "mcp", "tool", "assets", "exports", "logs")
    test.equal(
        "LOCK REVIEW accepts complete affected locked-read evidence",
        evaluate_locked(["mcp", "tool"], ["mcp", "tool"]),
        "EVIDENCE_COMPLETE",
    )
    test.equal(
        "LOCK REVIEW does not demand unrelated locked-read sinks",
        evaluate_locked(["mcp"], ["mcp"]),
        "EVIDENCE_COMPLETE",
    )
    test.equal(
        "LOCK REVIEW blocks locked-read missing session unlock gate",
        evaluate_locked(["mcp"], ["mcp"], has_unlock_gate=False),
        "BLOCKED",
    )
    for sink in possible_sinks:
        test.equal(
            f"LOCK REVIEW blocks affected locked-read sink {sink} without proof",
            evaluate_locked([sink], []),
            "BLOCKED",
        )
        test.equal(
            f"LOCK REVIEW accepts affected locked-read sink {sink} with proof",
            evaluate_locked([sink], [sink]),
            "EVIDENCE_COMPLETE",
        )

    changed_seal_evidence = expected_policy["CHANGED_SEAL"]["requires"]
    test.equal(
        "LOCK REVIEW accepts complete changed-seal evidence",
        evaluate("CHANGED_SEAL", changed_seal_evidence),
        "EVIDENCE_COMPLETE",
    )
    for required in changed_seal_evidence:
        test.equal(
            f"LOCK REVIEW blocks changed seal missing {required}",
            evaluate(
                "CHANGED_SEAL",
                [item for item in changed_seal_evidence if item != required],
            ),
            "BLOCKED",
        )

    test.equal(
        "LOCK REVIEW accepts justified unchanged-seal N-A",
        evaluate("UNCHANGED_SEAL", ["justified_na"]),
        "EVIDENCE_COMPLETE",
    )
    test.equal(
        "LOCK REVIEW blocks unjustified unchanged-seal N-A",
        evaluate("UNCHANGED_SEAL", []),
        "BLOCKED",
    )

    org_evidence = expected_policy["ORG_READ"]["requires"]
    test.equal(
        "LOCK REVIEW accepts complete org visibility evidence",
        evaluate("ORG_READ", org_evidence),
        "EVIDENCE_COMPLETE",
    )
    for required in org_evidence:
        test.equal(
            f"LOCK REVIEW blocks org read missing {required}",
            evaluate("ORG_READ", [item for item in org_evidence if item != required]),
            "BLOCKED",
        )
    test.true(
        "LOCK REVIEW wrapper around unchanged seal remains N-A",
        "same seal call does not by itself change seal" in prompt
        and "destructive plaintext-replacement semantics" in prompt
        and "brief call-chain justification" in prompt,
    )


def egress_review_scope_prompt_cases(test: Tests) -> None:
    prompt = (
        ROOT / ".agents" / "harness" / "prompts" / "egress-security-reviewer.md"
    ).read_text(encoding="utf-8")
    begin = "<!-- EGRESS_REVIEW_POLICY_V1_BEGIN -->"
    end = "<!-- EGRESS_REVIEW_POLICY_V1_END -->"
    test.equal("EGRESS REVIEW policy has one start marker", prompt.count(begin), 1)
    test.equal("EGRESS REVIEW policy has one end marker", prompt.count(end), 1)

    policy_text = prompt.split(begin, 1)[1].split(end, 1)[0]
    policy: Dict[str, Dict[str, Any]] = {}
    for raw_line in policy_text.splitlines():
        line = raw_line.strip()
        if not line:
            continue
        columns = line.split("|")
        if len(columns) != 4:
            raise AssertionError(f"invalid egress review policy row: {line}")
        property_id = columns[0]
        if property_id in policy:
            raise AssertionError(f"duplicate egress review policy row: {property_id}")
        fields: Dict[str, str] = {}
        for column in columns[1:]:
            key, separator, value = column.partition("=")
            if not separator or not key or not value or key in fields:
                raise AssertionError(f"invalid egress review policy field: {column}")
            fields[key] = value
        if set(fields) != {"applies", "requires", "missing"}:
            raise AssertionError(f"incomplete egress review policy row: {line}")
        policy[property_id] = {
            "applies": fields["applies"],
            "requires": tuple(fields["requires"].split(",")),
            "missing": fields["missing"],
        }

    expected_policy = {
        "CLOUD_EGRESS": {
            "applies": (
                "new_or_changed_payload_leaves_device_or_loopback_for_remote_service"
            ),
            "requires": (
                "explicit_consent",
                "redaction_if_applicable",
                "egress_ledger",
                "fail_closed_provider_classification",
                "no_raw_client_bypass",
            ),
            "missing": "BLOCKED",
        },
        "LOCAL_LOOPBACK": {
            "applies": "new_or_changed_local_service_exclusively_bound_to_loopback",
            "requires": (
                "loopback_only_bind",
                "no_remote_or_ambient_network_path",
                "changed_sink_authorization",
            ),
            "missing": "BLOCKED",
        },
        "NO_EGRESS": {
            "applies": "no_new_or_changed_network_or_provider_path",
            "requires": ("justified_na",),
            "missing": "BLOCKED",
        },
    }
    test.equal("EGRESS REVIEW parsed policy is exact", policy, expected_policy)

    legacy_unconditional = (
        "A PASS requires that every new outbound payload goes through explicit consent"
    )
    test.true(
        "EGRESS REVIEW rejects legacy unconditional cloud requirement",
        legacy_unconditional not in prompt,
    )

    def evaluate(property_id: str, evidence: Sequence[str]) -> str:
        row = policy[property_id]
        missing = set(row["requires"]) - set(evidence)
        return row["missing"] if missing else "EVIDENCE_COMPLETE"

    cloud_evidence = expected_policy["CLOUD_EGRESS"]["requires"]
    test.equal(
        "EGRESS REVIEW accepts complete cloud evidence",
        evaluate("CLOUD_EGRESS", cloud_evidence),
        "EVIDENCE_COMPLETE",
    )
    for required in cloud_evidence:
        test.equal(
            f"EGRESS REVIEW blocks cloud path missing {required}",
            evaluate(
                "CLOUD_EGRESS",
                [item for item in cloud_evidence if item != required],
            ),
            "BLOCKED",
        )

    loopback_evidence = expected_policy["LOCAL_LOOPBACK"]["requires"]
    test.equal(
        "EGRESS REVIEW accepts bounded local loopback without cloud controls",
        evaluate("LOCAL_LOOPBACK", loopback_evidence),
        "EVIDENCE_COMPLETE",
    )
    test.true(
        "EGRESS REVIEW loopback policy excludes cloud-only controls",
        set(loopback_evidence).isdisjoint(
            {"explicit_consent", "redaction_if_applicable", "egress_ledger"}
        ),
    )
    for required in loopback_evidence:
        test.equal(
            f"EGRESS REVIEW blocks loopback path missing {required}",
            evaluate(
                "LOCAL_LOOPBACK",
                [item for item in loopback_evidence if item != required],
            ),
            "BLOCKED",
        )

    test.equal(
        "EGRESS REVIEW accepts justified no-egress N-A",
        evaluate("NO_EGRESS", ["justified_na"]),
        "EVIDENCE_COMPLETE",
    )
    test.equal(
        "EGRESS REVIEW blocks unjustified no-egress N-A",
        evaluate("NO_EGRESS", []),
        "BLOCKED",
    )
    test.true(
        "EGRESS REVIEW prose pins loopback distinction",
        "`LOCAL_LOOPBACK` is a local disclosure boundary, not cloud egress" in prompt
        and "does not require cloud consent, redaction, or an egress-ledger row" in prompt,
    )


def main() -> int:
    test = Tests()
    open_protocol_base_cases(test)
    open_branch_ownership_cases(test)
    standalone_driver_open_cases(test)
    clean_stage_barrier_cases(test)
    clean_global_validation_barrier_cases(test)
    profile_cases(test)
    npm_lock_evidence_cases(test)
    reviewer_tool_guard_cases(test)
    verdict_cases(test)
    focused_review_evidence_cases(test)
    specialist_source_context_cases(test)
    retry_cases(test)
    guardian_and_artifact_cases(test)
    readonly_review_wall_timeout_cases(test)
    state_and_lock_cases(test)
    standalone_driver_lane_cases(test)
    fifo_lane_cases(test)
    sherpa_workspace_cache_cases(test)
    verification_snapshot_cases(test)
    snapshot_node_modules_manifest_cases(test)
    checkpoint_cases(test)
    protocol_and_runtime_cases(test)
    commit_recovery_cases(test)
    plan_and_probe_cases(test)
    clean_cases(test)
    lock_review_scope_prompt_cases(test)
    egress_review_scope_prompt_cases(test)
    probe_precedence_flow_cases(test)
    if test.failures:
        print("v2 selftest: FAIL")
        for failure in test.failures:
            print(f"  - {failure}")
        return 1
    print(f"v2 selftest: PASS ({test.count} assertions)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
