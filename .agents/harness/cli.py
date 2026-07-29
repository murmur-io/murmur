#!/usr/bin/env python3
"""Verifier-only lifecycle for the Murmur Harness.

A developer edits an isolated worktree, then ``plan`` and ``verify`` bind
deterministic checks and fresh reviews to the exact diff. There is no writer,
repair loop, or legacy task lifecycle.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import copy
import datetime as dt
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import time
from typing import Any, Dict, Iterable, List, Mapping, Optional, Sequence, Tuple

import runtime
import verifier


STANDALONE_DRIVER_NAME = ".murmur-agent-driver"
CLIENT_WORKTREE_NAME = "meetnotes"
CANONICAL_MURMUR_ORIGINS = frozenset(
    {
        "https://github.com/murmur-io/murmur.git",
        "git@github.com:murmur-io/murmur.git",
        "ssh://git@github.com/murmur-io/murmur.git",
    }
)


def v2_store(common: Path) -> Path:
    return common / "agent-harness" / "v2"


def v2_tasks(common: Path) -> Path:
    return v2_store(common) / "tasks"


def v2_task_dir(common: Path, task_id: str) -> Path:
    return v2_tasks(common) / task_id


def _single_write_jsonl(path: Path, document: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = runtime.canonical_json(document) + b"\n"
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    try:
        written = os.write(descriptor, payload)
        if written != len(payload):
            raise runtime.HarnessError(f"short append to {path}")
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def set_v2_state(task_dir: Path, status: str, **details: Any) -> Dict[str, Any]:
    if status not in verifier.V2_STATES:
        raise runtime.HarnessError(f"invalid v2 state: {status}")
    prior: Dict[str, Any] = {}
    state_path = task_dir / "state.json"
    if (task_dir / "events.jsonl").is_file():
        prior = load_v2_state(task_dir)
    elif state_path.is_file():
        raise runtime.HarnessError(
            "v2 state projection exists without its authoritative event ledger"
        )
    if prior:
        if prior.get("status") in verifier.V2_TERMINAL_STATES:
            raise runtime.HarnessError(
                f"v2 task is terminal ({prior.get('status')}); state cannot change"
            )
    prior_revision = prior.get("state_revision", 0)
    if (
        isinstance(prior_revision, bool)
        or not isinstance(prior_revision, int)
        or prior_revision < 0
    ):
        raise runtime.HarnessError("v2 state revision is malformed")
    state = {
        "schema_version": 2,
        "task_id": task_dir.name,
        "status": status,
        "updated_at": runtime.utc_now(),
        **{
            key: value
            for key, value in prior.items()
            if key not in {"schema_version", "task_id", "status", "updated_at"}
        },
        **details,
        "state_revision": prior_revision + 1,
    }
    # The append-only event is authoritative.  The projection follows it, so a
    # crash between writes is repaired by load_v2_state instead of losing the
    # transition.
    _single_write_jsonl(
        task_dir / "events.jsonl",
        {
            "at": state["updated_at"],
            "event": "state",
            "state": state,
        },
    )
    if (
        os.environ.get("MURMUR_HARNESS_SELFTEST") == "1"
        and os.environ.get("MURMUR_HARNESS_SELFTEST_KILL_AFTER_STATE_EVENT")
        == status
    ):
        os.kill(os.getpid(), signal.SIGKILL)
    runtime.atomic_write_json(state_path, state)
    return state


def _last_state_event(task_dir: Path) -> Optional[Dict[str, Any]]:
    return verifier.last_state_event(task_dir)


def load_v2_state(task_dir: Path) -> Dict[str, Any]:
    return verifier.load_v2_state(task_dir)


def _valid_task_id(task_id: str) -> None:
    if not runtime.TASK_ID_RE.fullmatch(task_id):
        raise runtime.HarnessError(
            "task id must match [a-z0-9][a-z0-9._-]{1,63}"
        )


def load_v2_task(
    task_id: str, cwd: Path
) -> Tuple[Dict[str, Any], Path, Path]:
    _valid_task_id(task_id)
    primary, common = runtime.repo_context(cwd)
    verifier.require_no_git_replacements(primary)
    task_dir = v2_task_dir(common, task_id)
    contract = runtime.load_json(task_dir / "task.json")
    contract_schema: Optional[Mapping[str, Any]] = None
    if (task_dir / "events.jsonl").is_file():
        state = load_v2_state(task_dir)
        receipt_path = task_dir / "commit.json"
        if state.get("status") in {"COMMITTED", "CLOSED"} and receipt_path.is_file():
            receipt = runtime.load_json(receipt_path)
            attested_commit = str(receipt.get("commit_sha", ""))
            if not runtime.SHA1_RE.fullmatch(attested_commit):
                raise runtime.HarnessError("v2 committed receipt commit is malformed")
            if receipt.get("task_id") != task_id:
                raise runtime.HarnessError(
                    "v2 committed receipt task differs from its task store"
                )
            if receipt.get("contract_sha256") != contract.get(
                "contract_sha256"
            ):
                raise runtime.HarnessError(
                    "v2 committed receipt contract binding is stale"
                )
            contract_schema = verifier.attested_schema(
                primary, attested_commit, "v2-task"
            )
    verifier.validate_hashed_document(
        contract,
        "v2-task",
        "contract_sha256",
        "v2 task",
        schema=contract_schema,
    )
    if Path(str(contract["git_common_dir"])).resolve() != common.resolve():
        raise runtime.HarnessError("v2 task belongs to another Git common directory")
    return contract, task_dir, common


def _fetch_base(cwd: Path, requested: Optional[str]) -> str:
    if requested:
        return runtime.git(
            cwd,
            "rev-parse",
            "--verify",
            "--end-of-options",
            f"{requested}^{{commit}}",
        )
    config = runtime.load_config()
    default = str(config.get("default_base", "origin/murmur"))
    remote, separator, branch = default.partition("/")
    try:
        if separator and remote and branch:
            subprocess.run(
                ["git", "fetch", "--quiet", remote, branch],
                cwd=str(cwd),
                check=True,
                capture_output=True,
                timeout=int(config.get("base_fetch_timeout_seconds", 30)),
                env={
                    **os.environ,
                    "GIT_TERMINAL_PROMPT": "0",
                    "GIT_ASKPASS": "",
                    "SSH_ASKPASS": "",
                },
            )
        return runtime.git(
            cwd,
            "rev-parse",
            "--verify",
            "--end-of-options",
            f"{default}^{{commit}}",
        )
    except Exception as exc:  # noqa: BLE001 - explicit offline fallback
        print(
            f"agent-harness: WARNING — could not resolve {default} "
            f"({type(exc).__name__}); using local HEAD",
            file=sys.stderr,
        )
        return runtime.git(cwd, "rev-parse", "HEAD")


def _read_description(args: argparse.Namespace) -> str:
    prompt = getattr(args, "prompt", None)
    prompt_file = getattr(args, "prompt_file", None)
    if bool(prompt) == bool(prompt_file):
        raise runtime.HarnessError("provide exactly one of --prompt or --prompt-file")
    if prompt_file:
        path = Path(prompt_file)
        try:
            prompt = path.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as exc:
            raise runtime.HarnessError(f"cannot read prompt file {path}: {exc}") from exc
    result = str(prompt).strip()
    if not result:
        raise runtime.HarnessError("task prompt must not be empty")
    return result


def _protected_v2_paths(
    owned: Sequence[str], config: Optional[Mapping[str, Any]] = None
) -> List[str]:
    protected = [
        runtime.normalize_owned_path(path)
        for path in (config or runtime.load_config()).get("protected_paths", [])
    ]
    return sorted(
        path
        for path in owned
        if any(runtime.path_overlaps(path, guard) for guard in protected)
    )


def _link_node_modules(primary: Path, worktree: Path) -> Optional[str]:
    source = primary / "node_modules"
    target = worktree / "node_modules"
    if not source.is_dir() or source.is_symlink():
        return None
    ignored = runtime.run_capture(
        ["git", "check-ignore", "--quiet", "--no-index", "--", "node_modules/"],
        worktree,
        check=False,
    )
    if ignored.returncode != 0:
        raise runtime.HarnessError("node_modules is not ignored in the v2 worktree")
    target.symlink_to(source.resolve(), target_is_directory=True)
    return str(source.resolve())


def _local_branch_oid(primary: Path, branch: str) -> Optional[str]:
    branch_ref = f"refs/heads/{branch}"
    result = runtime.run_capture(
        [
            "git",
            "rev-parse",
            "--verify",
            "--quiet",
            "--end-of-options",
            branch_ref,
        ],
        primary,
        check=False,
    )
    if result.returncode == 1:
        return None
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip()
        raise runtime.HarnessError(
            f"cannot inspect local branch {branch}: {detail}"
        )
    oid = result.stdout.strip()
    if not runtime.SHA1_RE.fullmatch(oid):
        raise runtime.HarnessError(f"local branch {branch} has an invalid OID")
    return oid


def _create_open_branch(primary: Path, branch: str, base_sha: str) -> str:
    branch_ref = f"refs/heads/{branch}"
    result = runtime.run_capture(
        ["git", "update-ref", "--no-deref", branch_ref, base_sha, "0" * 40],
        primary,
        check=False,
    )
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip()
        raise runtime.HarnessError(
            f"could not create task branch {branch}: {detail}"
        )
    return base_sha


def _delete_open_branch_if_unchanged(
    primary: Path, branch: str, expected_oid: str
) -> None:
    branch_ref = f"refs/heads/{branch}"
    runtime.run_capture(
        ["git", "update-ref", "--no-deref", "-d", branch_ref, expected_oid],
        primary,
        check=False,
    )


def _standalone_driver_urls(driver: Path, *, push: bool) -> List[str]:
    argv = ["git", "remote", "get-url"]
    if push:
        argv.append("--push")
    argv.extend(["--all", "origin"])
    result = runtime.run_capture(argv, driver, check=False)
    urls = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    if result.returncode != 0 or not urls:
        raise runtime.HarnessError(
            "standalone driver origin must be canonical GitHub "
            "murmur-io/murmur HTTPS or SSH; local/file origins are forbidden"
        )
    return urls


def _standalone_driver_context(cwd: Path) -> Tuple[Path, Path]:
    top = Path(
        runtime.git(
            cwd,
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
        )
    ).resolve()
    primary, common = runtime.repo_context(cwd)
    if top.name != STANDALONE_DRIVER_NAME:
        raise runtime.HarnessError(
            "Harness v2 open requires the dedicated "
            f"{STANDALONE_DRIVER_NAME} standalone clone"
        )
    if top != primary:
        raise runtime.HarnessError(
            "standalone driver must be the primary worktree of its own "
            "Git common directory; linked driver worktrees are forbidden"
        )

    expected_common = top / ".git"
    git_dir = Path(
        runtime.git(
            cwd,
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
        )
    ).resolve()
    try:
        common_metadata = expected_common.lstat()
    except OSError as exc:
        raise runtime.HarnessError(
            "standalone driver Git common directory is missing"
        ) from exc
    if (
        stat.S_ISLNK(common_metadata.st_mode)
        or not stat.S_ISDIR(common_metadata.st_mode)
        or common != expected_common
        or git_dir != expected_common
    ):
        raise runtime.HarnessError(
            "standalone driver Git common directory must be exactly "
            f"{expected_common}"
        )

    symbolic_head = runtime.run_capture(
        ["git", "symbolic-ref", "--quiet", "HEAD"],
        top,
        check=False,
    )
    if symbolic_head.returncode == 0:
        raise runtime.HarnessError("standalone driver HEAD must be detached")
    if symbolic_head.returncode != 1:
        detail = (symbolic_head.stderr or symbolic_head.stdout).strip()
        raise runtime.HarnessError(
            f"cannot prove standalone driver HEAD is detached: {detail}"
        )
    runtime.git(top, "rev-parse", "--verify", "HEAD^{commit}")

    if runtime.git_bytes(
        top,
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
    ).strip():
        raise runtime.HarnessError("standalone driver must be clean before open")

    alternates = expected_common / "objects" / "info" / "alternates"
    try:
        alternates_metadata = alternates.lstat()
    except FileNotFoundError:
        alternates_metadata = None
    except OSError as exc:
        raise runtime.HarnessError(
            "cannot inspect standalone driver objects/info/alternates"
        ) from exc
    if alternates_metadata is not None:
        if (
            stat.S_ISLNK(alternates_metadata.st_mode)
            or not stat.S_ISREG(alternates_metadata.st_mode)
        ):
            raise runtime.HarnessError(
                "standalone driver objects/info/alternates must be absent "
                "or empty"
            )
        try:
            nonempty_alternates = bool(alternates.read_bytes())
        except OSError as exc:
            raise runtime.HarnessError(
                "cannot inspect standalone driver objects/info/alternates"
            ) from exc
        if nonempty_alternates:
            raise runtime.HarnessError(
                "standalone driver objects/info/alternates must be absent "
                "or empty"
            )

    origin_urls = _standalone_driver_urls(top, push=False)
    push_urls = _standalone_driver_urls(top, push=True)
    if any(
        url not in CANONICAL_MURMUR_ORIGINS
        for url in [*origin_urls, *push_urls]
    ):
        raise runtime.HarnessError(
            "standalone driver origin must be canonical GitHub "
            "murmur-io/murmur HTTPS or SSH; local/file origins are forbidden"
        )
    return top, common


def _require_safe_new_task_root(driver: Path, task_root: Path) -> None:
    task_parent = driver.parent / ".murmur-agent-tasks"
    expected = task_parent / "v2" / task_root.name
    if task_root != expected:
        raise runtime.HarnessError("v2 task root escaped its dedicated parent")

    for component in (driver.parent, task_parent, task_parent / "v2"):
        try:
            metadata = component.lstat()
        except FileNotFoundError:
            continue
        except OSError as exc:
            raise runtime.HarnessError(
                f"cannot inspect v2 task root component: {component}"
            ) from exc
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
            raise runtime.HarnessError(
                f"v2 task root component is unsafe or symlinked: {component}"
            )

    try:
        root_metadata = task_root.lstat()
    except FileNotFoundError:
        return
    except OSError as exc:
        raise runtime.HarnessError(
            f"cannot inspect v2 task root: {task_root}"
        ) from exc
    if stat.S_ISLNK(root_metadata.st_mode) or not stat.S_ISDIR(
        root_metadata.st_mode
    ):
        raise runtime.HarnessError(
            f"v2 task root is unsafe or symlinked: {task_root}"
        )
    raise runtime.HarnessError(f"v2 task root already exists: {task_root}")


def cmd_open(args: argparse.Namespace) -> int:
    cwd = Path.cwd()
    primary, common = _standalone_driver_context(cwd)
    verifier.require_no_git_replacements(primary)
    _require_sha1_repository(primary, label="v2 Harness driver")
    _valid_task_id(args.task_id)
    task_dir = v2_task_dir(common, args.task_id)
    task_root = primary.parent / ".murmur-agent-tasks" / "v2" / args.task_id
    _require_safe_new_task_root(primary, task_root)
    if task_dir.exists():
        raise runtime.HarnessError(f"task already exists: {args.task_id}")
    description = _read_description(args)
    owned = sorted(
        set(runtime.normalize_owned_path(path) for path in args.owned)
    )
    protected = _protected_v2_paths(owned)
    if protected:
        raise runtime.HarnessError(
            "the Harness cannot certify its own protected control plane "
            f"({', '.join(protected)}); use a dedicated worktree, run the full "
            "control-plane selftests, obtain a fresh independent review, and rely "
            "on the base-anchored CI gate"
        )
    claims = sorted(set(args.claim or []))
    reviewer = args.reviewer or str(runtime.load_config()["default_reviewer"])
    if reviewer not in runtime.REAL_MODEL_VENDORS:
        raise runtime.HarnessError("v2 reviewer must be codex or claude")
    if "runtime" in claims:
        runtime.runtime_preflight(primary)
    base_sha = _fetch_base(cwd, args.base)
    if not runtime.SHA1_RE.fullmatch(base_sha):
        raise runtime.HarnessError("v2 base did not resolve to a commit")
    executing_protocol = verifier.protocol_bundle(verifier.SOURCE_ROOT)
    base_protocol = verifier.protocol_bundle_at_commit(primary, base_sha)
    if base_protocol != executing_protocol:
        raise runtime.HarnessError(
            "v2 base protocol differs from the executing Harness; update the "
            "base before opening a task"
        )
    branch = args.branch or f"agent/v2/{args.task_id}"
    if branch in {"murmur", "main", "master"}:
        raise runtime.HarnessError("v2 task cannot use a protected branch")
    if (
        runtime.run_capture(
            ["git", "check-ref-format", "--branch", branch], cwd, check=False
        ).returncode
        != 0
    ):
        raise runtime.HarnessError(f"invalid task branch: {branch}")
    if _local_branch_oid(primary, branch) is not None:
        raise runtime.HarnessError(f"branch already exists: {branch}")
    worktree = task_root / CLIENT_WORKTREE_NAME
    server_worktree = task_root / "murmur-server"
    contract: Dict[str, Any] = {
        "schema_version": 2,
        "task_id": args.task_id,
        "description": description,
        "kind": args.kind,
        "base_sha": base_sha,
        "contract_sha256": "",
        "repo_realpath": str(primary.resolve()),
        "git_common_dir": str(common.resolve()),
        "worktree_path": str(worktree.resolve()),
        "branch": branch,
        "owned_paths": owned,
        "claims": claims,
        "reviewer": reviewer,
        "allow_same_vendor_high_risk": bool(
            getattr(args, "allow_same_vendor_high_risk", False)
        ),
        "expected_change": bool(args.expected_change),
        "created_at": runtime.utc_now(),
    }
    branch_created = False
    branch_expected_oid: Optional[str] = None
    task_dir_created = False
    task_root_created = False
    provisioned_server_worktree: Optional[Path] = None
    provisioned_server_mode: Optional[str] = None
    try:
        task_root.mkdir(parents=True)
        task_root_created = True
        task_dir.mkdir(parents=True)
        task_dir_created = True
        runtime.run_capture(["git", "worktree", "prune"], primary)
        branch_expected_oid = _create_open_branch(primary, branch, base_sha)
        branch_created = True
        runtime.run_capture(
            [
                "git",
                "-c",
                "core.hooksPath=/dev/null",
                "worktree",
                "add",
                str(worktree),
                branch,
            ],
            primary,
        )
        verifier.verify_raw_checkout(primary, base_sha, worktree)
        if verifier.protocol_bundle(worktree) != executing_protocol:
            raise runtime.HarnessError(
                "v2 checked-out protocol differs from the executing Harness"
            )
        unsafe = [
            path
            for path in owned
            if runtime.path_has_symlink_component(worktree, path)
        ]
        if unsafe:
            raise runtime.HarnessError(
                "owned paths traverse symlinks: " + ", ".join(unsafe)
            )
        shared_node_modules = _link_node_modules(primary, worktree)
        contract["contract_sha256"] = verifier.document_hash(
            contract, "contract_sha256"
        )
        runtime.validate_schema(
            contract, runtime.load_schema("v2-task"), label="v2 task"
        )
        runtime.atomic_write_json(task_dir / "task.json", contract)
        runtime.atomic_write_json(
            task_dir / "runtime.json",
            {
                "schema_version": 2,
                "task_root": str(task_root),
                "shared_node_modules": shared_node_modules,
                "server_worktree": None,
                "server_source": str(primary.parent / "murmur-server"),
                "server_revision": None,
                "server_checkout_mode": None,
            },
        )
        if runtime.has_murmur_server_path_dependency(worktree):
            provisioned_server_worktree = _ensure_server_worktree(
                contract,
                task_dir,
                {"server_required": True},
            )
            provisioned_runtime = runtime.load_json(task_dir / "runtime.json")
            provisioned_server_mode = str(
                provisioned_runtime["server_checkout_mode"]
            )
        set_v2_state(task_dir, "OPEN", phase="open")
    except Exception:
        if server_worktree.exists():
            # The task root was proven absent before OPEN created it, and the
            # server checkout is the one exact child created by this attempt.
            # Removing the local shared clone cannot mutate the sibling
            # repository's Git metadata and also handles a partially completed
            # clone whose runtime metadata was never written.
            try:
                metadata = server_worktree.lstat()
                if (
                    not stat.S_ISLNK(metadata.st_mode)
                    and stat.S_ISDIR(metadata.st_mode)
                    and server_worktree.parent.resolve() == task_root.resolve()
                ):
                    shutil.rmtree(server_worktree)
            except OSError:
                pass
        if worktree.exists():
            runtime.run_capture(
                ["git", "worktree", "remove", "--force", str(worktree)],
                primary,
                check=False,
            )
        if branch_created and branch_expected_oid is not None:
            _delete_open_branch_if_unchanged(
                primary, branch, branch_expected_oid
            )
        if task_dir_created:
            shutil.rmtree(task_dir, ignore_errors=True)
        if task_root_created:
            try:
                task_root.rmdir()
                task_root.parent.rmdir()
            except OSError:
                pass
        raise
    opened = {
        "task_id": args.task_id,
        "generation": 2,
        "status": "OPEN",
        "base_sha": base_sha,
        "worktree": str(worktree),
        "server_worktree": (
            str(provisioned_server_worktree)
            if provisioned_server_worktree is not None
            else None
        ),
    }
    if provisioned_server_mode is not None:
        opened["server_checkout_mode"] = provisioned_server_mode
    print(json.dumps(opened, indent=2))
    return 0


def prepare_plan(
    contract: Mapping[str, Any], task_dir: Path
) -> Tuple[Dict[str, Any], Path, bytes]:
    worktree = Path(str(contract["worktree_path"]))
    if "runtime" in contract.get("claims", []):
        runtime.runtime_preflight(worktree)
    state = load_v2_state(task_dir)
    if state.get("status") in verifier.V2_TERMINAL_STATES | {"COMMITTED"}:
        raise runtime.HarnessError(
            f"cannot plan v2 task in state {state.get('status')}"
        )
    paths, diff, tree_sha = verifier.snapshot_scoped_diff(
        worktree, contract, task_dir
    )
    if not paths or not diff:
        raise runtime.HarnessError(
            "Harness v2 verifies changes only; the exact diff is empty"
        )
    plan, bundle = verifier.build_plan(
        contract, worktree, paths, diff, tree_sha, runtime.load_config()
    )
    current_id = verifier.attempt_id(plan)
    attempt_dir = task_dir / "attempts" / current_id
    attempt_dir.mkdir(parents=True, exist_ok=True)
    plan_path = attempt_dir / "plan.json"
    if plan_path.is_file():
        existing = runtime.load_json(plan_path)
        verifier.validate_hashed_document(
            existing, "v2-plan", "plan_sha256", "v2 plan"
        )
        if existing != plan:
            raise runtime.HarnessError(
                "attempt-key collision: existing plan differs from the exact profile"
            )
    else:
        runtime.atomic_write_json(plan_path, plan)
        runtime.atomic_write_json(attempt_dir / "protocol.json", bundle)
        runtime.atomic_write_bytes(attempt_dir / "diff.patch", diff)
    set_v2_state(
        task_dir,
        "OPEN",
        phase="planned",
        attempt_id=current_id,
        plan_path=str(plan_path),
        diff_sha256=plan["diff_sha256"],
        plan_sha256=plan["plan_sha256"],
        protocol_sha256=plan["protocol_sha256"],
    )
    return plan, attempt_dir, diff


def _verification_snapshot_path(
    contract: Mapping[str, Any], attempt_dir: Path
) -> Path:
    worktree = Path(str(contract["worktree_path"])).resolve()
    attempts_dir = attempt_dir.parent
    if attempts_dir.name != "attempts":
        raise runtime.HarnessError("v2 attempt directory layout is malformed")
    if re.fullmatch(r"[0-9a-f]{64}", attempt_dir.name) is None:
        raise runtime.HarnessError("v2 attempt id is malformed")
    return worktree.parent / f"verify-{attempt_dir.name}"


def _verification_snapshot_ref(task_id: str, attempt_id: str) -> str:
    safe = re.sub(r"[^a-zA-Z0-9._-]+", "-", task_id).strip(".-")
    safe = safe.replace("..", "-") or "task"
    suffix = runtime.sha256_bytes(task_id.encode("utf-8"))[:12]
    return (
        f"refs/agent-harness/v2/snapshots/{safe}-{suffix}/{attempt_id}"
    )


def _anchor_verification_tree(
    contract: Mapping[str, Any],
    plan: Mapping[str, Any],
    attempt_dir: Path,
) -> Tuple[str, str]:
    """Keep private-index objects reachable across crashes and concurrent GC."""

    primary = Path(str(contract["repo_realpath"])).resolve()
    reference = _verification_snapshot_ref(
        str(contract["task_id"]), attempt_dir.name
    )
    current = runtime.git(
        primary, "rev-parse", "--verify", reference, check=False
    )
    if current:
        commit_sha = current
    else:
        identity = runtime.load_config()["commit_identity"]
        environment = {
            **os.environ,
            "GIT_AUTHOR_NAME": str(identity["name"]),
            "GIT_AUTHOR_EMAIL": str(identity["email"]),
            "GIT_COMMITTER_NAME": str(identity["name"]),
            "GIT_COMMITTER_EMAIL": str(identity["email"]),
            "GIT_AUTHOR_DATE": str(contract["created_at"]),
            "GIT_COMMITTER_DATE": str(contract["created_at"]),
        }
        completed = subprocess.run(
            [
                "git",
                "commit-tree",
                plan["tree_sha"],
                "-p",
                plan["base_sha"],
                "-m",
                f"harness v2 immutable snapshot: {contract['task_id']}",
            ],
            cwd=str(primary),
            env=environment,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if completed.returncode != 0:
            raise runtime.HarnessError(
                "could not anchor v2 verification tree: "
                + completed.stderr.strip()
            )
        commit_sha = completed.stdout.strip()
        if not runtime.SHA1_RE.fullmatch(commit_sha):
            raise runtime.HarnessError(
                "v2 verification anchor did not produce a commit"
            )
        # Create-only publication. A concurrent publisher may only win with
        # the same deterministic commit; otherwise update-ref fails closed.
        runtime.run_capture(
            ["git", "update-ref", reference, commit_sha, "0" * 40],
            primary,
        )
    if runtime.git(primary, "rev-parse", f"{commit_sha}^{{tree}}") != plan["tree_sha"]:
        raise runtime.HarnessError("v2 verification anchor tree is stale")
    parents = runtime.git(
        primary, "show", "-s", "--format=%P", commit_sha
    ).split()
    if parents != [plan["base_sha"]]:
        raise runtime.HarnessError("v2 verification anchor parent is stale")
    return reference, commit_sha


def _snapshot_manifest(
    contract: Mapping[str, Any],
    plan: Mapping[str, Any],
    snapshot: Path,
    snapshot_ref: str,
    snapshot_commit: str,
) -> Dict[str, Any]:
    document: Dict[str, Any] = {
        "schema_version": 2,
        "task_id": contract["task_id"],
        "contract_sha256": contract["contract_sha256"],
        "plan_sha256": plan["plan_sha256"],
        "base_sha": plan["base_sha"],
        "tree_sha": plan["tree_sha"],
        "diff_sha256": plan["diff_sha256"],
        "path": str(snapshot),
        "snapshot_ref": snapshot_ref,
        "snapshot_commit": snapshot_commit,
        "created_at": contract["created_at"],
        "snapshot_sha256": "",
    }
    document["snapshot_sha256"] = verifier.document_hash(
        document, "snapshot_sha256"
    )
    return document


def _validate_verification_snapshot(
    contract: Mapping[str, Any],
    task_dir: Path,
    plan: Mapping[str, Any],
    attempt_dir: Path,
    snapshot: Path,
) -> None:
    if not snapshot.is_dir() or snapshot.is_symlink():
        raise runtime.HarnessError("v2 verification snapshot is missing or unsafe")
    if snapshot.resolve() != _verification_snapshot_path(contract, attempt_dir):
        raise runtime.HarnessError("v2 verification snapshot escaped its task root")
    common = Path(
        runtime.git(
            snapshot,
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        )
    ).resolve()
    if common != (snapshot / ".git").resolve():
        raise runtime.HarnessError(
            "v2 verification snapshot unexpectedly shares mutable Git metadata"
        )
    alternates = common / "objects" / "info" / "alternates"
    try:
        alternates_metadata = alternates.lstat()
    except FileNotFoundError:
        alternates_metadata = None
    except OSError as exc:
        raise runtime.HarnessError(
            "cannot inspect v2 verification snapshot object alternates"
        ) from exc
    if alternates_metadata is not None:
        if (
            stat.S_ISLNK(alternates_metadata.st_mode)
            or not stat.S_ISREG(alternates_metadata.st_mode)
        ):
            raise runtime.HarnessError(
                "v2 verification snapshot object alternates must be absent "
                "or empty"
            )
        try:
            nonempty_alternates = bool(alternates.read_bytes())
        except OSError as exc:
            raise runtime.HarnessError(
                "cannot inspect v2 verification snapshot object alternates"
            ) from exc
        if nonempty_alternates:
            raise runtime.HarnessError(
                "v2 verification snapshot object alternates must be absent "
                "or empty"
            )
    if (
        Path(runtime.git(snapshot, "rev-parse", "--show-toplevel")).resolve()
        != snapshot.resolve()
    ):
        raise runtime.HarnessError("v2 verification snapshot is not its Git root")
    if runtime.git(snapshot, "rev-parse", "HEAD") != plan["base_sha"]:
        raise runtime.HarnessError("v2 verification snapshot parent changed")
    paths, diff, tree_sha = verifier.snapshot_scoped_diff(
        snapshot, contract, task_dir
    )
    if paths != plan["changed_paths"]:
        raise runtime.HarnessError("v2 verification snapshot paths changed")
    if runtime.sha256_bytes(diff) != plan["diff_sha256"]:
        raise runtime.HarnessError("v2 verification snapshot diff changed")
    if tree_sha != plan["tree_sha"]:
        raise runtime.HarnessError("v2 verification snapshot tree changed")


def _discard_claimed_verification_snapshot(
    contract: Mapping[str, Any], attempt_dir: Path, snapshot: Path
) -> None:
    if snapshot.resolve() != _verification_snapshot_path(contract, attempt_dir):
        raise runtime.HarnessError(
            "refusing to discard a verification snapshot outside its task root"
        )
    if not snapshot.exists() and not snapshot.is_symlink():
        return
    if not snapshot.is_dir() or snapshot.is_symlink():
        raise runtime.HarnessError(
            "claimed verification snapshot path became unsafe"
        )
    shutil.rmtree(snapshot)


def _ensure_verification_snapshot(
    contract: Mapping[str, Any],
    task_dir: Path,
    plan: Mapping[str, Any],
    attempt_dir: Path,
) -> Path:
    """Materialize the exact planned tree in a runner-owned standalone repo.

    Checks and reviewers never read the concurrently editable developer
    worktree.  The snapshot keeps its own index, HEAD, and object database so
    deterministic Seatbelt checks need no read access to the primary repo.
    """

    snapshot = _verification_snapshot_path(contract, attempt_dir)
    snapshot_ref, snapshot_commit = _anchor_verification_tree(
        contract, plan, attempt_dir
    )
    manifest_path = attempt_dir / "snapshot.json"
    expected_manifest = _snapshot_manifest(
        contract,
        plan,
        snapshot,
        snapshot_ref,
        snapshot_commit,
    )
    if manifest_path.is_file():
        manifest = runtime.load_json(manifest_path)
        if manifest != expected_manifest:
            raise runtime.HarnessError(
                "v2 verification snapshot manifest differs from the exact plan"
            )
        try:
            _validate_verification_snapshot(
                contract, task_dir, plan, attempt_dir, snapshot
            )
            return snapshot
        except runtime.HarnessError:
            # The durable manifest was published before clone creation. A
            # parent crash may leave a partial clone; discard only that exact
            # runner-claimed path and reconstruct from the anchored tree.
            _discard_claimed_verification_snapshot(
                contract, attempt_dir, snapshot
            )
    else:
        if snapshot.exists() or snapshot.is_symlink():
            raise runtime.HarnessError(
                "unclaimed v2 verification snapshot path already exists"
            )
        runtime.atomic_write_json(manifest_path, expected_manifest)
    primary = Path(str(contract["repo_realpath"])).resolve()
    if not primary.is_dir() or primary.is_symlink():
        raise runtime.HarnessError("v2 primary repository is missing or unsafe")
    try:
        local_snapshot_ref = "refs/agent-harness/v2/source"
        runtime.run_capture(
            [
                "git",
                "init",
                "--quiet",
                "--no-template",
                "--object-format=sha1",
                str(snapshot),
            ],
            snapshot.parent,
        )
        runtime.run_capture(
            [
                "git",
                "fetch",
                "--quiet",
                "--no-tags",
                "--depth=2",
                "--no-write-fetch-head",
                str(primary),
                f"{snapshot_ref}:{local_snapshot_ref}",
            ],
            snapshot,
        )
        fetched_commit = runtime.git(
            snapshot,
            "rev-parse",
            "--verify",
            "--end-of-options",
            f"{local_snapshot_ref}^{{commit}}",
        )
        if fetched_commit != snapshot_commit:
            raise runtime.HarnessError(
                "v2 verification snapshot fetched a stale anchor"
            )
        if (
            runtime.git(
                snapshot, "rev-parse", f"{fetched_commit}^{{tree}}"
            )
            != plan["tree_sha"]
        ):
            raise runtime.HarnessError(
                "v2 verification snapshot fetched a stale tree"
            )
        fetched_parents = runtime.git(
            snapshot, "show", "-s", "--format=%P", fetched_commit
        ).split()
        if fetched_parents != [plan["base_sha"]]:
            raise runtime.HarnessError(
                "v2 verification snapshot fetched a stale parent"
            )
        runtime.run_capture(
            ["git", "checkout", "--quiet", "--detach", plan["base_sha"]],
            snapshot,
        )
        runtime.run_capture(
            ["git", "read-tree", "--reset", "-u", plan["tree_sha"]],
            snapshot,
        )
        _validate_verification_snapshot(
            contract, task_dir, plan, attempt_dir, snapshot
        )
        _link_node_modules(primary, snapshot)
        shared_target = primary / "target"
        snapshot_target = snapshot / "target"
        if (
            shared_target.is_dir()
            and not shared_target.is_symlink()
            and not snapshot_target.exists()
            and not snapshot_target.is_symlink()
        ):
            snapshot_target.symlink_to(
                shared_target.resolve(), target_is_directory=True
            )
    except Exception:
        # The manifest is a durable ownership intent, so a later resume can
        # safely reconstruct even when this process dies mid-clone.
        _discard_claimed_verification_snapshot(
            contract, attempt_dir, snapshot
        )
        raise
    return snapshot


def cmd_plan(args: argparse.Namespace) -> int:
    contract, task_dir, _ = load_v2_task(args.task_id, Path.cwd())
    lock = acquire_v2_run_lock(task_dir, "plan")
    try:
        plan, attempt_dir, _ = prepare_plan(contract, task_dir)
    finally:
        release_v2_run_lock(lock)
    print(
        json.dumps(
            {
                "task_id": contract["task_id"],
                "attempt_id": attempt_dir.name,
                "changed_paths": plan["changed_paths"],
                "claims": plan["claims"],
                "actual_risk_flags": plan["actual_risk_flags"],
                "checks": [item["id"] for item in plan["checks"]],
                "reviews": plan["reviews"],
                "server_required": plan["server_required"],
                "diff_sha256": plan["diff_sha256"],
                "plan_sha256": plan["plan_sha256"],
                "protocol_sha256": plan["protocol_sha256"],
            },
            indent=2,
            sort_keys=True,
        )
    )
    return 0


def acquire_v2_run_lock(
    task_dir: Path, command: str
) -> runtime.TaskRunLock:
    """Use the one hardened create-only lock protocol for every operation.

    ``command`` remains part of the call-site API so diagnostics can name the
    attempted operation without inventing a second, weaker on-disk protocol.
    """

    del command
    return runtime.acquire_run_lock(task_dir)


def release_v2_run_lock(lock: runtime.TaskRunLock) -> None:
    runtime.release_run_lock(lock)


def _ensure_server_worktree(
    contract: Mapping[str, Any],
    task_dir: Path,
    plan: Mapping[str, Any],
    *,
    verification_worktree: Optional[Path] = None,
) -> Optional[Path]:
    if not plan.get("server_required"):
        return None
    worktree = verification_worktree or Path(str(contract["worktree_path"]))
    runtime_path = task_dir / "runtime.json"
    runtime_doc = runtime.load_json(runtime_path)
    server_worktree = worktree.parent / "murmur-server"
    server_source = Path(str(runtime_doc.get("server_source", "")))
    expected_source = Path(str(contract["repo_realpath"])).parent / "murmur-server"
    if server_source.resolve() != expected_source.resolve():
        raise runtime.HarnessError("v2 server source is not the canonical sibling repository")
    revision_path = worktree / ".murmur-server-revision"
    try:
        revision = revision_path.read_text(encoding="utf-8").strip()
    except (OSError, UnicodeError) as exc:
        raise runtime.HarnessError("v2 Rust/protocol check needs .murmur-server-revision") from exc
    if not runtime.SHA1_RE.fullmatch(revision):
        raise runtime.HarnessError(".murmur-server-revision is malformed")
    if not server_source.is_dir():
        raise runtime.HarnessError(f"pinned sibling server repository is missing: {server_source}")
    resolved = runtime.git(
        server_source,
        "rev-parse",
        "--verify",
        "--end-of-options",
        f"{revision}^{{commit}}",
    )
    if resolved != revision:
        raise runtime.HarnessError("pinned server revision did not resolve exactly")
    if server_worktree.exists():
        if not server_worktree.is_dir() or server_worktree.is_symlink():
            raise runtime.HarnessError("v2 server worktree path is unsafe")
        if runtime.git_bytes(server_worktree, "status", "--porcelain").strip():
            raise runtime.HarnessError("existing v2 server worktree is dirty")
        checkout_mode = str(
            runtime_doc.get("server_checkout_mode") or "linked-worktree"
        )
        if runtime.git(server_worktree, "rev-parse", "HEAD") != revision:
            if checkout_mode != "local-shared-clone":
                raise runtime.HarnessError(
                    "existing linked v2 server worktree is at another revision"
                )
            common = Path(
                runtime.git(
                    server_worktree,
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-common-dir",
                )
            ).resolve()
            if common != (server_worktree / ".git").resolve():
                raise runtime.HarnessError(
                    "existing v2 server clone shares mutable Git metadata"
                )
            shutil.rmtree(server_worktree)
    if not server_worktree.exists():
        # Do not register a linked worktree in the sibling repository. Agent
        # sandboxes correctly deny that cross-workspace .git mutation. A local
        # shared clone writes only inside the task root while reusing objects.
        runtime.run_capture(
            [
                "git",
                "clone",
                "--quiet",
                "--shared",
                "--no-checkout",
                str(server_source),
                str(server_worktree),
            ],
            worktree.parent,
        )
        runtime.run_capture(
            ["git", "checkout", "--quiet", "--detach", revision],
            server_worktree,
        )
        checkout_mode = "local-shared-clone"
    runtime_doc.update(
        {
            "server_worktree": str(server_worktree),
            "server_source": str(server_source.resolve()),
            "server_revision": revision,
            "server_checkout_mode": checkout_mode,
        }
    )
    runtime.atomic_write_json(runtime_path, runtime_doc)
    return server_worktree


def _checkpoint_event(
    task_dir: Path, event: str, **details: Any
) -> None:
    _single_write_jsonl(
        task_dir / "events.jsonl",
        {"at": runtime.utc_now(), "event": event, **details},
    )


def _load_bound_record(
    path: Path, plan: Mapping[str, Any]
) -> Optional[Dict[str, Any]]:
    if not path.is_file():
        return None
    try:
        record = runtime.load_json(path)
    except (runtime.HarnessError, OSError, UnicodeError):
        return None
    if not verifier.binding_matches(record, plan):
        return None
    return record


def _probe_record_sha256(record: Mapping[str, Any]) -> str:
    return runtime.sha256_bytes(runtime.canonical_json(record))


def _probe_witness_key(probes_dir: Path, probe_id: str) -> str:
    return f"{probes_dir.parent.name}/{probe_id}"


def _witness_probe_high_water(
    task_dir: Path,
    probes_dir: Path,
    probe_id: str,
    execution_number: int,
) -> None:
    state = load_v2_state(task_dir)
    raw = state.get("probe_high_water", {})
    if not isinstance(raw, Mapping) or any(
        not isinstance(key, str)
        or isinstance(value, bool)
        or not isinstance(value, int)
        or value < 1
        or value > verifier.MAX_PROBE_EXECUTIONS_PER_ID
        for key, value in raw.items()
    ):
        raise runtime.HarnessError("v2 probe high-water witness is malformed")
    witnesses = dict(raw)
    key = _probe_witness_key(probes_dir, probe_id)
    prior = int(witnesses.get(key, 0))
    if prior > execution_number:
        raise runtime.HarnessError(
            f"v2 probe event ledger was rewound: {probe_id}"
        )
    if prior == execution_number:
        return
    witnesses[key] = execution_number
    set_v2_state(
        task_dir,
        str(state["status"]),
        phase="probes",
        attempt_id=probes_dir.parent.name,
        probe_high_water=witnesses,
    )


def _probe_execution_state(
    task_dir: Path,
    probes_dir: Path,
    probe_id: str,
    declared: Mapping[str, Any],
    plan: Mapping[str, Any],
    *,
    allow_test_adapter: bool,
) -> Tuple[
    List[Dict[str, Any]],
    int,
    List[bool],
    List[Dict[str, Any]],
]:
    """Load completed records, reserved high-water, and legacy presence."""

    events_path = task_dir / "events.jsonl"
    records: Dict[int, Dict[str, Any]] = {}
    reservations: set[int] = set()
    reservation_contexts: Dict[int, List[Dict[str, Any]]] = {}
    legacy_event_outcomes: List[bool] = []
    expected_command_sha256 = runtime.sha256_bytes(
        str(declared["command"]).encode("utf-8")
    )
    try:
        with events_path.open(
            "r", encoding="utf-8", errors="strict"
        ) as handle:
            for line in handle:
                document = json.loads(line)
                if (
                    not isinstance(document, dict)
                    or document.get("attempt_id") != probes_dir.parent.name
                    or document.get("probe_id") != probe_id
                ):
                    continue
                event_kind = document.get("event")
                if event_kind not in {
                    "probe-execution-reserved",
                    "probe-checkpoint",
                }:
                    continue
                execution_number = document.get("execution_number")
                # Pre-high-water events remain compatible. A validated legacy
                # projection is migrated to a numbered event below.
                if execution_number is None:
                    if event_kind == "probe-checkpoint":
                        if not isinstance(document.get("passed"), bool):
                            raise runtime.HarnessError(
                                "legacy probe event outcome is malformed"
                            )
                        legacy_event_outcomes.append(
                            bool(document["passed"])
                        )
                    continue
                if (
                    isinstance(execution_number, bool)
                    or not isinstance(execution_number, int)
                    or execution_number < 1
                    or execution_number
                    > verifier.MAX_PROBE_EXECUTIONS_PER_ID
                ):
                    raise runtime.HarnessError(
                        f"v2 probe event number is malformed: {probe_id}"
                    )
                if (
                    document.get("plan_sha256")
                    != plan.get("plan_sha256")
                    or document.get("diff_sha256")
                    != plan.get("diff_sha256")
                ):
                    raise runtime.HarnessError(
                        f"v2 probe event binding changed: {probe_id}"
                    )
                if event_kind == "probe-execution-reserved":
                    contexts = document.get("request_contexts")
                    if (
                        document.get("check_id") != probe_id
                        or document.get("command_sha256")
                        != expected_command_sha256
                        or not isinstance(contexts, list)
                        or not contexts
                        or document.get("request_contexts_sha256")
                        != runtime.sha256_bytes(
                            runtime.canonical_json(contexts)
                        )
                        or any(
                            not isinstance(context, Mapping)
                            or context.get("probe_id") != probe_id
                            or context.get("context_sha256")
                            != verifier.document_hash(
                                context, "context_sha256"
                            )
                            for context in contexts
                        )
                        or execution_number in reservations
                    ):
                        raise runtime.HarnessError(
                            f"v2 probe reservation changed: {probe_id}"
                        )
                    reservations.add(execution_number)
                    reservation_contexts[execution_number] = [
                        dict(context) for context in contexts
                    ]
                    continue
                expected_path = probes_dir / f"{probe_id}.json"
                record = document.get("record")
                if (
                    not isinstance(record, Mapping)
                    or not isinstance(document.get("projection_path"), str)
                    or Path(
                        str(document.get("projection_path"))
                    ).absolute()
                    != expected_path.absolute()
                    or not isinstance(document.get("record_sha256"), str)
                    or runtime.SHA256_RE.fullmatch(
                        str(document.get("record_sha256"))
                    )
                    is None
                ):
                    raise runtime.HarnessError(
                        f"v2 probe event binding changed: {probe_id}"
                    )
                bound_record = dict(record)
                if (
                    bound_record.get("execution_number")
                    != execution_number
                    or _probe_record_sha256(bound_record)
                    != document["record_sha256"]
                    or not verifier.binding_matches(bound_record, plan)
                ):
                    raise runtime.HarnessError(
                        f"v2 probe event record changed: {probe_id}"
                    )
                verifier.validate_probe_checkpoint(
                    bound_record,
                    declared,
                    plan,
                    task_dir,
                    allow_test_adapter=allow_test_adapter,
                )
                if execution_number in records:
                    raise runtime.HarnessError(
                        f"v2 probe event number was reused: {probe_id}"
                    )
                records[execution_number] = bound_record
                if (
                    execution_number in reservation_contexts
                    and runtime.canonical_json(
                        bound_record.get("request_contexts")
                    )
                    != runtime.canonical_json(
                        reservation_contexts[execution_number]
                    )
                ):
                    raise runtime.HarnessError(
                        f"v2 probe completion context changed: {probe_id}"
                    )
                reservations.add(execution_number)
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise runtime.HarnessError(
            f"v2 probe event ledger is malformed: {events_path}: {exc}"
        ) from exc
    numbers = sorted(reservations)
    if numbers and numbers != list(range(1, max(numbers) + 1)):
        raise runtime.HarnessError(
            f"v2 probe event high-water is not contiguous: {probe_id}"
        )
    high_water = max(numbers, default=0)
    state = load_v2_state(task_dir)
    witnesses = state.get("probe_high_water", {})
    if not isinstance(witnesses, Mapping):
        raise runtime.HarnessError("v2 probe high-water witness is malformed")
    witnessed = witnesses.get(
        _probe_witness_key(probes_dir, probe_id), 0
    )
    if (
        isinstance(witnessed, bool)
        or not isinstance(witnessed, int)
        or witnessed < 0
        or witnessed > verifier.MAX_PROBE_EXECUTIONS_PER_ID
    ):
        raise runtime.HarnessError("v2 probe high-water witness is malformed")
    if witnessed > high_water:
        raise runtime.HarnessError(
            f"v2 probe event ledger was rewound: {probe_id}"
        )
    return (
        [records[number] for number in sorted(records)],
        high_water,
        legacy_event_outcomes,
        (
            reservation_contexts.get(high_water, [])
            if high_water not in records
            else []
        ),
    )


def _reserve_probe_execution(
    task_dir: Path,
    probes_dir: Path,
    probe_id: str,
    declared: Mapping[str, Any],
    plan: Mapping[str, Any],
    execution_number: int,
    request_contexts: Sequence[Mapping[str, Any]],
) -> None:
    contexts = [copy.deepcopy(dict(item)) for item in request_contexts]
    if not contexts:
        raise runtime.HarnessError("v2 probe reservation context is missing")
    _checkpoint_event(
        task_dir,
        "probe-execution-reserved",
        attempt_id=probes_dir.parent.name,
        probe_id=probe_id,
        execution_number=execution_number,
        check_id=probe_id,
        command_sha256=runtime.sha256_bytes(
            str(declared["command"]).encode("utf-8")
        ),
        request_contexts=contexts,
        request_contexts_sha256=runtime.sha256_bytes(
            runtime.canonical_json(contexts)
        ),
        plan_sha256=plan["plan_sha256"],
        diff_sha256=plan["diff_sha256"],
    )
    _witness_probe_high_water(
        task_dir, probes_dir, probe_id, execution_number
    )


def _append_probe_checkpoint(
    task_dir: Path,
    probes_dir: Path,
    probe_id: str,
    plan: Mapping[str, Any],
    record: Mapping[str, Any],
) -> None:
    evidence = record.get("evidence", {})
    _checkpoint_event(
        task_dir,
        "probe-checkpoint",
        attempt_id=probes_dir.parent.name,
        probe_id=probe_id,
        execution_number=record["execution_number"],
        projection_path=str(probes_dir / f"{probe_id}.json"),
        record=record,
        record_sha256=_probe_record_sha256(record),
        plan_sha256=plan["plan_sha256"],
        diff_sha256=plan["diff_sha256"],
        passed=bool(evidence.get("passed")),
    )


def _load_probe_checkpoint(
    probes_dir: Path,
    probe_id: str,
    declared: Mapping[str, Any],
    plan: Mapping[str, Any],
    task_dir: Path,
    *,
    allow_test_adapter: bool,
) -> Tuple[Optional[Dict[str, Any]], int, bool, List[Dict[str, Any]]]:
    """Load the append-only high-water and repair only its latest view."""

    projection_path = probes_dir / f"{probe_id}.json"
    projection = _load_bound_record(projection_path, plan)
    if projection is not None:
        verifier.validate_probe_checkpoint(
            projection,
            declared,
            plan,
            task_dir,
            allow_test_adapter=allow_test_adapter,
        )
    (
        records,
        high_water,
        legacy_event_outcomes,
        outstanding_contexts,
    ) = _probe_execution_state(
        task_dir,
        probes_dir,
        probe_id,
        declared,
        plan,
        allow_test_adapter=allow_test_adapter,
    )
    if high_water > 0:
        _witness_probe_high_water(
            task_dir, probes_dir, probe_id, high_water
        )
    if len(legacy_event_outcomes) > 1:
        raise runtime.HarnessError(
            "multiple legacy probe events cannot be migrated safely"
        )
    if legacy_event_outcomes and records:
        migrated_numbered = records[0]
        if (
            int(migrated_numbered["execution_number"]) != 1
            or bool(
                migrated_numbered.get("evidence", {}).get("passed")
            )
            != legacy_event_outcomes[0]
        ):
            raise runtime.HarnessError(
                "legacy probe outcome conflicts with numbered history"
            )
    if not records and (
        high_water == 0 or bool(legacy_event_outcomes)
    ):
        if projection is None:
            if legacy_event_outcomes:
                raise runtime.HarnessError(
                    "legacy probe event has no recoverable projection"
                )
            if projection_path.exists() or projection_path.is_symlink():
                raise runtime.HarnessError(
                    f"v2 probe projection is malformed: {probe_id}"
                )
            return None, 0, False, []
        migration_count = 1
        if high_water > migration_count:
            raise runtime.HarnessError(
                "legacy probe migration exceeds its execution history"
            )
        projection_number = projection.get("execution_number", 1)
        if projection_number != migration_count:
            raise runtime.HarnessError(
                "v2 probe projection has no append-only execution history"
            )
        migrated = {
            **projection,
            "execution_number": migration_count,
        }
        if (
            legacy_event_outcomes
            and bool(migrated.get("evidence", {}).get("passed"))
            != legacy_event_outcomes[0]
        ):
            raise runtime.HarnessError(
                "legacy probe outcome conflicts with its projection"
            )
        if (
            high_water == migration_count
            and runtime.canonical_json(outstanding_contexts)
            != runtime.canonical_json(migrated["request_contexts"])
        ):
            raise runtime.HarnessError(
                "legacy probe migration context changed"
            )
        for number in range(high_water + 1, migration_count + 1):
            _reserve_probe_execution(
                task_dir,
                probes_dir,
                probe_id,
                declared,
                plan,
                number,
                migrated["request_contexts"],
            )
        _append_probe_checkpoint(
            task_dir, probes_dir, probe_id, plan, migrated
        )
        records = [migrated]
        high_water = migration_count
        outstanding_contexts = []

    # The per-ID JSON is only a convenience projection. The append-only event
    # owns both the full record and its high-water, so rollback or deletion can
    # never lower the execution count.
    latest = records[-1] if records else None
    if latest is not None and (
        projection is None
        or runtime.canonical_json(projection)
        != runtime.canonical_json(latest)
    ):
        runtime.atomic_write_json(projection_path, latest)
    completed_numbers = {
        int(record["execution_number"]) for record in records
    }
    return (
        latest,
        high_water,
        high_water not in completed_numbers,
        outstanding_contexts,
    )


def _snapshot_still_matches(
    contract: Mapping[str, Any],
    task_dir: Path,
    plan: Mapping[str, Any],
) -> bool:
    paths, diff, tree_sha = verifier.snapshot_scoped_diff(
        Path(str(contract["worktree_path"])), contract, task_dir
    )
    return (
        paths == plan["changed_paths"]
        and runtime.sha256_bytes(diff) == plan["diff_sha256"]
        and tree_sha == plan["tree_sha"]
    )


def _run_or_resume_check(
    contract: Mapping[str, Any],
    task_dir: Path,
    plan: Mapping[str, Any],
    attempt_dir: Path,
    declared: Mapping[str, Any],
    verification_worktree: Path,
    *,
    checkpoint_number: int,
) -> Tuple[Dict[str, Any], bool]:
    record_path = attempt_dir / "checks" / f"{declared['id']}.json"
    record = _load_bound_record(record_path, plan)
    if record is not None:
        try:
            verifier.validate_check_checkpoint(
                record, declared, plan, task_dir
            )
        except runtime.HarnessError:
            record = None
    if record is not None:
        prior_evidence = record.get("evidence", {})
        # A successful or deterministic failed check is a reusable exact-diff
        # checkpoint. Environmental BLOCKED/timeout records are pause markers.
        if not (
            prior_evidence.get("outcome") == "BLOCKED"
            or prior_evidence.get("timed_out")
        ):
            return record, False
    evidence = runtime.run_check(
        verification_worktree,
        task_dir,
        declared,
        f"v2-{attempt_dir.name[:12]}",
        bound_environment=(
            {"MURMUR_HARNESS_BASE_SHA": str(plan["base_sha"])}
            if declared.get("id") == "npm-lock"
            else None
        ),
    )
    if not _snapshot_still_matches(
        {**contract, "worktree_path": str(verification_worktree)},
        task_dir,
        plan,
    ):
        evidence = {
            **evidence,
            "passed": False,
            "outcome": "FAIL",
            "tree_mutated": True,
            "blocked_reason": "runner-owned check changed the exact task diff",
        }
    record = verifier.check_record(declared, plan, evidence)
    runtime.atomic_write_json(record_path, record)
    _checkpoint_event(
        task_dir,
        "check-checkpoint",
        attempt_id=attempt_dir.name,
        check_id=declared["id"],
        record_path=str(record_path),
        passed=bool(evidence.get("passed")),
    )
    if (
        os.environ.get("MURMUR_HARNESS_SELFTEST") == "1"
        and os.environ.get("MURMUR_HARNESS_SELFTEST_KILL_AFTER_CHECK")
        == str(checkpoint_number)
    ):
        os.kill(os.getpid(), signal.SIGKILL)
    return record, True


def _reviews_require_fix(review_records: Sequence[Mapping[str, Any]]) -> bool:
    """Return true when evidence requests must not mask a review defect."""

    return any(
        verifier.review_result_state(record.get("result", {})) == "NEEDS_FIX"
        for record in review_records
    )


def verify_task(
    contract: Mapping[str, Any],
    task_dir: Path,
    *,
    allow_test_adapter: bool = False,
) -> str:
    lock = acquire_v2_run_lock(task_dir, "verify")
    try:
        state = load_v2_state(task_dir)
        if state.get("status") in verifier.V2_TERMINAL_STATES | {"COMMITTED"}:
            raise runtime.HarnessError(
                f"cannot verify v2 task in state {state.get('status')}"
            )
        plan, attempt_dir, diff = prepare_plan(contract, task_dir)
        verification_worktree = _ensure_verification_snapshot(
            contract, task_dir, plan, attempt_dir
        )
        set_v2_state(
            task_dir,
            "VERIFYING",
            phase="checks",
            attempt_id=attempt_dir.name,
            plan_path=str(attempt_dir / "plan.json"),
            diff_sha256=plan["diff_sha256"],
            plan_sha256=plan["plan_sha256"],
            protocol_sha256=plan["protocol_sha256"],
        )
        _ensure_server_worktree(
            contract,
            task_dir,
            plan,
            verification_worktree=verification_worktree,
        )
        worktree = verification_worktree
        check_dir = attempt_dir / "checks"
        check_dir.mkdir(parents=True, exist_ok=True)
        check_records: List[Dict[str, Any]] = []
        ran_checks = 0
        for declared in plan["checks"]:
            record, did_run = _run_or_resume_check(
                contract,
                task_dir,
                plan,
                attempt_dir,
                declared,
                verification_worktree,
                checkpoint_number=ran_checks + 1,
            )
            if did_run:
                ran_checks += 1
            check_records.append(record)
            if not _snapshot_still_matches(contract, task_dir, plan):
                set_v2_state(
                    task_dir,
                    "NEEDS_FIX",
                    phase="checks",
                    reason=(
                        "developer worktree changed during verification; "
                        "the completed snapshot checkpoint was preserved and "
                        "a new exact-diff attempt is required"
                    ),
                    attempt_id=attempt_dir.name,
                    plan_path=str(attempt_dir / "plan.json"),
                )
                return "NEEDS_FIX"

        check_state: Optional[str] = None
        check_reason = ""
        resource_wait_ms = sum(
            int(record.get("evidence", {}).get("resource_wait_ms", 0) or 0)
            for record in check_records
        )
        for record in check_records:
            evidence = record.get("evidence", {})
            if evidence.get("outcome") == "BLOCKED" or evidence.get("timed_out"):
                check_state = "PAUSED_RETRYABLE"
                check_reason = f"check {record.get('id')} is retryable"
                break
            if not evidence.get("passed"):
                check_state = "NEEDS_FIX"
                check_reason = f"check {record.get('id')} failed"
                break
        if check_state is not None:
            set_v2_state(
                task_dir,
                check_state,
                phase="checks",
                reason=check_reason,
                resource_wait_ms=resource_wait_ms,
                attempt_id=attempt_dir.name,
                plan_path=str(attempt_dir / "plan.json"),
            )
            return check_state

        set_v2_state(
            task_dir,
            "VERIFYING",
            phase="reviews",
            attempt_id=attempt_dir.name,
            plan_path=str(attempt_dir / "plan.json"),
            resource_wait_ms=resource_wait_ms,
        )
        reviews_dir = attempt_dir / "reviews"
        reviews_dir.mkdir(parents=True, exist_ok=True)
        probes_dir = attempt_dir / "probes"
        probes_dir.mkdir(parents=True, exist_ok=True)
        probe_records: List[Dict[str, Any]] = []
        probe_checkpoints: Dict[str, Dict[str, Any]] = {}
        outstanding_probe_contexts: Dict[str, List[Dict[str, Any]]] = {}
        deterministic_probe_failures: List[str] = []
        config = runtime.load_config()
        for probe_id in sorted(verifier.allowed_probe_ids(plan)):
            declared_probe = verifier.canonical_check(probe_id, config)
            (
                record,
                high_water,
                incomplete,
                reserved_contexts,
            ) = _load_probe_checkpoint(
                probes_dir,
                probe_id,
                declared_probe,
                plan,
                task_dir,
                allow_test_adapter=allow_test_adapter,
            )
            if record is not None:
                prior = record.get("evidence", {})
                if (
                    not prior.get("passed")
                    and prior.get("outcome") != "BLOCKED"
                    and not prior.get("timed_out")
                ):
                    deterministic_probe_failures.append(probe_id)
                    probe_checkpoints[probe_id] = record
                    continue
            if incomplete:
                if not reserved_contexts:
                    raise runtime.HarnessError(
                        f"v2 incomplete probe lost its request context: {probe_id}"
                    )
                outstanding_probe_contexts[probe_id] = reserved_contexts
                probe_checkpoints[probe_id] = {
                    "id": probe_id,
                    "execution_number": high_water,
                    "evidence": {
                        "passed": False,
                        "outcome": "BLOCKED",
                        "timed_out": True,
                        "blocked_reason": (
                            "probe execution was reserved but interrupted "
                            "before its checkpoint"
                        ),
                    },
                }
                continue
            if record is None:
                continue
            probe_checkpoints[probe_id] = record
            prior = record.get("evidence", {})
            if prior.get("outcome") == "BLOCKED" or prior.get("timed_out"):
                contexts = record.get("request_contexts")
                if not isinstance(contexts, list) or not contexts:
                    raise runtime.HarnessError(
                        f"v2 retryable probe lost its request context: {probe_id}"
                    )
                outstanding_probe_contexts[probe_id] = [
                    copy.deepcopy(dict(context)) for context in contexts
                ]
                continue
            probe_records.append(record)
        if deterministic_probe_failures:
            set_v2_state(
                task_dir,
                "NEEDS_FIX",
                phase="probes",
                reason=(
                    "deterministic reviewer probe failed: "
                    + ", ".join(deterministic_probe_failures)
                ),
                attempt_id=attempt_dir.name,
            )
            return "NEEDS_FIX"
        probe_records.sort(key=lambda item: str(item.get("id", "")))
        probe_hash = verifier.probe_evidence_hash(probe_records)
        records_by_kind: Dict[str, Dict[str, Any]] = {}
        missing: List[Mapping[str, str]] = []
        for declared in plan["reviews"]:
            record_path = reviews_dir / f"{declared['kind']}.json"
            record = _load_bound_record(record_path, plan)
            if record is not None:
                prompt = verifier.combined_review_prompt(
                    contract,
                    plan,
                    diff,
                    check_records,
                    str(declared["kind"]),
                    worktree,
                    task_dir,
                    probes=probe_records,
                )
                try:
                    verifier.validate_review_checkpoint(
                        record,
                        declared,
                        plan,
                        task_dir,
                        expected_prompt_sha256=runtime.sha256_bytes(
                            prompt.encode("utf-8")
                        ),
                        allow_test_adapter=allow_test_adapter,
                    )
                except runtime.HarnessError:
                    record = None
            if (
                record is None
                or record.get("probe_evidence_sha256") != probe_hash
            ):
                missing.append(declared)
            else:
                records_by_kind[declared["kind"]] = record

        retry_pauses: List[Tuple[str, verifier.ReviewPaused]] = []
        permanent_errors: List[Tuple[str, Exception]] = []
        if missing:
            max_workers = max(
                1,
                min(
                    3,
                    int(runtime.load_config().get("v2_max_parallel_reviews", 3)),
                    len(missing),
                ),
            )

            def run_review(declared: Mapping[str, str]) -> Dict[str, Any]:
                run_dir = attempt_dir / "review-runs" / declared["kind"]
                run_dir.mkdir(parents=True, exist_ok=True)
                return verifier.invoke_readonly_review(
                    contract=contract,
                    plan=plan,
                    worktree=worktree,
                    task_dir=task_dir,
                    attempt_dir=run_dir,
                    diff=diff,
                    checks=check_records,
                    probes=probe_records,
                    review=declared,
                    probe_evidence_sha256=probe_hash,
                    allow_test_adapter=allow_test_adapter,
                )

            with concurrent.futures.ThreadPoolExecutor(
                max_workers=max_workers,
                thread_name_prefix="murmur-v2-review",
            ) as pool:
                future_map = {
                    pool.submit(run_review, declared): declared for declared in missing
                }
                # Consume every future: specialist failures never short-circuit
                # another independent review.
                for future in concurrent.futures.as_completed(future_map):
                    declared = future_map[future]
                    kind = declared["kind"]
                    try:
                        record = future.result()
                    except verifier.ReviewPaused as exc:
                        retry_pauses.append((kind, exc))
                        continue
                    except Exception as exc:  # noqa: BLE001 - persist all reviewer failures
                        permanent_errors.append((kind, exc))
                        continue
                    record_path = reviews_dir / f"{kind}.json"
                    runtime.atomic_write_json(record_path, record)
                    records_by_kind[kind] = record
                    _checkpoint_event(
                        task_dir,
                        "review-checkpoint",
                        attempt_id=attempt_dir.name,
                        review_kind=kind,
                        record_path=str(record_path),
                        verdict=record["result"]["verdict"],
                    )
        if (
            not _snapshot_still_matches(contract, task_dir, plan)
            or not _snapshot_still_matches(
                {**contract, "worktree_path": str(verification_worktree)},
                task_dir,
                plan,
            )
        ):
            set_v2_state(
                task_dir,
                "NEEDS_FIX",
                phase="reviews",
                reason="review phase changed the exact diff",
                attempt_id=attempt_dir.name,
            )
            return "NEEDS_FIX"
        if retry_pauses:
            reason = "; ".join(
                f"{kind}: {exc}" for kind, exc in sorted(retry_pauses)
            )
            set_v2_state(
                task_dir,
                "PAUSED_RETRYABLE",
                phase="reviews",
                reason=reason,
                attempt_id=attempt_dir.name,
            )
            return "PAUSED_RETRYABLE"
        if permanent_errors:
            reason = "; ".join(
                f"{kind}: {type(exc).__name__}: {exc}"
                for kind, exc in sorted(permanent_errors)
            )
            set_v2_state(
                task_dir,
                "NEEDS_EVIDENCE",
                phase="reviews",
                reason=reason,
                attempt_id=attempt_dir.name,
            )
            return "NEEDS_EVIDENCE"

        review_records = [
            records_by_kind[item["kind"]] for item in plan["reviews"]
        ]
        requested_probe_ids = sorted(
            {
                str(request["probe_id"])
                for review in review_records
                for request in review.get("result", {}).get("probe_requests", [])
            }
            | set(outstanding_probe_contexts)
        )
        eligible_probe_ids = set(verifier.allowed_probe_ids(plan))
        invalid_probe_ids = sorted(
            set(requested_probe_ids) - eligible_probe_ids
        )
        if invalid_probe_ids:
            set_v2_state(
                task_dir,
                "NEEDS_EVIDENCE",
                phase="reviews",
                reason=(
                    "review requested probes outside the exact plan; no command "
                    f"was executed: {', '.join(invalid_probe_ids)}"
                ),
                attempt_id=attempt_dir.name,
            )
            return "NEEDS_EVIDENCE"
        request_contexts: Dict[str, List[Dict[str, Any]]] = {}
        for probe_id in requested_probe_ids:
            contexts = [
                *outstanding_probe_contexts.get(probe_id, []),
                *verifier.probe_request_contexts(
                    review_records, probe_id
                ),
            ]
            request_contexts[probe_id] = (
                verifier.canonical_probe_request_contexts(contexts)
            )
        missing_contexts = [
            probe_id
            for probe_id, contexts in request_contexts.items()
            if not contexts
        ]
        if missing_contexts:
            set_v2_state(
                task_dir,
                "NEEDS_EVIDENCE",
                phase="reviews",
                reason=(
                    "reviewer probe request provenance is missing; no command "
                    f"was executed: {', '.join(missing_contexts)}"
                ),
                attempt_id=attempt_dir.name,
            )
            return "NEEDS_EVIDENCE"
        # A typed probe can close an empirical proof gap, but it cannot repair a
        # reviewer-confirmed code or specification defect.  Resolve that
        # precedence before probe handling only when a probe was actually
        # requested.  Probe-free NEEDS_FIX reviews continue through the
        # pre-existing evidence/checkpoint path unchanged.
        if requested_probe_ids and _reviews_require_fix(review_records):
            set_v2_state(
                task_dir,
                "NEEDS_FIX",
                phase="reviews",
                reason="a review has unresolved FAIL/MAJOR/BLOCKER findings",
                attempt_id=attempt_dir.name,
            )
            return "NEEDS_FIX"
        missing_probe_ids: List[str] = []
        for probe_id in requested_probe_ids:
            existing = probe_checkpoints.get(probe_id)
            if existing is None:
                missing_probe_ids.append(probe_id)
                continue
            execution_number = int(existing.get("execution_number", 1))
            prior_evidence = existing.get("evidence", {})
            if (
                prior_evidence.get("outcome") == "BLOCKED"
                or prior_evidence.get("timed_out")
            ):
                if (
                    execution_number
                    < verifier.MAX_PROBE_EXECUTIONS_PER_ID
                ):
                    missing_probe_ids.append(probe_id)
            # One green deterministic execution is sufficient for this exact
            # plan/diff. Fresh reviewers receive it; rephrased rationales never
            # create another process. Only retryable infrastructure outcomes
            # consume the one bounded retry above.
        if missing_probe_ids:
            if any(
                probe_id in {"rust-lib", "protocol-server"}
                for probe_id in missing_probe_ids
            ):
                _ensure_server_worktree(
                    contract,
                    task_dir,
                    {**plan, "server_required": True},
                    verification_worktree=verification_worktree,
                )
            probe_pause = False
            probe_failure = False
            for probe_id in missing_probe_ids:
                declared = verifier.canonical_check(probe_id, config)
                record_path = probes_dir / f"{probe_id}.json"
                execution_number = (
                    int(
                        probe_checkpoints[probe_id].get(
                            "execution_number", 1
                        )
                    )
                    if probe_id in probe_checkpoints
                    else 0
                ) + 1
                if (
                    execution_number
                    > verifier.MAX_PROBE_EXECUTIONS_PER_ID
                ):
                    raise runtime.HarnessError(
                        f"v2 probe execution budget exhausted: {probe_id}"
                    )
                _reserve_probe_execution(
                    task_dir,
                    probes_dir,
                    probe_id,
                    declared,
                    plan,
                    execution_number,
                    request_contexts[probe_id],
                )
                evidence = runtime.run_check(
                    worktree,
                    task_dir,
                    declared,
                    f"v2-{attempt_dir.name[:12]}-probe",
                    bound_environment=(
                        {"MURMUR_HARNESS_BASE_SHA": str(plan["base_sha"])}
                        if probe_id == "npm-lock"
                        else None
                    ),
                )
                if not _snapshot_still_matches(
                    {
                        **contract,
                        "worktree_path": str(verification_worktree),
                    },
                    task_dir,
                    plan,
                ):
                    evidence = {
                        **evidence,
                        "passed": False,
                        "outcome": "FAIL",
                        "tree_mutated": True,
                        "blocked_reason": "runner-owned probe changed the exact task diff",
                    }
                record = {
                    **verifier.check_record(declared, plan, evidence),
                    "source": "reviewer-probe",
                    "request_contexts": request_contexts[probe_id],
                    "execution_number": execution_number,
                }
                _append_probe_checkpoint(
                    task_dir, probes_dir, probe_id, plan, record
                )
                runtime.atomic_write_json(record_path, record)
                if not _snapshot_still_matches(contract, task_dir, plan):
                    set_v2_state(
                        task_dir,
                        "NEEDS_FIX",
                        phase="probes",
                        reason=(
                            "developer worktree changed while a snapshot probe "
                            "ran; checkpoint preserved for the old attempt"
                        ),
                        attempt_id=attempt_dir.name,
                    )
                    return "NEEDS_FIX"
                if evidence.get("outcome") == "BLOCKED" or evidence.get("timed_out"):
                    probe_pause = True
                elif not evidence.get("passed"):
                    probe_failure = True
            if probe_failure:
                set_v2_state(
                    task_dir,
                    "NEEDS_FIX",
                    phase="probes",
                    reason="a runner-owned reviewer probe failed",
                    attempt_id=attempt_dir.name,
                )
                return "NEEDS_FIX"
            if probe_pause:
                set_v2_state(
                    task_dir,
                    "PAUSED_RETRYABLE",
                    phase="probes",
                    reason="a runner-owned reviewer probe is retryable",
                    attempt_id=attempt_dir.name,
                )
                return "PAUSED_RETRYABLE"
            set_v2_state(
                task_dir,
                "NEEDS_EVIDENCE",
                phase="probes",
                reason=(
                    "context-bound probe evidence collected; resume will run "
                    "fresh reviews against its command, output, rationale, and "
                    "prior proof gaps"
                ),
                attempt_id=attempt_dir.name,
            )
            return "NEEDS_EVIDENCE"
        if requested_probe_ids:
            set_v2_state(
                task_dir,
                "NEEDS_EVIDENCE",
                phase="probes",
                reason=(
                    "review still requests a probe it already saw; no arbitrary "
                    "or repeated command was executed"
                ),
                attempt_id=attempt_dir.name,
            )
            return "NEEDS_EVIDENCE"
        evidence = verifier.build_evidence(
            contract,
            plan,
            worktree,
            check_records,
            probe_records,
            review_records,
        )
        evidence_path = attempt_dir / "evidence.json"
        runtime.atomic_write_json(evidence_path, evidence)
        _checkpoint_event(
            task_dir,
            "evidence-checkpoint",
            attempt_id=attempt_dir.name,
            evidence_path=str(evidence_path),
            verdict=evidence["verdict"],
            evidence_sha256=evidence["evidence_sha256"],
            resource_wait_ms=resource_wait_ms
            + sum(
                int(record.get("evidence", {}).get("resource_wait_ms", 0) or 0)
                for record in probe_records
            ),
        )
        new_state = evidence["verdict"]
        set_v2_state(
            task_dir,
            new_state,
            phase="complete",
            reason=evidence["reason"],
            attempt_id=attempt_dir.name,
            plan_path=str(attempt_dir / "plan.json"),
            evidence_path=str(evidence_path),
            diff_sha256=evidence["diff_sha256"],
            plan_sha256=evidence["plan_sha256"],
            protocol_sha256=evidence["protocol_sha256"],
            evidence_sha256=evidence["evidence_sha256"],
        )
        if new_state == "PASSED":
            try:
                verifier.verify_v2_evidence(
                    contract,
                    task_dir,
                    allow_test_adapter=allow_test_adapter,
                )
            except Exception as exc:
                set_v2_state(
                    task_dir,
                    "NEEDS_EVIDENCE",
                    phase="receipt",
                    reason=f"exact evidence verification failed: {exc}",
                )
                raise
        return new_state
    except (KeyboardInterrupt, runtime.HarnessCancellation):
        current = load_v2_state(task_dir)
        if current.get("status") not in verifier.V2_TERMINAL_STATES:
            set_v2_state(
                task_dir,
                "INTERRUPTED",
                phase=current.get("phase", "verify"),
                reason="verifier interrupted; resume preserves completed checkpoints",
            )
        raise
    except runtime.HarnessError as exc:
        current = load_v2_state(task_dir)
        if current.get("status") == "VERIFYING":
            set_v2_state(
                task_dir,
                "NEEDS_EVIDENCE",
                phase=current.get("phase", "verify"),
                reason=str(exc),
            )
        raise
    finally:
        release_v2_run_lock(lock)


def cmd_verify(args: argparse.Namespace) -> int:
    contract, task_dir, _ = load_v2_task(args.task_id, Path.cwd())
    result = verify_task(contract, task_dir)
    print(json.dumps(v2_status(contract, task_dir), indent=2, sort_keys=True))
    return 0 if result == "PASSED" else 1


def cmd_resume(args: argparse.Namespace) -> int:
    contract, task_dir, _ = load_v2_task(args.task_id, Path.cwd())
    result = verify_task(contract, task_dir)
    print(json.dumps(v2_status(contract, task_dir), indent=2, sort_keys=True))
    return 0 if result == "PASSED" else 1


def _visible_tree(worktree: Path, runtime_dir: Path) -> str:
    runtime_dir.mkdir(parents=True, exist_ok=True)
    import tempfile

    descriptor, raw_index = tempfile.mkstemp(
        prefix="visible-tree-", dir=str(runtime_dir)
    )
    os.close(descriptor)
    index = Path(raw_index)
    environment = {**os.environ, "GIT_INDEX_FILE": str(index)}
    try:
        index.unlink(missing_ok=True)
        subprocess.run(
            ["git", "read-tree", "HEAD"],
            cwd=str(worktree),
            env=environment,
            check=True,
            capture_output=True,
        )
        subprocess.run(
            ["git", "add", "-A", "--", "."],
            cwd=str(worktree),
            env=environment,
            check=True,
            capture_output=True,
        )
        return subprocess.run(
            ["git", "write-tree"],
            cwd=str(worktree),
            env=environment,
            check=True,
            text=True,
            capture_output=True,
        ).stdout.strip()
    except subprocess.CalledProcessError as exc:
        detail = (
            exc.stderr.decode("utf-8", "replace")
            if isinstance(exc.stderr, bytes)
            else str(exc.stderr)
        )
        raise runtime.HarnessError(
            f"could not reconstruct visible tree: {detail}"
        ) from exc
    finally:
        index.unlink(missing_ok=True)


def _stage_v2_commit(
    contract: Mapping[str, Any], task_dir: Path, evidence: Mapping[str, Any]
) -> None:
    worktree = Path(str(contract["worktree_path"]))
    # This is the one intentional real-index mutation in v2.  It happens only
    # after exact evidence is verified, immediately before commit.
    runtime.git(worktree, "reset", "--quiet", "HEAD", "--", ".")
    if evidence["changed_paths"]:
        runtime.git(worktree, "add", "-A", "--", *contract["owned_paths"])
    diff = runtime.staged_diff(worktree)
    if runtime.sha256_bytes(diff) != evidence["diff_sha256"]:
        raise runtime.HarnessError("real staged index differs from v2 evidence")
    if runtime.git(worktree, "write-tree") != evidence["tree_sha"]:
        raise runtime.HarnessError("real staged tree differs from v2 evidence")


def _v2_commit_message(message: str, evidence: Mapping[str, Any]) -> str:
    subject = message.strip()
    if not subject or "\x00" in subject:
        raise runtime.HarnessError("commit message must be non-empty and contain no NUL")
    if re.search(
        r"(?im)^\s*(?:Harness-(?:Version|Task|Verdict|Base|Diff-Sha256|"
        r"Evidence-Sha256|Attestation-Sha256)|Co-Authored-By):",
        subject,
    ):
        raise runtime.HarnessError(
            "commit message must not contain receipt or co-author trailers"
        )
    trailers = [
        "Harness-Version: 2",
        f"Harness-Task: {evidence['task_id']}",
        "Harness-Verdict: PASS",
        f"Harness-Base: {evidence['parent_sha']}",
        f"Harness-Diff-Sha256: {evidence['diff_sha256']}",
        f"Harness-Evidence-Sha256: {evidence['evidence_sha256']}",
        f"Harness-Attestation-Sha256: {evidence['evidence_sha256']}",
    ]
    return subject + "\n\n" + "\n".join(trailers)


def _strict_v2_receipt_trailers(
    message: str, evidence: Mapping[str, Any]
) -> Dict[str, str]:
    keys = {
        "Harness-Version",
        "Harness-Task",
        "Harness-Verdict",
        "Harness-Base",
        "Harness-Diff-Sha256",
        "Harness-Evidence-Sha256",
        "Harness-Attestation-Sha256",
    }
    values: Dict[str, str] = {}
    for line in message.splitlines():
        match = re.fullmatch(r"(Harness-[A-Za-z0-9-]+): ([^\r\n]+)", line)
        if match is None:
            if re.match(r"(?i)^\s*Harness-", line):
                raise runtime.HarnessError(
                    "v2 commit contains a malformed receipt trailer"
                )
            continue
        key, value = match.groups()
        if key not in keys:
            raise runtime.HarnessError(
                f"v2 commit contains an unknown receipt trailer: {key}"
            )
        if key in values:
            raise runtime.HarnessError(
                f"v2 commit contains duplicate receipt trailer: {key}"
            )
        values[key] = value
    expected = {
        "Harness-Version": "2",
        "Harness-Task": str(evidence["task_id"]),
        "Harness-Verdict": "PASS",
        "Harness-Base": str(evidence["parent_sha"]),
        "Harness-Diff-Sha256": str(evidence["diff_sha256"]),
        "Harness-Evidence-Sha256": str(evidence["evidence_sha256"]),
        "Harness-Attestation-Sha256": str(evidence["evidence_sha256"]),
    }
    if values != expected:
        raise runtime.HarnessError(
            "v2 commit trailers do not exactly match the attested evidence"
        )
    return values


def cmd_v2_guard_commit(
    contract: Mapping[str, Any],
    task_dir: Path,
    *,
    allow_test_adapter: bool = False,
) -> Dict[str, Any]:
    evidence = verifier.verify_v2_evidence(
        contract, task_dir, allow_test_adapter=allow_test_adapter
    )
    _stage_v2_commit(contract, task_dir, evidence)
    return evidence


def _commit_intent(
    contract: Mapping[str, Any],
    task_dir: Path,
    evidence: Mapping[str, Any],
    message: str,
) -> Dict[str, Any]:
    intent: Dict[str, Any] = {
        "schema_version": 2,
        "task_id": contract["task_id"],
        "contract_sha256": contract["contract_sha256"],
        "evidence_sha256": evidence["evidence_sha256"],
        "parent_sha": evidence["parent_sha"],
        "tree_sha": evidence["tree_sha"],
        "diff_sha256": evidence["diff_sha256"],
        "message": message,
        "message_sha256": runtime.sha256_bytes(message.encode("utf-8")),
        # Intent is deterministically bound to the immutable PASS evidence so
        # retrying before/after git commit reproduces the exact same artifact.
        "created_at": evidence["created_at"],
        "intent_sha256": "",
    }
    intent["intent_sha256"] = verifier.document_hash(intent, "intent_sha256")
    path = task_dir / "commit-intent.json"
    if path.is_file():
        existing = runtime.load_json(path)
        verifier.validate_hashed_document(
            existing,
            "v2-commit-intent",
            "intent_sha256",
            "v2 commit intent",
        )
        if existing != intent:
            raise runtime.HarnessError(
                "existing v2 commit intent differs; resume with the exact original message"
            )
        return existing
    runtime.validate_schema(
        intent,
        runtime.load_schema("v2-commit-intent"),
        label="v2 commit intent",
    )
    runtime.atomic_write_json(path, intent)
    return intent


def _validate_v2_commit_head(
    worktree: Path,
    evidence: Mapping[str, Any],
    intent: Mapping[str, Any],
    expected_identity: Mapping[str, str],
) -> Dict[str, Any]:
    commit_sha = runtime.git(worktree, "rev-parse", "HEAD")
    parent_sha = runtime.git(worktree, "rev-parse", "HEAD^")
    tree_sha = runtime.git(worktree, "rev-parse", "HEAD^{tree}")
    actual_diff = runtime.git_bytes(
        worktree,
        "diff",
        "--binary",
        "--full-index",
        "--no-ext-diff",
        "--no-renames",
        parent_sha,
        commit_sha,
        "--",
    )
    author = {
        "name": runtime.git(worktree, "log", "-1", "--format=%an"),
        "email": runtime.git(worktree, "log", "-1", "--format=%ae"),
    }
    committer = {
        "name": runtime.git(worktree, "log", "-1", "--format=%cn"),
        "email": runtime.git(worktree, "log", "-1", "--format=%ce"),
    }
    actual_message = runtime.git(
        worktree, "log", "-1", "--format=%B"
    ).rstrip("\n")
    if parent_sha != evidence["parent_sha"] or parent_sha != intent["parent_sha"]:
        raise runtime.HarnessError("v2 commit actual parent differs from evidence/intent")
    if tree_sha != evidence["tree_sha"] or tree_sha != intent["tree_sha"]:
        raise runtime.HarnessError("v2 commit tree differs from evidence/intent")
    diff_sha = runtime.sha256_bytes(actual_diff)
    if diff_sha != evidence["diff_sha256"] or diff_sha != intent["diff_sha256"]:
        raise runtime.HarnessError("v2 commit diff differs from evidence/intent")
    if actual_message != intent["message"]:
        raise runtime.HarnessError("v2 commit message differs from the durable intent")
    if runtime.sha256_bytes(actual_message.encode("utf-8")) != intent["message_sha256"]:
        raise runtime.HarnessError("v2 commit message hash differs from the durable intent")
    if author != expected_identity or committer != expected_identity:
        raise runtime.HarnessError("v2 commit author/committer is not QueaT")
    if runtime.git_bytes(worktree, "status", "--porcelain").strip():
        raise runtime.HarnessError("v2 committed worktree/index is not clean")
    return {
        "commit_sha": commit_sha,
        "parent_sha": parent_sha,
        "tree_sha": tree_sha,
        "diff_sha256": diff_sha,
        "author": author,
        "committer": committer,
        "message": actual_message,
    }


def cmd_v2_commit(args: argparse.Namespace) -> int:
    contract, task_dir, _ = load_v2_task(args.task_id, Path.cwd())
    lock = acquire_v2_run_lock(task_dir, "commit")
    try:
        return _cmd_v2_commit_locked(args, contract, task_dir)
    finally:
        release_v2_run_lock(lock)


def _cmd_v2_commit_locked(
    args: argparse.Namespace,
    contract: Mapping[str, Any],
    task_dir: Path,
) -> int:
    allow_test_adapter = bool(
        getattr(args, "_allow_test_adapter", False)
    )
    state = load_v2_state(task_dir)
    if state.get("status") == "COMMITTED":
        receipt = verify_v2_committed(
            contract,
            task_dir,
            allow_test_adapter=allow_test_adapter,
        )
        print(
            json.dumps(
                {
                    "task_id": contract["task_id"],
                    "generation": 2,
                    "status": "COMMITTED",
                    "commit_sha": receipt["commit_sha"],
                    "idempotent": True,
                },
                indent=2,
            )
        )
        return 0
    if state.get("status") != "PASSED":
        raise runtime.HarnessError("only a PASSED v2 task can be committed")
    worktree = Path(str(contract["worktree_path"]))
    identity = runtime.load_config().get("commit_identity", {})
    name = identity.get("name") if isinstance(identity, Mapping) else None
    email = identity.get("email") if isinstance(identity, Mapping) else None
    if (name, email) != ("QueaT", "kgm004a@gmail.com"):
        raise runtime.HarnessError("v2 commit identity contract changed")
    expected_identity = {"name": name, "email": email}
    current_head = runtime.git(worktree, "rev-parse", "HEAD")
    if current_head == contract["base_sha"]:
        evidence = cmd_v2_guard_commit(
            contract,
            task_dir,
            allow_test_adapter=allow_test_adapter,
        )
        message = _v2_commit_message(args.message, evidence)
        intent = _commit_intent(contract, task_dir, evidence, message)
        runtime.run_capture(
            [
                "git",
                "-c",
                f"user.name={name}",
                "-c",
                f"user.email={email}",
                "commit",
                "-m",
                message,
            ],
            worktree,
        )
        if (
            os.environ.get("MURMUR_HARNESS_SELFTEST") == "1"
            and os.environ.get("MURMUR_HARNESS_SELFTEST_KILL_AFTER_GIT_COMMIT")
            == "1"
        ):
            os.kill(os.getpid(), signal.SIGKILL)
    else:
        intent = runtime.load_json(task_dir / "commit-intent.json")
        verifier.validate_hashed_document(
            intent,
            "v2-commit-intent",
            "intent_sha256",
            "v2 commit intent",
        )
        evidence = verifier.verify_v2_evidence(
            contract,
            task_dir,
            allow_test_adapter=allow_test_adapter,
            allow_committed_head=True,
        )
        message = _v2_commit_message(args.message, evidence)
        if intent.get("message") != message:
            raise runtime.HarnessError(
                "commit recovery requires the exact original commit message"
            )
    committed = _validate_v2_commit_head(
        worktree, evidence, intent, expected_identity
    )
    commit_sha = committed["commit_sha"]
    parent_sha = committed["parent_sha"]
    tree_sha = committed["tree_sha"]
    author = committed["author"]
    committer = committed["committer"]
    receipt = {
        "schema_version": 2,
        "task_id": contract["task_id"],
        "contract_sha256": contract["contract_sha256"],
        "evidence_sha256": evidence["evidence_sha256"],
        "commit_sha": commit_sha,
        "parent_sha": parent_sha,
        "tree_sha": tree_sha,
        "diff_sha256": evidence["diff_sha256"],
        "author": author,
        "committer": committer,
        "message": committed["message"],
        "authored_at": runtime.git(worktree, "log", "-1", "--format=%aI"),
        "committed_at": runtime.git(worktree, "log", "-1", "--format=%cI"),
        "recorded_at": runtime.utc_now(),
    }
    runtime.validate_schema(
        receipt, runtime.load_schema("v2-commit"), label="v2 commit receipt"
    )
    runtime.atomic_write_json(task_dir / "commit.json", receipt)
    set_v2_state(
        task_dir,
        "COMMITTED",
        phase="commit",
        commit_sha=commit_sha,
        parent_sha=parent_sha,
        tree_sha=tree_sha,
        evidence_path=load_v2_state(task_dir).get("evidence_path"),
        evidence_sha256=evidence["evidence_sha256"],
    )
    print(
        json.dumps(
            {
                "task_id": contract["task_id"],
                "generation": 2,
                "status": "COMMITTED",
                "commit_sha": commit_sha,
            },
            indent=2,
        )
    )
    return 0


def _v2_archive_ref(task_id: str) -> str:
    safe = re.sub(r"[^a-zA-Z0-9._-]+", "-", task_id).strip(".-")
    safe = safe.replace("..", "-") or "task"
    suffix = runtime.sha256_bytes(task_id.encode("utf-8"))[:12]
    return f"refs/agent-harness/v2/archive/{safe}-{suffix}"


def _verify_tree_matches_raw_bytes(
    worktree: Path,
    tree: str,
    *,
    label: str,
) -> None:
    """Prove a Git tree stores each visible filesystem leaf byte-for-byte."""

    listed = subprocess.run(
        [
            "git",
            "--no-replace-objects",
            "ls-tree",
            "-r",
            "-z",
            "--full-tree",
            tree,
        ],
        cwd=str(worktree),
        env={**os.environ, "GIT_NO_REPLACE_OBJECTS": "1"},
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if listed.returncode != 0:
        raise runtime.HarnessError(
            f"v2 clean {label} tree is unreadable for raw-byte proof"
        )
    archived: Dict[str, Tuple[str, str]] = {}
    for raw_record in (
        record for record in listed.stdout.split(b"\0") if record
    ):
        try:
            header, raw_path = raw_record.split(b"\t", 1)
            mode, kind, object_id = header.decode("ascii").split()
            relative = raw_path.decode("utf-8", "surrogateescape")
        except (UnicodeDecodeError, ValueError) as exc:
            raise runtime.HarnessError(
                f"v2 clean {label} tree entry is malformed"
            ) from exc
        if (
            kind != "blob"
            or mode not in {"100644", "100755", "120000"}
            or not runtime.SHA1_RE.fullmatch(object_id)
        ):
            raise runtime.HarnessError(
                f"v2 clean {label} has an unsupported entry: {relative}"
            )
        archived[relative] = (mode, object_id)

    visible = {
        relative
        for relative in _clean_visible_paths(worktree)
        if (
            (worktree / relative).exists()
            or (worktree / relative).is_symlink()
        )
    }
    if set(archived) != visible:
        extra = sorted(set(archived) - visible)
        missing = sorted(visible - set(archived))
        raise runtime.HarnessError(
            f"v2 clean {label} path set differs from raw filesystem bytes"
            f" (extra={extra[:10]}, missing={missing[:10]})"
        )

    stable_fields = (
        "st_dev",
        "st_ino",
        "st_mode",
        "st_nlink",
        "st_size",
        "st_mtime_ns",
    )
    for relative, (expected_mode, expected_oid) in archived.items():
        path = worktree / relative
        before = path.lstat()
        if expected_mode == "120000":
            if not stat.S_ISLNK(before.st_mode):
                raise runtime.HarnessError(
                    f"v2 clean {label} type differs from raw bytes: {relative}"
                )
            payload = os.readlink(path).encode(
                "utf-8", "surrogateescape"
            )
        else:
            if not stat.S_ISREG(before.st_mode):
                raise runtime.HarnessError(
                    f"v2 clean {label} type differs from raw bytes: {relative}"
                )
            actual_mode = (
                "100755" if before.st_mode & 0o111 else "100644"
            )
            if actual_mode != expected_mode:
                raise runtime.HarnessError(
                    f"v2 clean {label} mode differs from raw bytes: {relative}"
                )
            payload = path.read_bytes()
        after = path.lstat()
        if any(
            getattr(before, field) != getattr(after, field)
            for field in stable_fields
        ):
            raise runtime.HarnessError(
                f"v2 clean raw bytes changed during {label} proof: {relative}"
            )
        header = f"blob {len(payload)}\0".encode("ascii")
        actual_oid = hashlib.sha1(header + payload).hexdigest()
        if actual_oid != expected_oid:
            raise runtime.HarnessError(
                f"v2 clean {label} transformed raw bytes: {relative}"
            )


def _archive_all_visible_bytes(
    primary: Path,
    worktree: Path,
    contract: Mapping[str, Any],
    task_dir: Path,
) -> Tuple[str, str, str, str]:
    """Archive HEAD plus every tracked/untracked byte via a private index."""

    runtime_dir = task_dir / "runtime"
    runtime_dir.mkdir(parents=True, exist_ok=True)
    import tempfile

    descriptor, raw_index = tempfile.mkstemp(
        prefix="v2-clean-index-", dir=str(runtime_dir)
    )
    os.close(descriptor)
    index_path = Path(raw_index)
    environment = {**os.environ, "GIT_INDEX_FILE": str(index_path)}
    head = runtime.git(worktree, "rev-parse", "HEAD")
    try:
        index_path.unlink(missing_ok=True)
        subprocess.run(
            ["git", "read-tree", "HEAD"],
            cwd=str(worktree),
            env=environment,
            check=True,
            capture_output=True,
        )
        subprocess.run(
            ["git", "add", "-A", "--", "."],
            cwd=str(worktree),
            env=environment,
            check=True,
            capture_output=True,
        )
        tree = subprocess.run(
            ["git", "write-tree"],
            cwd=str(worktree),
            env=environment,
            check=True,
            text=True,
            capture_output=True,
        ).stdout.strip()
        _verify_tree_matches_raw_bytes(
            worktree,
            tree,
            label="archive",
        )
    except subprocess.CalledProcessError as exc:
        detail = (
            exc.stderr.decode("utf-8", "replace")
            if isinstance(exc.stderr, bytes)
            else str(exc.stderr)
        )
        raise runtime.HarnessError(
            "could not archive every Git-visible v2 byte: " + detail
        ) from exc
    finally:
        index_path.unlink(missing_ok=True)
    if tree == runtime.git(worktree, "rev-parse", "HEAD^{tree}"):
        snapshot = head
    else:
        identity = runtime.load_config()["commit_identity"]
        snapshot = runtime.git(
            worktree,
            "-c",
            f"user.name={identity['name']}",
            "-c",
            f"user.email={identity['email']}",
            "commit-tree",
            tree,
            "-p",
            head,
            "-m",
            f"harness v2 archive: {contract['task_id']}",
        )
    archive_ref = _v2_archive_ref(str(contract["task_id"]))
    runtime.git(primary, "update-ref", archive_ref, snapshot)
    if runtime.git(primary, "rev-parse", archive_ref) != snapshot:
        raise runtime.HarnessError("v2 archive ref verification failed")
    if runtime.git(primary, "rev-parse", f"{snapshot}^{{tree}}") != tree:
        raise runtime.HarnessError("v2 archive tree verification failed")
    runtime.atomic_write_json(
        task_dir / "archive.json",
        {
            "schema_version": 2,
            "task_id": contract["task_id"],
            "archive_ref": archive_ref,
            "snapshot_sha": snapshot,
            "original_head_sha": head,
            "tree_sha": tree,
            "created_at": runtime.utc_now(),
        },
    )
    return archive_ref, snapshot, tree, head


DISPOSABLE_IGNORED_PREFIXES = (
    ".angular/cache",
    "dist/meetnotes",
)
DISPOSABLE_IGNORED_FILES = {
    "src-tauri/binaries/meetnotes-aeccap",
    "src-tauri/binaries/meetnotes-audiocap",
    "src-tauri/binaries/meetnotes-brain",
    "src-tauri/binaries/meetnotes-sysaudio",
    "src-tauri/binaries/murmur-brain",
    "test-results/.last-run.json",
}
SNAPSHOT_DISPOSABLE_IGNORED_FILES = {
    "src-tauri/binaries/meetnotes-aeccap",
    "src-tauri/binaries/meetnotes-audiocap",
    "src-tauri/binaries/meetnotes-sysaudio",
    "src-tauri/binaries/murmur-brain",
}
CARGO_CACHE_PREFIXES = ("src-tauri/target", "target")
CARGO_CACHE_TAG_SIGNATURE = b"Signature: 8a477f597d28d172789f06886806bc55"
IGNORED_POLICY_NONE = "none"
IGNORED_POLICY_SNAPSHOT_HELPERS = "snapshot-helpers"
IGNORED_POLICY_FULL = "full"


def _valid_cargo_cache_root(worktree: Path, prefix: str) -> bool:
    tag = worktree / prefix / "CACHEDIR.TAG"
    try:
        metadata = tag.lstat()
    except FileNotFoundError:
        return False
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > 4_096:
        return False
    try:
        return tag.read_bytes().startswith(CARGO_CACHE_TAG_SIGNATURE)
    except OSError:
        return False


def _disposable_ignored_path(worktree: Path, path: str) -> bool:
    if path in DISPOSABLE_IGNORED_FILES:
        return True
    if any(
        path == prefix or path.startswith(prefix + "/")
        for prefix in DISPOSABLE_IGNORED_PREFIXES
    ):
        return True
    for prefix in CARGO_CACHE_PREFIXES:
        if path != prefix and not path.startswith(prefix + "/"):
            continue
        relative = path.removeprefix(prefix).lstrip("/")
        components = relative.split("/") if relative else []
        if any(
            components[index : index + 2] == ["release", "bundle"]
            for index in range(len(components) - 1)
        ):
            # Tauri release bundles can contain signed, notarized, and stapled
            # operator artifacts. They are ignored by Git but not reproducible.
            return False
        return _valid_cargo_cache_root(worktree, prefix)
    return False


def _ignored_path_allowed(
    worktree: Path,
    path: str,
    *,
    policy: str,
) -> bool:
    if policy == IGNORED_POLICY_NONE:
        return False
    if policy == IGNORED_POLICY_SNAPSHOT_HELPERS:
        return path in SNAPSHOT_DISPOSABLE_IGNORED_FILES
    if policy == IGNORED_POLICY_FULL:
        return _disposable_ignored_path(worktree, path)
    raise runtime.HarnessError(
        f"v2 clean ignored-byte policy is malformed: {policy}"
    )


def _ignored_paths(worktree: Path) -> List[str]:
    raw = runtime.git_bytes(
        worktree,
        "ls-files",
        "--others",
        "--ignored",
        "--exclude-standard",
        "-z",
        "--",
    )
    values = sorted(
        item.decode("utf-8", "surrogateescape")
        for item in raw.split(b"\x00")
        if item
    )
    if runtime.managed_node_modules_link(worktree):
        values = [
            path
            for path in values
            if path != "node_modules" and not path.startswith("node_modules/")
        ]
    return values


def _non_disposable_ignored_paths(worktree: Path) -> List[str]:
    return [
        path
        for path in _ignored_paths(worktree)
        if not _disposable_ignored_path(worktree, path)
    ]


def _linked_worktree_admin_path(worktree: Path, owner: Path) -> Path:
    try:
        marker_text = (worktree / ".git").read_text(
            encoding="utf-8"
        ).strip()
    except (OSError, UnicodeError) as exc:
        raise runtime.HarnessError(
            "v2 clean cannot read linked worktree metadata"
        ) from exc
    prefix = "gitdir: "
    if not marker_text.startswith(prefix):
        raise runtime.HarnessError("v2 clean linked worktree metadata is malformed")
    raw_admin = Path(marker_text.removeprefix(prefix))
    git_admin = (
        raw_admin if raw_admin.is_absolute() else (worktree / raw_admin)
    ).resolve()
    common = Path(
        runtime.git(
            owner,
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        )
    ).resolve()
    if git_admin.parent != common / "worktrees":
        raise runtime.HarnessError("v2 clean linked worktree admin path is unsafe")
    return git_admin


def _new_clean_root_record(
    path: Path,
    *,
    expected_tree: str,
    expected_revision: Optional[str],
    git_owner: Optional[Path],
) -> Dict[str, Any]:
    metadata = path.lstat()
    if not stat.S_ISDIR(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode):
        raise runtime.HarnessError(f"v2 clean root is unsafe: {path}")
    quarantine = (
        path.parent
        / f".clean-quarantine-{secrets.token_hex(16)}"
        / path.name
    )
    git_admin = (
        _linked_worktree_admin_path(path, git_owner)
        if git_owner is not None
        else None
    )
    return {
        "path": str(path),
        "device": metadata.st_dev,
        "inode": metadata.st_ino,
        "quarantine_path": str(quarantine),
        "delete_staging_path": str(quarantine.parent / ".delete-staging"),
        "expected_tree": expected_tree,
        "expected_revision": expected_revision,
        "git_admin_path": (
            str(git_admin)
            if git_admin is not None
            else None
        ),
        "git_admin_staging_path": (
            str(
                git_admin.parent
                / (
                    f".clean-{git_admin.name}-"
                    f"{quarantine.parent.name.removeprefix('.clean-quarantine-')}"
                )
            )
            if git_admin is not None
            else None
        ),
    }


def _validate_clean_root_record(
    record: Mapping[str, Any],
    *,
    expected_path: Path,
    git_owner: Optional[Path],
) -> None:
    if Path(str(record.get("path", ""))) != expected_path:
        raise runtime.HarnessError("v2 clean root record path is stale")
    for key in ("device", "inode"):
        if not isinstance(record.get(key), int) or record[key] <= 0:
            raise runtime.HarnessError(
                f"v2 clean root record {key} is malformed"
            )
    if not runtime.SHA1_RE.fullmatch(str(record.get("expected_tree", ""))):
        raise runtime.HarnessError(
            "v2 clean root record expected tree is malformed"
        )
    expected_revision = record.get("expected_revision")
    if expected_revision is not None and not runtime.SHA1_RE.fullmatch(
        str(expected_revision)
    ):
        raise runtime.HarnessError(
            "v2 clean root record expected revision is malformed"
        )
    quarantine = Path(str(record.get("quarantine_path", "")))
    staging = Path(str(record.get("delete_staging_path", "")))
    if (
        quarantine.name != expected_path.name
        or quarantine.parent.parent != expected_path.parent
        or not re.fullmatch(
            r"\.clean-quarantine-[0-9a-f]{32}", quarantine.parent.name
        )
        or staging != quarantine.parent / ".delete-staging"
    ):
        raise runtime.HarnessError(
            "v2 clean root record quarantine path is unsafe"
        )
    raw_admin = record.get("git_admin_path")
    raw_admin_staging = record.get("git_admin_staging_path")
    if git_owner is None:
        if raw_admin is not None or raw_admin_staging is not None:
            raise runtime.HarnessError(
                "v2 standalone clean root unexpectedly has Git admin metadata"
            )
    else:
        admin = Path(str(raw_admin or ""))
        common = Path(
            runtime.git(
                git_owner,
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            )
        ).resolve()
        if admin.parent != common / "worktrees":
            raise runtime.HarnessError(
                "v2 clean root record Git admin path is unsafe"
            )
        admin_staging = Path(str(raw_admin_staging or ""))
        if (
            admin_staging.parent != admin.parent
            or not re.fullmatch(
                rf"\.clean-{re.escape(admin.name)}-[0-9a-f]{{32}}",
                admin_staging.name,
            )
        ):
            raise runtime.HarnessError(
                "v2 clean root record Git admin staging path is unsafe"
            )


def _clean_intent_document(
    contract: Mapping[str, Any],
    *,
    worktree: Path,
    final_status: str,
    previous_status: str,
    archive_ref: str,
    snapshot_sha: str,
    tree_sha: str,
    worktree_revision: str,
    server: Tuple[
        Optional[Path],
        Optional[Path],
        Optional[str],
        Optional[str],
        Optional[str],
    ],
    verification_snapshots: Sequence[Tuple[Path, str, str, str]],
) -> Dict[str, Any]:
    (
        server_worktree,
        server_source,
        server_mode,
        server_revision,
        server_tree,
    ) = server
    primary = Path(str(contract["repo_realpath"])).resolve()
    client_record = _new_clean_root_record(
        worktree,
        expected_tree=tree_sha,
        expected_revision=worktree_revision,
        git_owner=primary,
    )
    server_record: Optional[Dict[str, Any]] = None
    if server_worktree is not None:
        if server_revision is None or server_tree is None:
            raise runtime.HarnessError(
                "v2 clean server preflight lost its pinned revision"
            )
        server_record = _new_clean_root_record(
            server_worktree,
            expected_tree=server_tree,
            expected_revision=server_revision,
            git_owner=(
                server_source if server_mode == "linked-worktree" else None
            ),
        )
    snapshot_records = []
    for path, reference, commit, head_revision in verification_snapshots:
        if path.exists() or path.is_symlink():
            record = {
                **_new_clean_root_record(
                    path,
                    expected_tree=runtime.git(
                        primary, "rev-parse", f"{commit}^{{tree}}"
                    ),
                    expected_revision=head_revision,
                    git_owner=None,
                ),
                "present": True,
            }
        else:
            record = {
                "path": str(path),
                "present": False,
                "expected_revision": head_revision,
            }
        snapshot_records.append(
            {
                **record,
                "snapshot_ref": reference,
                "snapshot_commit": commit,
            }
        )
    document: Dict[str, Any] = {
        "schema_version": 2,
        "task_id": contract["task_id"],
        "contract_sha256": contract["contract_sha256"],
        "final_status": final_status,
        "previous_status": previous_status,
        "archive_ref": archive_ref,
        "snapshot_sha": snapshot_sha,
        "tree_sha": tree_sha,
        "worktree_revision": worktree_revision,
        "worktree_path": contract["worktree_path"],
        "worktree_device": client_record["device"],
        "worktree_inode": client_record["inode"],
        "quarantine_path": client_record["quarantine_path"],
        "delete_staging_path": client_record["delete_staging_path"],
        "git_admin_path": client_record["git_admin_path"],
        "git_admin_staging_path": client_record[
            "git_admin_staging_path"
        ],
        "branch": contract["branch"],
        "server_worktree": (
            str(server_worktree) if server_worktree is not None else None
        ),
        "server_source": str(server_source) if server_source is not None else None,
        "server_mode": server_mode,
        "server_cleanup": server_record,
        "verification_snapshots": snapshot_records,
        "created_at": runtime.utc_now(),
        "intent_sha256": "",
    }
    document["intent_sha256"] = verifier.document_hash(
        document, "intent_sha256"
    )
    return document


def _load_clean_intent(
    contract: Mapping[str, Any], task_dir: Path
) -> Optional[Dict[str, Any]]:
    path = task_dir / "clean-intent.json"
    if not path.is_file():
        return None
    document = runtime.load_json(path)
    if document.get("intent_sha256") != verifier.document_hash(
        document, "intent_sha256"
    ):
        raise runtime.HarnessError("v2 clean intent hash mismatch")
    for key, expected in (
        ("schema_version", 2),
        ("task_id", contract["task_id"]),
        ("contract_sha256", contract["contract_sha256"]),
        ("worktree_path", contract["worktree_path"]),
        ("branch", contract["branch"]),
    ):
        if document.get(key) != expected:
            raise runtime.HarnessError(f"v2 clean intent {key} is stale")
    if document.get("final_status") not in {"CLOSED", "ABANDONED"}:
        raise runtime.HarnessError("v2 clean intent final status is malformed")
    for key in ("worktree_device", "worktree_inode"):
        if not isinstance(document.get(key), int) or document[key] <= 0:
            raise runtime.HarnessError(f"v2 clean intent {key} is malformed")
    if not runtime.SHA1_RE.fullmatch(
        str(document.get("worktree_revision", ""))
    ):
        raise runtime.HarnessError(
            "v2 clean intent worktree revision is malformed"
        )
    worktree = Path(str(contract["worktree_path"]))
    quarantine = Path(str(document.get("quarantine_path", "")))
    staging = Path(str(document.get("delete_staging_path", "")))
    if (
        quarantine.name != worktree.name
        or quarantine.parent.parent != worktree.parent
        or not re.fullmatch(
            r"\.clean-quarantine-[0-9a-f]{32}", quarantine.parent.name
        )
        or staging != quarantine.parent / ".delete-staging"
    ):
        raise runtime.HarnessError("v2 clean intent quarantine path is unsafe")
    git_admin = Path(str(document.get("git_admin_path", "")))
    common = Path(str(contract["git_common_dir"])).resolve()
    if git_admin.parent != common / "worktrees":
        raise runtime.HarnessError("v2 clean intent Git admin path is unsafe")
    git_admin_staging = Path(
        str(document.get("git_admin_staging_path", ""))
    )
    if (
        git_admin_staging.parent != git_admin.parent
        or not re.fullmatch(
            rf"\.clean-{re.escape(git_admin.name)}-[0-9a-f]{{32}}",
            git_admin_staging.name,
        )
    ):
        raise runtime.HarnessError(
            "v2 clean intent Git admin staging path is unsafe"
        )
    server_raw = document.get("server_worktree")
    server_record = document.get("server_cleanup")
    if server_raw is None:
        if server_record is not None:
            raise runtime.HarnessError(
                "v2 clean intent has unexpected server cleanup metadata"
            )
    else:
        if not isinstance(server_record, Mapping):
            raise runtime.HarnessError(
                "v2 clean intent server cleanup metadata is missing"
            )
        server_mode = document.get("server_mode")
        server_owner = (
            Path(str(document.get("server_source")))
            if server_mode == "linked-worktree"
            else None
        )
        _validate_clean_root_record(
            server_record,
            expected_path=Path(str(server_raw)),
            git_owner=server_owner,
        )
        if server_record.get("expected_revision") is None:
            raise runtime.HarnessError(
                "v2 clean intent server revision binding is missing"
            )
    snapshots = document.get("verification_snapshots")
    if not isinstance(snapshots, list):
        raise runtime.HarnessError(
            "v2 clean intent snapshot cleanup metadata is malformed"
        )
    task_root = worktree.parent
    for record in snapshots:
        if not isinstance(record, Mapping):
            raise runtime.HarnessError(
                "v2 clean intent snapshot cleanup record is malformed"
            )
        snapshot = Path(str(record.get("path", "")))
        if snapshot.parent != task_root or not snapshot.name.startswith("verify-"):
            raise runtime.HarnessError(
                "v2 clean intent snapshot path escaped the task root"
            )
        if not runtime.SHA1_RE.fullmatch(
            str(record.get("snapshot_commit", ""))
        ) or not runtime.SHA1_RE.fullmatch(
            str(record.get("expected_revision", ""))
        ):
            raise runtime.HarnessError(
                "v2 clean intent snapshot revision binding is malformed"
            )
        if record.get("present") is True:
            _validate_clean_root_record(
                record, expected_path=snapshot, git_owner=None
            )
        elif record.get("present") is not False:
            raise runtime.HarnessError(
                "v2 clean intent snapshot presence is malformed"
            )
    return document


def _server_cleanup_preflight(
    contract: Mapping[str, Any], task_dir: Path
) -> Tuple[
    Optional[Path],
    Optional[Path],
    Optional[str],
    Optional[str],
    Optional[str],
]:
    runtime_doc = runtime.load_json(task_dir / "runtime.json")
    raw_worktree = runtime_doc.get("server_worktree")
    raw_source = runtime_doc.get("server_source")
    if not raw_worktree:
        return None, None, None, None, None
    server_worktree = Path(str(raw_worktree))
    server_source = Path(str(raw_source))
    expected = Path(str(contract["worktree_path"])).parent / "murmur-server"
    if server_worktree.resolve() != expected.resolve():
        raise runtime.HarnessError("recorded v2 server worktree escapes the task root")
    if not server_worktree.is_dir() or server_worktree.is_symlink():
        raise runtime.HarnessError("recorded v2 server worktree is missing or unsafe")
    _require_sha1_repository(
        server_worktree,
        label="v2 pinned server checkout",
    )
    _require_quarantine_idle(server_worktree)
    if runtime.git_bytes(server_worktree, "status", "--porcelain").strip():
        raise runtime.HarnessError(
            "refusing clean: pinned server worktree is dirty; nothing was removed"
        )
    ignored = _ignored_paths(server_worktree)
    if ignored:
        raise runtime.HarnessError(
            "refusing clean: pinned server checkout has ignored bytes: "
            + ", ".join(ignored[:20])
        )
    expected_revision = str(runtime_doc.get("server_revision") or "")
    if (
        not runtime.SHA1_RE.fullmatch(expected_revision)
        or runtime.git(server_worktree, "rev-parse", "HEAD")
        != expected_revision
    ):
        raise runtime.HarnessError(
            "refusing clean: pinned server revision changed"
        )
    mode = str(runtime_doc.get("server_checkout_mode") or "linked-worktree")
    if mode not in {"linked-worktree", "local-shared-clone"}:
        raise runtime.HarnessError(
            f"recorded v2 server checkout mode is unsupported: {mode}"
        )
    if mode == "local-shared-clone":
        common = Path(
            runtime.git(
                server_worktree,
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            )
        ).resolve()
        if common != (server_worktree / ".git").resolve():
            raise runtime.HarnessError(
                "local v2 server clone unexpectedly shares mutable Git metadata"
            )
    expected_tree = runtime.git(
        server_worktree,
        "rev-parse",
        f"{expected_revision}^{{tree}}",
    )
    if not runtime.SHA1_RE.fullmatch(expected_tree):
        raise runtime.HarnessError(
            "refusing clean: pinned server tree is malformed"
        )
    return (
        server_worktree,
        server_source,
        mode,
        expected_revision,
        expected_tree,
    )


def _verification_snapshots_for_cleanup(
    contract: Mapping[str, Any],
    task_dir: Path,
    *,
    validate_worktrees: bool = True,
) -> List[Tuple[Path, str, str, str]]:
    values: List[Tuple[Path, str, str, str]] = []
    attempts = task_dir / "attempts"
    if not attempts.is_dir():
        return values
    primary = Path(str(contract["repo_realpath"])).resolve()
    for attempt_dir in sorted(attempts.iterdir()):
        manifest_path = attempt_dir / "snapshot.json"
        if not manifest_path.is_file():
            continue
        manifest = runtime.load_json(manifest_path)
        if (
            manifest.get("snapshot_sha256")
            != verifier.document_hash(manifest, "snapshot_sha256")
        ):
            raise runtime.HarnessError(
                f"v2 snapshot manifest is corrupt: {manifest_path}"
            )
        expected_path = _verification_snapshot_path(contract, attempt_dir)
        snapshot = Path(str(manifest.get("path", "")))
        reference = str(manifest.get("snapshot_ref", ""))
        commit_sha = str(manifest.get("snapshot_commit", ""))
        head_revision = str(manifest.get("base_sha", ""))
        if snapshot.resolve() != expected_path:
            raise runtime.HarnessError("v2 cleanup snapshot path is stale")
        if reference != _verification_snapshot_ref(
            str(contract["task_id"]), attempt_dir.name
        ):
            raise runtime.HarnessError("v2 cleanup snapshot ref is stale")
        if not runtime.SHA1_RE.fullmatch(commit_sha):
            raise runtime.HarnessError("v2 cleanup snapshot commit is malformed")
        if not runtime.SHA1_RE.fullmatch(head_revision):
            raise runtime.HarnessError("v2 cleanup snapshot HEAD is malformed")
        current_ref = runtime.git(
            primary, "rev-parse", "--verify", reference, check=False
        )
        if current_ref and current_ref != commit_sha:
            raise runtime.HarnessError("v2 cleanup snapshot ref moved")
        if snapshot.exists() or snapshot.is_symlink():
            if not snapshot.is_dir() or snapshot.is_symlink():
                raise runtime.HarnessError(
                    "v2 cleanup snapshot path is not a safe directory"
                )
            if validate_worktrees:
                _require_sha1_repository(
                    snapshot,
                    label="v2 verification snapshot",
                )
                _require_quarantine_idle(snapshot)
                common = Path(
                    runtime.git(
                        snapshot,
                        "rev-parse",
                        "--path-format=absolute",
                        "--git-common-dir",
                    )
                ).resolve()
                if common != (snapshot / ".git").resolve():
                    raise runtime.HarnessError(
                        "v2 cleanup snapshot shares mutable Git metadata"
                    )
                if (
                    runtime.git(snapshot, "rev-parse", "HEAD")
                    != head_revision
                ):
                    raise runtime.HarnessError(
                        "refusing clean: verification snapshot HEAD changed"
                    )
                ignored = _ignored_paths(snapshot)
                protected_ignored = [
                    path
                    for path in ignored
                    if not _ignored_path_allowed(
                        snapshot,
                        path,
                        policy=IGNORED_POLICY_SNAPSHOT_HELPERS,
                    )
                ]
                if protected_ignored:
                    raise runtime.HarnessError(
                        "refusing clean: verification snapshot has ignored bytes: "
                        + ", ".join(protected_ignored[:20])
                    )
                expected_tree = runtime.git(
                    primary, "rev-parse", f"{commit_sha}^{{tree}}"
                )
                if _visible_tree(snapshot, task_dir / "runtime") != expected_tree:
                    raise runtime.HarnessError(
                        "refusing clean: verification snapshot bytes changed"
                    )
        values.append((snapshot, reference, commit_sha, head_revision))
    return values


def _v2_clean_catchup_merges(
    worktree: Path,
    attested_commit: str,
    current_head: str,
    *,
    default_base: str,
) -> List[str]:
    """Validate only clean base-branch merges after an immutable task commit.

    The task commit and its receipt never move.  A branch that became stale may
    merge the configured base branch, but every first-parent descendant must be
    a two-parent merge whose side parent is contained by that base branch and
    whose tree is exactly Git's automatic merge tree.  This rejects rebases,
    extra branch-authored commits, conflict resolutions, and merge smuggling.
    """

    if current_head == attested_commit:
        return []
    if (
        runtime.run_capture(
            [
                "git",
                "merge-base",
                "--is-ancestor",
                attested_commit,
                current_head,
            ],
            worktree,
            check=False,
        ).returncode
        != 0
    ):
        raise runtime.HarnessError(
            "v2 task commit is not an ancestor of the current branch tip; "
            "rebases and replacement commits require fresh verification"
        )
    base_tip = runtime.git(
        worktree,
        "rev-parse",
        "--verify",
        "--end-of-options",
        f"{default_base}^{{commit}}",
        check=False,
    )
    if not base_tip:
        raise runtime.HarnessError(
            f"cannot validate v2 catch-up merges: fetch {default_base} first"
        )

    merges: List[str] = []
    cursor = current_head
    seen: set[str] = set()
    while cursor != attested_commit:
        if cursor in seen:
            raise runtime.HarnessError("cycle detected in v2 first-parent history")
        seen.add(cursor)
        raw = runtime.git(worktree, "show", "-s", "--format=%P", cursor)
        parents = raw.split()
        if len(parents) != 2:
            raise runtime.HarnessError(
                "v2 branch contains a non-merge commit after its attested task "
                f"commit ({cursor[:12]}); re-verify branch-authored content"
            )
        first_parent, side_parent = parents
        if (
            runtime.run_capture(
                [
                    "git",
                    "merge-base",
                    "--is-ancestor",
                    side_parent,
                    base_tip,
                ],
                worktree,
                check=False,
            ).returncode
            != 0
        ):
            raise runtime.HarnessError(
                f"v2 catch-up merge {cursor[:12]} has a side parent outside "
                f"{default_base}"
            )
        completed = subprocess.run(
            [
                "git",
                "merge-tree",
                "--write-tree",
                "--no-messages",
                first_parent,
                side_parent,
            ],
            cwd=str(worktree),
            text=True,
            capture_output=True,
            check=False,
        )
        if completed.returncode != 0:
            raise runtime.HarnessError(
                f"v2 catch-up merge {cursor[:12]} had conflicts or manual "
                "resolution; verify the resulting diff as a new task"
            )
        output = completed.stdout.strip().splitlines()
        expected_tree = output[0] if output else ""
        actual_tree = runtime.git(worktree, "rev-parse", f"{cursor}^{{tree}}")
        if not expected_tree or expected_tree != actual_tree:
            raise runtime.HarnessError(
                f"v2 catch-up merge {cursor[:12]} contains unverified "
                "resolution content"
            )
        merges.append(cursor)
        cursor = first_parent
    return merges


def verify_v2_committed(
    contract: Mapping[str, Any],
    task_dir: Path,
    *,
    allow_test_adapter: bool = False,
) -> Dict[str, Any]:
    receipt = runtime.load_json(task_dir / "commit.json")
    worktree = Path(str(contract["worktree_path"]))
    if not worktree.is_dir() or worktree.is_symlink():
        raise runtime.HarnessError("v2 committed worktree is missing or unsafe")
    if (
        Path(runtime.git(worktree, "rev-parse", "--show-toplevel")).resolve()
        != worktree.resolve()
    ):
        raise runtime.HarnessError("v2 committed worktree path is not its Git root")
    if runtime.git(worktree, "branch", "--show-current") != contract["branch"]:
        raise runtime.HarnessError("v2 committed task branch changed")
    if runtime.git_bytes(worktree, "status", "--porcelain").strip():
        raise runtime.HarnessError("v2 committed worktree/index is not clean")

    attested_commit = str(receipt.get("commit_sha", ""))
    if not runtime.SHA1_RE.fullmatch(attested_commit):
        raise runtime.HarnessError("v2 committed receipt commit is malformed")
    runtime.validate_schema(
        receipt,
        verifier.attested_schema(worktree, attested_commit, "v2-commit"),
        label="v2 commit receipt",
    )
    attested_config = verifier.attested_json_object(
        worktree,
        attested_commit,
        ".agents/harness/config.json",
        "harness config",
    )
    default_base = attested_config.get("default_base", "origin/murmur")
    if not isinstance(default_base, str) or not default_base:
        raise runtime.HarnessError("v2 attested default_base is malformed")
    current_head = runtime.git(worktree, "rev-parse", "HEAD")
    _v2_clean_catchup_merges(
        worktree,
        attested_commit,
        current_head,
        default_base=default_base,
    )
    parents = runtime.git(
        worktree, "show", "-s", "--format=%P", attested_commit
    ).split()
    if len(parents) != 1:
        raise runtime.HarnessError("v2 attested task commit must have exactly one parent")
    parent = parents[0]
    tree = runtime.git(worktree, "rev-parse", f"{attested_commit}^{{tree}}")
    diff = runtime.git_bytes(
        worktree,
        "diff",
        "--binary",
        "--full-index",
        "--no-ext-diff",
        "--no-renames",
        parent,
        attested_commit,
        "--",
    )
    for key, value in (
        ("task_id", contract["task_id"]),
        ("contract_sha256", contract["contract_sha256"]),
        ("commit_sha", attested_commit),
        ("parent_sha", parent),
        ("tree_sha", tree),
        ("diff_sha256", runtime.sha256_bytes(diff)),
    ):
        if receipt.get(key) != value:
            raise runtime.HarnessError(f"v2 committed receipt {key} is stale")
    state = load_v2_state(task_dir)
    for key, value in (
        ("commit_sha", attested_commit),
        ("parent_sha", parent),
        ("tree_sha", tree),
        ("evidence_sha256", receipt["evidence_sha256"]),
    ):
        if state.get(key) != value:
            raise runtime.HarnessError(
                f"v2 committed state {key} differs from its receipt"
            )
    evidence = verifier.verify_v2_evidence(
        contract,
        task_dir,
        allow_test_adapter=allow_test_adapter,
        attested_commit_sha=attested_commit,
    )
    if receipt.get("evidence_sha256") != evidence.get("evidence_sha256"):
        raise runtime.HarnessError("v2 commit no longer binds its exact evidence")
    if receipt.get("author") != {
        "name": "QueaT",
        "email": "kgm004a@gmail.com",
    } or receipt.get("committer") != {
        "name": "QueaT",
        "email": "kgm004a@gmail.com",
    }:
        raise runtime.HarnessError("v2 committed receipt identity is invalid")
    actual_author = {
        "name": runtime.git(worktree, "show", "-s", "--format=%an", attested_commit),
        "email": runtime.git(worktree, "show", "-s", "--format=%ae", attested_commit),
    }
    actual_committer = {
        "name": runtime.git(worktree, "show", "-s", "--format=%cn", attested_commit),
        "email": runtime.git(worktree, "show", "-s", "--format=%ce", attested_commit),
    }
    actual_message = runtime.git(
        worktree, "show", "-s", "--format=%B", attested_commit
    ).rstrip("\n")
    _strict_v2_receipt_trailers(actual_message, evidence)
    if receipt.get("author") != actual_author or receipt.get("committer") != actual_committer:
        raise runtime.HarnessError("v2 committed receipt identity is stale")
    if receipt.get("message") != actual_message:
        raise runtime.HarnessError("v2 committed receipt message is stale")
    if receipt.get("authored_at") != runtime.git(
        worktree, "show", "-s", "--format=%aI", attested_commit
    ):
        raise runtime.HarnessError("v2 committed receipt authored_at is stale")
    if receipt.get("committed_at") != runtime.git(
        worktree, "show", "-s", "--format=%cI", attested_commit
    ):
        raise runtime.HarnessError("v2 committed receipt committed_at is stale")
    return receipt


def _clean_entry_snapshot_at(
    parent_fd: int,
    name: str,
    relative: str,
    root_device: int,
) -> Dict[str, Any]:
    before = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
    if before.st_dev != root_device:
        raise runtime.HarnessError(
            f"v2 clean entry crosses a filesystem boundary: {relative}"
        )
    common: Dict[str, Any] = {
        "path": relative,
        "device": before.st_dev,
        "inode": before.st_ino,
        "mode": stat.S_IMODE(before.st_mode),
        "nlink": before.st_nlink,
        "size": before.st_size,
        "mtime_ns": before.st_mtime_ns,
    }
    if stat.S_ISDIR(before.st_mode):
        return {**common, "kind": "directory", "sha256": ""}
    if stat.S_ISREG(before.st_mode):
        flags = os.O_RDONLY
        for optional in ("O_CLOEXEC", "O_NOFOLLOW"):
            flags |= int(getattr(os, optional, 0))
        descriptor = os.open(name, flags, dir_fd=parent_fd)
        try:
            opened = os.fstat(descriptor)
            if (
                not stat.S_ISREG(opened.st_mode)
                or (opened.st_dev, opened.st_ino)
                != (before.st_dev, before.st_ino)
            ):
                raise runtime.HarnessError(
                    f"v2 clean entry changed while opening: {relative}"
                )
            digest = hashlib.sha256()
            while True:
                chunk = os.read(descriptor, 1024 * 1024)
                if not chunk:
                    break
                digest.update(chunk)
            after = os.fstat(descriptor)
        finally:
            os.close(descriptor)
        stable_fields = ("st_dev", "st_ino", "st_mode", "st_nlink", "st_size", "st_mtime_ns")
        if any(getattr(before, key) != getattr(after, key) for key in stable_fields):
            raise runtime.HarnessError(
                f"v2 clean entry changed while hashing: {relative}"
            )
        return {**common, "kind": "regular", "sha256": digest.hexdigest()}
    if stat.S_ISLNK(before.st_mode):
        target = os.readlink(name, dir_fd=parent_fd)
        after = os.stat(name, dir_fd=parent_fd, follow_symlinks=False)
        stable_fields = ("st_dev", "st_ino", "st_mode", "st_nlink", "st_size", "st_mtime_ns")
        if any(getattr(before, key) != getattr(after, key) for key in stable_fields):
            raise runtime.HarnessError(
                f"v2 clean symlink changed while reading: {relative}"
            )
        return {
            **common,
            "kind": "symlink",
            "sha256": runtime.sha256_bytes(
                target.encode("utf-8", "surrogateescape")
            ),
        }
    raise runtime.HarnessError(
        f"v2 clean refuses a special filesystem entry: {relative}"
    )


def _clean_tree_snapshot(
    root: Path,
    *,
    max_regular_bytes: Optional[int] = None,
) -> List[Dict[str, Any]]:
    flags = os.O_RDONLY | int(getattr(os, "O_DIRECTORY", 0))
    flags |= int(getattr(os, "O_CLOEXEC", 0))
    flags |= int(getattr(os, "O_NOFOLLOW", 0))
    root_fd = os.open(root, flags)
    root_metadata = os.fstat(root_fd)
    entries: List[Dict[str, Any]] = []
    regular_bytes = 0

    def visit(directory_fd: int, prefix: str) -> None:
        nonlocal regular_bytes
        with os.scandir(directory_fd) as iterator:
            names = sorted(entry.name for entry in iterator)
        for name in names:
            relative = f"{prefix}/{name}" if prefix else name
            before = os.stat(
                name,
                dir_fd=directory_fd,
                follow_symlinks=False,
            )
            if stat.S_ISREG(before.st_mode):
                regular_bytes += before.st_size
                if (
                    max_regular_bytes is not None
                    and regular_bytes > max_regular_bytes
                ):
                    raise runtime.HarnessError(
                        "v2 clean Git-control metadata exceeds the "
                        "recoverable archive limit"
                    )
            snapshot = _clean_entry_snapshot_at(
                directory_fd, name, relative, root_metadata.st_dev
            )
            entries.append(snapshot)
            if snapshot["kind"] == "directory":
                child_fd = os.open(name, flags, dir_fd=directory_fd)
                try:
                    opened = os.fstat(child_fd)
                    if (opened.st_dev, opened.st_ino) != (
                        snapshot["device"],
                        snapshot["inode"],
                    ):
                        raise runtime.HarnessError(
                            f"v2 clean directory changed while opening: {relative}"
                        )
                    visit(child_fd, relative)
                finally:
                    os.close(child_fd)

    try:
        visit(root_fd, "")
    finally:
        os.close(root_fd)
    return sorted(entries, key=lambda entry: str(entry["path"]))


def _clean_visible_paths(worktree: Path) -> set[str]:
    raw = runtime.git_bytes(
        worktree,
        "ls-files",
        "--cached",
        "--others",
        "--exclude-standard",
        "-z",
        "--",
    )
    return {
        item.decode("utf-8", "surrogateescape")
        for item in raw.split(b"\0")
        if item
    }


def _clean_entry_matches(
    expected: Mapping[str, Any], current: Mapping[str, Any]
) -> bool:
    common = ("kind", "device", "inode", "mode")
    if any(expected.get(key) != current.get(key) for key in common):
        return False
    if expected.get("kind") == "directory":
        return True
    # Deleting one name of a frozen hardlink group legitimately decrements the
    # remaining names' link counts. Identity, bytes, mode, size, and mtime stay
    # bound; nlink is evidence in the manifest but not a per-name delete guard.
    exact = ("size", "mtime_ns", "sha256")
    return all(expected.get(key) == current.get(key) for key in exact)


GIT_CONTROL_ARCHIVE_LIMIT = 512 * 1024 * 1024


def _immutable_artifact_exists(path: Path, *, label: str) -> bool:
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        return False
    if (
        not stat.S_ISREG(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or metadata.st_nlink != 1
    ):
        raise runtime.HarnessError(
            f"v2 clean {label} is not a private immutable file"
        )
    return True


def _ensure_private_durable_directory(path: Path, *, label: str) -> None:
    try:
        metadata = path.lstat()
    except FileNotFoundError:
        os.mkdir(path, 0o700)
        parent_fd = os.open(
            path.parent,
            os.O_RDONLY | int(getattr(os, "O_DIRECTORY", 0)),
        )
        try:
            os.fsync(parent_fd)
        finally:
            os.close(parent_fd)
        metadata = path.lstat()
    if not stat.S_ISDIR(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode):
        raise runtime.HarnessError(f"v2 clean {label} is unsafe")
    if stat.S_IMODE(metadata.st_mode) & 0o077:
        raise runtime.HarnessError(f"v2 clean {label} is not private")


def _git_control_root(
    repo_root: Path,
    record: Mapping[str, Any],
    git_owner: Optional[Path],
) -> Path:
    if git_owner is not None:
        candidate = Path(str(record["git_admin_path"]))
    else:
        candidate = repo_root / ".git"
    if not candidate.exists() and not candidate.is_symlink():
        raise runtime.HarnessError(
            "v2 clean Git-control root is missing"
        )
    if not candidate.is_dir() or candidate.is_symlink():
        raise runtime.HarnessError(
            "v2 clean Git-control root is missing or unsafe"
        )
    return candidate


def _clean_directory_projection(
    root: Path,
    *,
    max_regular_bytes: Optional[int] = None,
) -> Tuple[Dict[str, Any], List[Dict[str, Any]]]:
    before = root.lstat()
    if not stat.S_ISDIR(before.st_mode) or stat.S_ISLNK(before.st_mode):
        raise runtime.HarnessError(
            "v2 clean Git-control directory is unsafe"
        )
    entries = _control_entry_projection(
        _clean_tree_snapshot(
            root,
            max_regular_bytes=max_regular_bytes,
        )
    )
    after = root.lstat()
    stable = (
        "st_dev",
        "st_ino",
        "st_mode",
        "st_nlink",
        "st_size",
        "st_mtime_ns",
    )
    if any(getattr(before, key) != getattr(after, key) for key in stable):
        raise runtime.HarnessError(
            "v2 clean Git-control root changed while archiving"
        )
    projection = {
        "device": after.st_dev,
        "inode": after.st_ino,
        "mode": stat.S_IMODE(after.st_mode),
        "nlink": after.st_nlink,
        "size": after.st_size,
        "mtime_ns": after.st_mtime_ns,
    }
    return projection, entries


def _clean_path_projection(path: Path, relative: str) -> Dict[str, Any]:
    flags = os.O_RDONLY | int(getattr(os, "O_DIRECTORY", 0))
    flags |= int(getattr(os, "O_CLOEXEC", 0))
    flags |= int(getattr(os, "O_NOFOLLOW", 0))
    parent_fd = os.open(path.parent, flags)
    try:
        parent = os.fstat(parent_fd)
        return _clean_entry_snapshot_at(
            parent_fd,
            path.name,
            relative,
            parent.st_dev,
        )
    finally:
        os.close(parent_fd)


def _control_entry_projection(
    entries: Sequence[Mapping[str, Any]],
) -> List[Dict[str, Any]]:
    keys = (
        "path",
        "kind",
        "device",
        "inode",
        "mode",
        "nlink",
        "size",
        "mtime_ns",
        "sha256",
    )
    return [
        {key: entry[key] for key in keys}
        for entry in entries
    ]


def _read_git_control_payload(
    source_root: Path,
    entry: Mapping[str, Any],
) -> bytes:
    path = source_root / str(entry["path"])
    before = path.lstat()
    if entry["kind"] == "regular":
        if not stat.S_ISREG(before.st_mode):
            raise runtime.HarnessError(
                "v2 clean Git-control payload type changed"
            )
        payload = path.read_bytes()
    elif entry["kind"] == "symlink":
        if not stat.S_ISLNK(before.st_mode):
            raise runtime.HarnessError(
                "v2 clean Git-control payload type changed"
            )
        payload = os.readlink(path).encode(
            "utf-8", "surrogateescape"
        )
    else:
        raise runtime.HarnessError(
            "v2 clean Git-control payload kind is malformed"
        )
    after = path.lstat()
    current = {
        "kind": entry["kind"],
        "device": after.st_dev,
        "inode": after.st_ino,
        "mode": stat.S_IMODE(after.st_mode),
        "size": after.st_size,
        "mtime_ns": after.st_mtime_ns,
        "sha256": runtime.sha256_bytes(payload),
    }
    if not _clean_entry_matches(entry, current):
        raise runtime.HarnessError(
            "v2 clean Git-control payload changed during archival: "
            + str(entry["path"])
        )
    return payload


def _require_sha1_repository(repo: Path, *, label: str) -> None:
    object_format = runtime.git(
        repo,
        "rev-parse",
        "--show-object-format",
        check=False,
    )
    if object_format != "sha1":
        raise runtime.HarnessError(
            f"{label} must use Git SHA-1 object format; found "
            f"{object_format or 'unknown'}"
        )


def _existing_git_object(repo: Path, object_id: str) -> bool:
    completed = subprocess.run(
        [
            "git",
            "--no-replace-objects",
            "cat-file",
            "-e",
            f"{object_id}^{{object}}",
        ],
        cwd=str(repo),
        env={**os.environ, "GIT_NO_REPLACE_OBJECTS": "1"},
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        check=False,
    )
    return completed.returncode == 0


def _control_object_seeds(
    *,
    repo_root: Path,
    control_root: Path,
    git_owner: Optional[Path],
    entries: Sequence[Mapping[str, Any]],
) -> Tuple[str, List[str]]:
    _require_sha1_repository(
        repo_root,
        label="v2 clean Git-control repository",
    )
    if git_owner is None:
        completed = subprocess.run(
            [
                "git",
                "--no-replace-objects",
                "cat-file",
                "--batch-all-objects",
                "--batch-check=%(objectname)",
            ],
            cwd=str(repo_root),
            env={**os.environ, "GIT_NO_REPLACE_OBJECTS": "1"},
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if completed.returncode != 0:
            raise runtime.HarnessError(
                "v2 clean cannot enumerate standalone Git objects: "
                + completed.stderr.strip()
            )
        seeds = sorted(
            {
                line
                for line in completed.stdout.splitlines()
                if runtime.SHA1_RE.fullmatch(line)
            }
        )
        malformed = [
            line
            for line in completed.stdout.splitlines()
            if line and not runtime.SHA1_RE.fullmatch(line)
        ]
        if malformed:
            raise runtime.HarnessError(
                "v2 clean standalone Git object inventory is malformed"
            )
        return "all-objects", seeds

    candidates: set[str] = set()
    head = runtime.git(repo_root, "rev-parse", "HEAD", check=False)
    if runtime.SHA1_RE.fullmatch(head):
        candidates.add(head)
    for argv in (
        ("ls-files", "--stage", "-z"),
        ("ls-files", "--resolve-undo", "-z"),
    ):
        raw = runtime.git_bytes(repo_root, *argv)
        candidates.update(
            match.group(0).decode("ascii")
            for match in re.finditer(rb"[0-9a-f]{40}", raw)
        )
    for entry in entries:
        if entry["kind"] != "regular":
            continue
        payload = _read_git_control_payload(control_root, entry)
        candidates.update(
            match.group(0).decode("ascii")
            for match in re.finditer(rb"[0-9a-f]{40}", payload)
        )
    seeds = sorted(
        object_id
        for object_id in candidates
        if object_id != "0" * 40
        and _existing_git_object(repo_root, object_id)
    )
    if not seeds:
        raise runtime.HarnessError(
            "v2 clean linked Git-control object closure is empty"
        )
    return "reachable-closure", seeds


def _atomic_install_file(source: Path, destination: Path) -> None:
    if _immutable_artifact_exists(
        destination,
        label="Git-control object pack",
    ):
        return
    try:
        os.link(source, destination)
    except FileExistsError as exc:
        raise runtime.HarnessError(
            f"refusing to overwrite prior execution artifact: {destination}"
        ) from exc
    parent_fd = os.open(
        destination.parent,
        os.O_RDONLY | int(getattr(os, "O_DIRECTORY", 0)),
    )
    try:
        os.fsync(parent_fd)
    finally:
        os.close(parent_fd)


def _ensure_control_object_pack(
    *,
    repo_root: Optional[Path],
    archive_root: Path,
    mode: str,
    seeds: Optional[Sequence[str]],
    expected: Optional[Mapping[str, Any]],
) -> Dict[str, Any]:
    pack_path = archive_root / "objects.pack"
    index_path = archive_root / "objects.idx"
    if expected is None:
        if repo_root is None or seeds is None:
            raise runtime.HarnessError(
                "v2 clean Git-control object pack cannot be recovered"
            )
        runtime_dir = archive_root.parent.parent / "runtime"
        runtime_dir.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(
            prefix="clean-object-pack-",
            dir=str(runtime_dir),
        ) as raw_temporary:
            temporary = Path(raw_temporary)
            temporary_pack = temporary / "objects.pack"
            temporary_index = temporary / "objects.idx"
            with temporary_pack.open("wb") as output:
                completed = subprocess.run(
                    [
                        "git",
                        "--no-replace-objects",
                        "pack-objects",
                        "--stdout",
                        *(["--revs"] if mode == "reachable-closure" else []),
                    ],
                    cwd=str(repo_root),
                    env={**os.environ, "GIT_NO_REPLACE_OBJECTS": "1"},
                    input=("\n".join(seeds) + "\n").encode("ascii"),
                    stdout=output,
                    stderr=subprocess.PIPE,
                    check=False,
                )
            if completed.returncode != 0:
                raise runtime.HarnessError(
                    "v2 clean could not create a self-contained "
                    "Git object pack: "
                    + completed.stderr.decode("utf-8", "replace").strip()
                )
            if (
                temporary_pack.stat().st_size
                > GIT_CONTROL_ARCHIVE_LIMIT
            ):
                raise runtime.HarnessError(
                    "v2 clean Git object pack exceeds the recoverable "
                    "archive limit"
                )
            indexed = subprocess.run(
                [
                    "git",
                    "index-pack",
                    "-o",
                    str(temporary_index),
                    str(temporary_pack),
                ],
                cwd=str(repo_root),
                env={**os.environ, "GIT_NO_REPLACE_OBJECTS": "1"},
                text=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                check=False,
            )
            if indexed.returncode != 0:
                raise runtime.HarnessError(
                    "v2 clean Git object pack failed index verification: "
                    + indexed.stderr.strip()
                )
            _atomic_install_file(temporary_pack, pack_path)
            _atomic_install_file(temporary_index, index_path)
        expected = {
            "mode": mode,
            "seed_count": len(seeds),
            "seeds_sha256": runtime.sha256_bytes(
                ("\n".join(seeds) + "\n").encode("ascii")
            ),
            "pack_sha256": runtime.sha256_file(pack_path),
            "index_sha256": runtime.sha256_file(index_path),
        }
    if (
        expected.get("mode") != mode
        or not isinstance(expected.get("seed_count"), int)
        or not runtime.SHA256_RE.fullmatch(
            str(expected.get("seeds_sha256", ""))
        )
    ):
        raise runtime.HarnessError(
            "v2 clean Git-control object pack metadata is malformed"
        )
    if seeds is not None and (
        expected["seed_count"] != len(seeds)
        or expected["seeds_sha256"]
        != runtime.sha256_bytes(
            ("\n".join(seeds) + "\n").encode("ascii")
        )
    ):
        raise runtime.HarnessError(
            "refusing clean: Git-control object inventory changed"
        )
    for path, key in (
        (pack_path, "pack_sha256"),
        (index_path, "index_sha256"),
    ):
        if not _immutable_artifact_exists(
            path, label="Git-control object pack"
        ) or runtime.sha256_file(path) != expected.get(key):
            raise runtime.HarnessError(
                "v2 clean Git-control object pack is missing or stale"
            )
    verified = subprocess.run(
        ["git", "verify-pack", "-v", str(index_path)],
        cwd=str(archive_root),
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if verified.returncode != 0:
        raise runtime.HarnessError(
            "v2 clean archived Git object pack is corrupt: "
            + verified.stderr.strip()
        )
    packed_objects = {
        line.split()[0]
        for line in verified.stdout.splitlines()
        if line
        and runtime.SHA1_RE.fullmatch(line.split()[0])
    }
    if seeds is not None and not set(seeds).issubset(packed_objects):
        raise runtime.HarnessError(
            "v2 clean archived Git object pack omits bound objects"
        )
    return dict(expected)


def _archive_git_control(
    *,
    task_dir: Path,
    intent: Mapping[str, Any],
    archive_role: str,
    repo_root: Path,
    record: Mapping[str, Any],
    git_owner: Optional[Path],
    live: bool,
) -> Dict[str, Any]:
    manifest_path = (
        task_dir / f"clean-git-control-{archive_role}.json"
    )
    complete_path = (
        task_dir
        / f"clean-git-control-{archive_role}-complete.json"
    )
    expected_control_root = (
        Path(str(record["git_admin_path"]))
        if git_owner is not None
        else repo_root / ".git"
    )
    expected_marker = (
        repo_root / ".git" if git_owner is not None else None
    )
    current_root: Optional[Dict[str, Any]] = None
    current_entries: Optional[List[Dict[str, Any]]] = None
    current_marker: Optional[Dict[str, Any]] = None
    object_mode: Optional[str] = None
    object_seeds: Optional[List[str]] = None
    current_object_inventory: Optional[Dict[str, Any]] = None
    control_root: Optional[Path] = None
    if live:
        control_root = _git_control_root(
            repo_root, record, git_owner
        )
        current_root, current_entries = (
            _clean_directory_projection(
                control_root,
                max_regular_bytes=GIT_CONTROL_ARCHIVE_LIMIT,
            )
        )
        if expected_marker is not None:
            current_marker = _clean_path_projection(
                expected_marker, ".git"
            )
            if current_marker["kind"] != "regular":
                raise runtime.HarnessError(
                    "v2 clean linked Git marker is not a regular file"
                )
            if (
                int(current_marker["size"])
                > GIT_CONTROL_ARCHIVE_LIMIT
            ):
                raise runtime.HarnessError(
                    "v2 clean linked Git marker exceeds the "
                    "recoverable archive limit"
                )
        object_mode, object_seeds = _control_object_seeds(
            repo_root=repo_root,
            control_root=control_root,
            git_owner=git_owner,
            entries=current_entries,
        )
        current_object_inventory = {
            "format": "sha1",
            "mode": object_mode,
            "seed_count": len(object_seeds),
            "seeds_sha256": runtime.sha256_bytes(
                ("\n".join(object_seeds) + "\n").encode("ascii")
            ),
            "seeds": object_seeds,
        }

    if _immutable_artifact_exists(
        manifest_path, label=f"{archive_role} Git-control manifest"
    ):
        document = runtime.load_json(manifest_path)
        if document.get("manifest_sha256") != verifier.document_hash(
            document, "manifest_sha256"
        ):
            raise runtime.HarnessError(
                f"v2 clean {archive_role} Git-control manifest "
                "hash mismatch"
            )
        for key, expected in (
            ("schema_version", 2),
            ("archive_role", archive_role),
            ("intent_sha256", intent["intent_sha256"]),
            ("repo_root", str(repo_root)),
            ("control_root", str(expected_control_root)),
            (
                "marker_path",
                (
                    str(expected_marker)
                    if expected_marker is not None
                    else None
                ),
            ),
        ):
            if document.get(key) != expected:
                raise runtime.HarnessError(
                    f"v2 clean {archive_role} Git-control manifest "
                    "is stale"
                )
        if (
            live
            and (
                document.get("root") != current_root
                or document.get("entries") != current_entries
                or document.get("marker") != current_marker
                or document.get("object_inventory")
                != current_object_inventory
            )
        ):
            raise runtime.HarnessError(
                f"refusing clean: {archive_role} Git-control "
                "metadata changed"
            )
    else:
        if not live or current_root is None or current_entries is None:
            raise runtime.HarnessError(
                f"v2 clean {archive_role} Git-control root disappeared "
                "before archival"
            )
        document = {
            "schema_version": 2,
            "archive_role": archive_role,
            "intent_sha256": intent["intent_sha256"],
            "repo_root": str(repo_root),
            "control_root": str(expected_control_root),
            "marker_path": (
                str(expected_marker)
                if expected_marker is not None
                else None
            ),
            "root": current_root,
            "entries": current_entries,
            "marker": current_marker,
            "object_inventory": current_object_inventory,
            "manifest_sha256": "",
        }
        document["manifest_sha256"] = verifier.document_hash(
            document, "manifest_sha256"
        )
        runtime.atomic_create_json(manifest_path, document)

    archive_base = task_dir / "clean-git-control"
    _ensure_private_durable_directory(
        archive_base,
        label="Git-control archive base",
    )
    archive_root = archive_base / archive_role
    _ensure_private_durable_directory(
        archive_root,
        label=f"{archive_role} Git-control archive path",
    )
    archive_root_before = archive_root.lstat()

    payload_records = []
    payload_index = 0
    sources: List[Tuple[str, Mapping[str, Any], Optional[Path]]] = [
        ("control", entry, control_root)
        for entry in document["entries"]
    ]
    marker_entry = document.get("marker")
    if isinstance(marker_entry, Mapping):
        sources.append(
            (
                "marker",
                marker_entry,
                expected_marker.parent
                if live and expected_marker is not None
                else None,
            )
        )
    for source, entry, source_root in sources:
        if entry["kind"] == "directory":
            continue
        destination = archive_root / f"payload-{payload_index:08d}.bin"
        payload_index += 1
        if _immutable_artifact_exists(
            destination,
            label=f"{archive_role} Git-control payload",
        ):
            payload_sha256 = runtime.sha256_file(destination)
            if payload_sha256 != entry["sha256"]:
                raise runtime.HarnessError(
                    f"v2 clean {archive_role} Git-control payload "
                    "hash mismatch"
                )
        else:
            if source_root is None:
                raise runtime.HarnessError(
                    f"v2 clean {archive_role} Git-control payload "
                    "is missing"
                )
            payload = _read_git_control_payload(
                source_root,
                entry,
            )
            runtime.atomic_create_bytes(destination, payload)
            payload_sha256 = runtime.sha256_bytes(payload)
        payload_records.append(
            {
                "source": source,
                "path": entry["path"],
                "payload": str(destination.relative_to(task_dir)),
                "sha256": payload_sha256,
            }
        )

    inventory = document.get("object_inventory")
    if (
        not isinstance(inventory, Mapping)
        or inventory.get("format") != "sha1"
        or inventory.get("mode")
        not in {"all-objects", "reachable-closure"}
        or not isinstance(inventory.get("seeds"), list)
        or inventory.get("seed_count")
        != len(inventory.get("seeds", []))
        or any(
            not isinstance(object_id, str)
            or not runtime.SHA1_RE.fullmatch(object_id)
            for object_id in inventory.get("seeds", [])
        )
    ):
        raise runtime.HarnessError(
            f"v2 clean {archive_role} Git object inventory is malformed"
        )
    existing_completion: Optional[Dict[str, Any]] = None
    if _immutable_artifact_exists(
        complete_path,
        label=f"{archive_role} Git-control completion",
    ):
        loaded_completion = runtime.load_json(complete_path)
        if loaded_completion.get(
            "complete_sha256"
        ) != verifier.document_hash(
            loaded_completion, "complete_sha256"
        ):
            raise runtime.HarnessError(
                f"v2 clean {archive_role} Git-control completion "
                "hash mismatch"
            )
        existing_completion = loaded_completion
    object_pack = _ensure_control_object_pack(
        repo_root=repo_root if live else None,
        archive_root=archive_root,
        mode=str(inventory["mode"]),
        seeds=list(inventory["seeds"]),
        expected=(
            existing_completion.get("object_pack")
            if existing_completion is not None
            else None
        ),
    )
    for name, key in (
        ("objects.pack", "pack_sha256"),
        ("objects.idx", "index_sha256"),
    ):
        payload_records.append(
            {
                "source": "object-pack",
                "path": name,
                "payload": str(
                    (archive_root / name).relative_to(task_dir)
                ),
                "sha256": object_pack[key],
            }
        )
    total_archive_bytes = sum(
        int(entry["size"])
        for entry in document["entries"]
        if entry["kind"] == "regular"
    ) + sum(
        (archive_root / name).stat().st_size
        for name in ("objects.pack", "objects.idx")
    )
    if total_archive_bytes > GIT_CONTROL_ARCHIVE_LIMIT:
        raise runtime.HarnessError(
            f"v2 clean {archive_role} Git-control archive exceeds "
            "the recoverable archive limit"
        )
    if live:
        verified_root, verified_entries = (
            _clean_directory_projection(
                expected_control_root,
                max_regular_bytes=GIT_CONTROL_ARCHIVE_LIMIT,
            )
        )
        verified_marker = (
            _clean_path_projection(expected_marker, ".git")
            if expected_marker is not None
            else None
        )
        if (
            document.get("root") != verified_root
            or document.get("entries") != verified_entries
            or document.get("marker") != verified_marker
        ):
            raise runtime.HarnessError(
                f"refusing clean: {archive_role} Git-control source "
                "changed during payload archival"
            )
        verified_mode, verified_seeds = _control_object_seeds(
            repo_root=repo_root,
            control_root=expected_control_root,
            git_owner=git_owner,
            entries=verified_entries,
        )
        if (
            verified_mode != inventory["mode"]
            or len(verified_seeds) != inventory["seed_count"]
            or runtime.sha256_bytes(
                ("\n".join(verified_seeds) + "\n").encode("ascii")
            )
            != inventory["seeds_sha256"]
        ):
            raise runtime.HarnessError(
                f"refusing clean: {archive_role} Git object inventory "
                "changed during archival"
            )
    for record in payload_records:
        payload_path = task_dir / str(record["payload"])
        os.chmod(payload_path, 0o400, follow_symlinks=False)
    os.chmod(archive_root, 0o500)
    archive_fd = os.open(
        archive_root,
        os.O_RDONLY | int(getattr(os, "O_DIRECTORY", 0)),
    )
    try:
        os.fsync(archive_fd)
    finally:
        os.close(archive_fd)
    archive_root_after = archive_root.lstat()
    if (archive_root_after.st_dev, archive_root_after.st_ino) != (
        archive_root_before.st_dev,
        archive_root_before.st_ino,
    ):
        raise runtime.HarnessError(
            f"v2 clean {archive_role} Git-control archive root "
            "was replaced"
        )
    archive_identity = {
        "path": str(archive_root),
        "device": archive_root_after.st_dev,
        "inode": archive_root_after.st_ino,
        "mode": stat.S_IMODE(archive_root_after.st_mode),
    }
    completion = {
        "schema_version": 2,
        "archive_role": archive_role,
        "manifest_sha256": document["manifest_sha256"],
        "archive_root": archive_identity,
        "object_pack": object_pack,
        "payloads": payload_records,
        "complete_sha256": "",
    }
    completion["complete_sha256"] = verifier.document_hash(
        completion, "complete_sha256"
    )
    if existing_completion is not None:
        if existing_completion != completion:
            raise runtime.HarnessError(
                f"v2 clean {archive_role} Git-control completion "
                "is stale"
            )
    else:
        runtime.atomic_create_json(complete_path, completion)
    return document


def _bind_clean_delete_ready(
    *,
    task_dir: Path,
    intent: Mapping[str, Any],
    role: str,
    delete_manifest: Mapping[str, Any],
    pre_control: Mapping[str, Any],
    final_control: Mapping[str, Any],
) -> Dict[str, Any]:
    path = task_dir / f"clean-delete-ready-{role}.json"
    completion_hashes: Dict[str, str] = {}
    for phase, manifest in (
        ("pre", pre_control),
        ("final", final_control),
    ):
        archive_role = f"{role}-{phase}"
        completion_path = (
            task_dir
            / f"clean-git-control-{archive_role}-complete.json"
        )
        if not _immutable_artifact_exists(
            completion_path,
            label=f"{archive_role} Git-control completion",
        ):
            raise runtime.HarnessError(
                f"v2 clean {archive_role} Git-control completion "
                "is missing"
            )
        completion = runtime.load_json(completion_path)
        if (
            completion.get("complete_sha256")
            != verifier.document_hash(
                completion, "complete_sha256"
            )
            or completion.get("manifest_sha256")
            != manifest["manifest_sha256"]
        ):
            raise runtime.HarnessError(
                f"v2 clean {archive_role} Git-control completion "
                "is stale"
            )
        completion_hashes[phase] = str(
            completion["complete_sha256"]
        )
    document = {
        "schema_version": 1,
        "role": role,
        "intent_sha256": intent["intent_sha256"],
        "delete_manifest_sha256": delete_manifest["manifest_sha256"],
        "pre_control_sha256": pre_control["manifest_sha256"],
        "final_control_sha256": final_control["manifest_sha256"],
        "pre_control_complete_sha256": completion_hashes["pre"],
        "final_control_complete_sha256": completion_hashes["final"],
        "ready_sha256": "",
    }
    document["ready_sha256"] = verifier.document_hash(
        document, "ready_sha256"
    )
    if _immutable_artifact_exists(
        path, label=f"{role} delete-ready binding"
    ):
        existing = runtime.load_json(path)
        if existing != document:
            raise runtime.HarnessError(
                f"v2 clean {role} delete-ready binding is stale"
            )
    else:
        runtime.atomic_create_json(path, document)
    return document


def _verify_linked_admin_archive(
    *,
    task_dir: Path,
    role: str,
    record: Mapping[str, Any],
) -> None:
    admin = Path(str(record["git_admin_path"]))
    if not admin.exists() and not admin.is_symlink():
        return
    if not admin.is_dir() or admin.is_symlink():
        raise runtime.HarnessError(
            f"v2 clean {role} linked Git admin path is unsafe"
        )
    manifest_path = (
        task_dir / f"clean-git-control-{role}-final.json"
    )
    if not _immutable_artifact_exists(
        manifest_path,
        label=f"{role} final Git-control manifest",
    ):
        raise runtime.HarnessError(
            f"v2 clean {role} final Git-control manifest is missing"
        )
    document = runtime.load_json(manifest_path)
    current_root, current_entries = _clean_directory_projection(
        admin,
        max_regular_bytes=GIT_CONTROL_ARCHIVE_LIMIT,
    )
    if (
        document.get("control_root") != str(admin)
        or document.get("root") != current_root
        or document.get("entries") != current_entries
    ):
        raise runtime.HarnessError(
            f"refusing clean: {role} linked Git admin metadata "
            "changed after delete-start"
        )


def _stage_linked_admin(
    *,
    task_dir: Path,
    role: str,
    record: Mapping[str, Any],
    ready: Mapping[str, Any],
) -> Dict[str, Any]:
    admin = Path(str(record["git_admin_path"]))
    staging = Path(str(record["git_admin_staging_path"]))
    admin_exists = admin.exists() or admin.is_symlink()
    staging_exists = staging.exists() or staging.is_symlink()
    if admin_exists and staging_exists:
        raise runtime.HarnessError(
            f"v2 clean {role} found live and staged Git admin roots"
        )
    manifest_path = (
        task_dir / f"clean-git-control-{role}-final.json"
    )
    if not _immutable_artifact_exists(
        manifest_path,
        label=f"{role} final Git-control manifest",
    ):
        raise runtime.HarnessError(
            f"v2 clean {role} final Git-control manifest is missing"
        )
    manifest = runtime.load_json(manifest_path)
    if admin_exists:
        _verify_linked_admin_archive(
            task_dir=task_dir,
            role=role,
            record=record,
        )
        os.rename(admin, staging)
        parent_fd = os.open(
            admin.parent,
            os.O_RDONLY | int(getattr(os, "O_DIRECTORY", 0)),
        )
        try:
            os.fsync(parent_fd)
        finally:
            os.close(parent_fd)
        staging_exists = True
    if not staging_exists or not staging.is_dir() or staging.is_symlink():
        raise runtime.HarnessError(
            f"v2 clean {role} staged Git admin root is missing or unsafe"
        )
    current_root, current_entries = _clean_directory_projection(
        staging,
        max_regular_bytes=GIT_CONTROL_ARCHIVE_LIMIT,
    )
    if (
        manifest.get("root") != current_root
        or manifest.get("entries") != current_entries
    ):
        raise runtime.HarnessError(
            f"refusing clean: {role} staged Git admin metadata changed"
        )
    document = {
        "schema_version": 1,
        "role": role,
        "ready_sha256": ready["ready_sha256"],
        "control_manifest_sha256": manifest["manifest_sha256"],
        "original_path": str(admin),
        "staging_path": str(staging),
        "admin_stage_sha256": "",
    }
    document["admin_stage_sha256"] = verifier.document_hash(
        document, "admin_stage_sha256"
    )
    stage_path = task_dir / f"clean-admin-stage-{role}.json"
    if _immutable_artifact_exists(
        stage_path, label=f"{role} Git admin stage completion"
    ):
        if runtime.load_json(stage_path) != document:
            raise runtime.HarnessError(
                f"v2 clean {role} Git admin stage completion is stale"
            )
    else:
        runtime.atomic_create_json(stage_path, document)
    return document


def _load_admin_stage(
    *,
    task_dir: Path,
    role: str,
    ready: Mapping[str, Any],
) -> Dict[str, Any]:
    path = task_dir / f"clean-admin-stage-{role}.json"
    if not _immutable_artifact_exists(
        path, label=f"{role} Git admin stage completion"
    ):
        raise runtime.HarnessError(
            f"v2 clean {role} Git admin stage completion is missing"
        )
    document = runtime.load_json(path)
    if (
        document.get("admin_stage_sha256")
        != verifier.document_hash(
            document, "admin_stage_sha256"
        )
        or document.get("ready_sha256") != ready["ready_sha256"]
    ):
        raise runtime.HarnessError(
            f"v2 clean {role} Git admin stage completion is stale"
        )
    return document


def _validate_staged_git_admin(
    *,
    task_dir: Path,
    role: str,
    record: Mapping[str, Any],
    admin_stage: Mapping[str, Any],
    require_complete: bool,
) -> None:
    original = Path(str(record["git_admin_path"]))
    staging = Path(str(record["git_admin_staging_path"]))
    if original.exists() or original.is_symlink():
        raise runtime.HarnessError(
            f"v2 clean {role} Git admin root reappeared before "
            "global validation"
        )
    manifest_path = (
        task_dir / f"clean-git-control-{role}-final.json"
    )
    if not _immutable_artifact_exists(
        manifest_path,
        label=f"{role} final Git-control manifest",
    ):
        raise runtime.HarnessError(
            f"v2 clean {role} final Git-control manifest is missing"
        )
    manifest = runtime.load_json(manifest_path)
    if (
        admin_stage.get("admin_stage_sha256")
        != verifier.document_hash(
            admin_stage, "admin_stage_sha256"
        )
        or admin_stage.get("original_path") != str(original)
        or admin_stage.get("staging_path") != str(staging)
        or admin_stage.get("control_manifest_sha256")
        != manifest.get("manifest_sha256")
    ):
        raise runtime.HarnessError(
            f"v2 clean {role} Git admin stage binding is stale"
        )
    if staging.exists() or staging.is_symlink():
        if not staging.is_dir() or staging.is_symlink():
            raise runtime.HarnessError(
                f"v2 clean {role} staged Git admin root is unsafe"
            )
        metadata = staging.lstat()
        root = manifest["root"]
        if (
            metadata.st_dev != int(root["device"])
            or metadata.st_ino != int(root["inode"])
        ):
            raise runtime.HarnessError(
                f"v2 clean {role} staged Git admin identity changed"
            )
        expected = {
            str(entry["path"]): entry
            for entry in manifest["entries"]
        }
        current = {
            str(entry["path"]): entry
            for entry in _clean_tree_snapshot(staging)
        }
        extras = sorted(set(current) - set(expected))
        if extras:
            raise runtime.HarnessError(
                f"v2 clean {role} staged Git admin gained entries: "
                + ", ".join(extras[:20])
            )
        for relative, entry in current.items():
            if not _clean_entry_matches(expected[relative], entry):
                raise runtime.HarnessError(
                    f"v2 clean {role} staged Git admin changed: "
                    f"{relative}"
                )
        if require_complete and set(current) != set(expected):
            raise runtime.HarnessError(
                f"v2 clean {role} staged Git admin is incomplete before "
                "global validation"
            )
    elif require_complete:
        raise runtime.HarnessError(
            f"v2 clean {role} staged Git admin root disappeared before "
            "global validation"
        )


def _purge_staged_git_admin(
    *,
    task_dir: Path,
    role: str,
    record: Mapping[str, Any],
    admin_stage: Mapping[str, Any],
) -> None:
    _validate_staged_git_admin(
        task_dir=task_dir,
        role=role,
        record=record,
        admin_stage=admin_stage,
        require_complete=False,
    )
    original = Path(str(record["git_admin_path"]))
    staging = Path(str(record["git_admin_staging_path"]))
    manifest = runtime.load_json(
        task_dir / f"clean-git-control-{role}-final.json"
    )
    if staging.exists() or staging.is_symlink():
        expected = {
            str(entry["path"]): entry
            for entry in manifest["entries"]
        }
        current = {
            str(entry["path"]): entry
            for entry in _clean_tree_snapshot(staging)
        }
        flags = os.O_RDONLY | int(getattr(os, "O_DIRECTORY", 0))
        flags |= int(getattr(os, "O_CLOEXEC", 0))
        flags |= int(getattr(os, "O_NOFOLLOW", 0))
        root_fd = os.open(staging, flags)
        try:
            leaves = sorted(
                (
                    entry
                    for entry in manifest["entries"]
                    if entry["kind"] != "directory"
                ),
                key=lambda entry: (
                    -str(entry["path"]).count("/"),
                    str(entry["path"]),
                ),
            )
            for entry in leaves:
                relative = str(entry["path"])
                parent, _, name = relative.rpartition("/")
                parent_fd = _open_clean_parent_fd(root_fd, parent)
                if parent_fd is None:
                    continue
                try:
                    try:
                        value = _clean_entry_snapshot_at(
                            parent_fd,
                            name,
                            relative,
                            int(entry["device"]),
                        )
                    except FileNotFoundError:
                        continue
                    if not _clean_entry_matches(entry, value):
                        raise runtime.HarnessError(
                            f"v2 clean {role} staged Git admin leaf "
                            "changed"
                        )
                    os.unlink(name, dir_fd=parent_fd)
                finally:
                    os.close(parent_fd)
            directories = sorted(
                (
                    entry
                    for entry in manifest["entries"]
                    if entry["kind"] == "directory"
                ),
                key=lambda entry: (
                    -str(entry["path"]).count("/"),
                    str(entry["path"]),
                ),
            )
            for entry in directories:
                relative = str(entry["path"])
                parent, _, name = relative.rpartition("/")
                parent_fd = _open_clean_parent_fd(root_fd, parent)
                if parent_fd is None:
                    continue
                try:
                    try:
                        value = _clean_entry_snapshot_at(
                            parent_fd,
                            name,
                            relative,
                            int(entry["device"]),
                        )
                    except FileNotFoundError:
                        continue
                    if not _clean_entry_matches(entry, value):
                        raise runtime.HarnessError(
                            f"v2 clean {role} staged Git admin "
                            "directory changed"
                        )
                    os.rmdir(name, dir_fd=parent_fd)
                finally:
                    os.close(parent_fd)
            with os.scandir(root_fd) as iterator:
                leftovers = sorted(entry.name for entry in iterator)
            if leftovers:
                raise runtime.HarnessError(
                    f"v2 clean {role} staged Git admin is not empty: "
                    + ", ".join(leftovers[:20])
                )
            metadata = os.fstat(root_fd)
            root = manifest["root"]
            if (
                metadata.st_dev != int(root["device"])
                or metadata.st_ino != int(root["inode"])
            ):
                raise runtime.HarnessError(
                    f"v2 clean {role} staged Git admin identity changed"
                )
        finally:
            os.close(root_fd)
        os.rmdir(staging)
    if original.exists() or original.is_symlink():
        raise runtime.HarnessError(
            f"v2 clean {role} Git admin root survived purge"
        )


def _mark_clean_delete_started(
    *,
    task_dir: Path,
    role: str,
    ready: Mapping[str, Any],
) -> Dict[str, Any]:
    path = task_dir / f"clean-delete-started-{role}.json"
    document = {
        "schema_version": 1,
        "role": role,
        "ready_sha256": ready["ready_sha256"],
        "started_sha256": "",
    }
    document["started_sha256"] = verifier.document_hash(
        document, "started_sha256"
    )
    if _immutable_artifact_exists(
        path, label=f"{role} delete-start binding"
    ):
        existing = runtime.load_json(path)
        if existing != document:
            raise runtime.HarnessError(
                f"v2 clean {role} delete-start binding is stale"
            )
    else:
        runtime.atomic_create_json(path, document)
    return document


def _require_quarantine_idle(root: Path) -> None:
    lsof = Path("/usr/sbin/lsof")
    executable = str(lsof) if lsof.is_file() else shutil.which("lsof")
    if not executable:
        raise runtime.HarnessError(
            "v2 clean requires lsof before deleting a quarantined worktree"
        )
    try:
        completed = subprocess.run(
            [executable, "-nP", "-F0", "+D", str(root)],
            cwd=str(root.parent),
            text=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=60,
            check=False,
        )
    except subprocess.TimeoutExpired as exc:
        raise runtime.HarnessError(
            "v2 clean lsof quarantine inspection timed out"
        ) from exc
    if completed.stdout:
        excerpt = completed.stdout[:512].decode("utf-8", "replace")
        raise runtime.HarnessError(
            "v2 clean quarantine is still used by a process: " + excerpt
        )
    if completed.returncode != 1 or completed.stderr:
        detail = (completed.stderr or completed.stdout)[:512].decode(
            "utf-8", "replace"
        )
        raise runtime.HarnessError(
            "v2 clean could not prove quarantine process liveness: " + detail
        )


def _freeze_clean_delete_manifest(
    *,
    task_dir: Path,
    role: str,
    root: Path,
    staging: Path,
    intent: Mapping[str, Any],
    expected_tree: str,
    expected_revision: Optional[str],
    ignored_policy: str,
) -> Dict[str, Any]:
    manifest_path = task_dir / f"clean-delete-{role}.json"
    if _immutable_artifact_exists(
        manifest_path, label=f"{role} deletion manifest"
    ):
        document = runtime.load_json(manifest_path)
        if document.get("manifest_sha256") != verifier.document_hash(
            document, "manifest_sha256"
        ):
            raise runtime.HarnessError(
                f"v2 clean {role} deletion manifest hash mismatch"
            )
        for key, expected in (
            ("schema_version", 2),
            ("role", role),
            ("intent_sha256", intent["intent_sha256"]),
            ("root_path", str(root)),
            ("staging_path", str(staging)),
            ("expected_tree", expected_tree),
            ("expected_revision", expected_revision),
        ):
            if document.get(key) != expected:
                raise runtime.HarnessError(
                    f"v2 clean {role} deletion manifest {key} is stale"
                )
        return document

    _require_quarantine_idle(root.parent)
    ignored_before = set(_ignored_paths(root))
    unknown = sorted(
        path
        for path in ignored_before
        if not _ignored_path_allowed(
            root,
            path,
            policy=ignored_policy,
        )
    )
    if unknown:
        shown = ", ".join(unknown[:20])
        raise runtime.HarnessError(
            f"refusing clean: {role} ignored bytes are not archived: {shown}"
    )
    visible_before = _clean_visible_paths(root)
    managed_node_modules = runtime.managed_node_modules_link(root)
    if _visible_tree(root, task_dir / "runtime") != expected_tree:
        raise runtime.HarnessError(
            f"refusing clean: {role} bytes changed after durable evidence"
        )
    entries_before = _clean_tree_snapshot(root)
    # A second exact-tree proof catches visible writes racing the first
    # snapshot. It may add Git objects inside a standalone clone, so freeze the
    # final control metadata only after this proof.
    if _visible_tree(root, task_dir / "runtime") != expected_tree:
        raise runtime.HarnessError(
            f"refusing clean: {role} bytes changed during manifest freeze"
        )
    _verify_tree_matches_raw_bytes(
        root,
        expected_tree,
        label=f"{role} freeze",
    )
    entries = _clean_tree_snapshot(root)
    before_working = {
        str(entry["path"]): entry
        for entry in entries_before
        if not (
            str(entry["path"]) == ".git"
            or str(entry["path"]).startswith(".git/")
        )
    }
    final_working = {
        str(entry["path"]): entry
        for entry in entries
        if not (
            str(entry["path"]) == ".git"
            or str(entry["path"]).startswith(".git/")
        )
    }
    if set(before_working) != set(final_working) or any(
        not _clean_entry_matches(before_working[path], final_working[path])
        for path in before_working
    ):
        raise runtime.HarnessError(
            f"refusing clean: {role} filesystem changed during freeze"
        )
    leaf_paths = {
        str(entry["path"])
        for entry in entries
        if entry["kind"] != "directory"
    }
    classified: List[Dict[str, Any]] = []
    for index, entry in enumerate(entries):
        relative = str(entry["path"])
        if entry["kind"] == "directory":
            classification = "directory"
        elif relative == ".git" or relative.startswith(".git/"):
            classification = "git-control"
        elif relative == "node_modules" and managed_node_modules:
            classification = "managed-node-modules"
        elif relative in ignored_before:
            classification = "disposable"
        elif relative in visible_before:
            classification = "archived"
        else:
            raise runtime.HarnessError(
                f"v2 clean found an unclassified late entry: {role}:{relative}"
            )
        classified.append(
            {
                **entry,
                "classification": classification,
                "staging_name": f"entry-{index:08d}",
            }
        )
    if not ignored_before.issubset(leaf_paths):
        raise runtime.HarnessError(
            f"v2 clean {role} ignored path set differs from filesystem"
        )
    if set(_ignored_paths(root)) != ignored_before:
        raise runtime.HarnessError(
            f"refusing clean: {role} ignored bytes changed during freeze"
        )
    if _clean_visible_paths(root) != visible_before:
        raise runtime.HarnessError(
            f"refusing clean: {role} visible path set changed during freeze"
        )
    _require_quarantine_idle(root.parent)
    root_metadata = root.lstat()
    if not stat.S_ISDIR(root_metadata.st_mode):
        raise runtime.HarnessError(
            f"v2 clean {role} quarantine root is not a directory"
        )
    document: Dict[str, Any] = {
        "schema_version": 2,
        "role": role,
        "intent_sha256": intent["intent_sha256"],
        "root_path": str(root),
        "staging_path": str(staging),
        "expected_tree": expected_tree,
        "expected_revision": expected_revision,
        "root_device": root_metadata.st_dev,
        "root_inode": root_metadata.st_ino,
        "entries": classified,
        "manifest_sha256": "",
    }
    document["manifest_sha256"] = verifier.document_hash(
        document, "manifest_sha256"
    )
    runtime.atomic_create_json(manifest_path, document)
    return document


def _open_clean_parent_fd(root_fd: int, relative_parent: str) -> Optional[int]:
    current = os.dup(root_fd)
    flags = os.O_RDONLY | int(getattr(os, "O_DIRECTORY", 0))
    flags |= int(getattr(os, "O_CLOEXEC", 0))
    flags |= int(getattr(os, "O_NOFOLLOW", 0))
    try:
        if relative_parent:
            for component in relative_parent.split("/"):
                try:
                    following = os.open(
                        component, flags, dir_fd=current
                    )
                except FileNotFoundError:
                    os.close(current)
                    return None
                os.close(current)
                current = following
        return current
    except Exception:
        os.close(current)
        raise


def _stage_frozen_clean_tree(
    root: Path,
    staging: Path,
    manifest: Mapping[str, Any],
    *,
    original_path: Path,
    task_dir: Path,
    role: str,
    ready: Mapping[str, Any],
) -> Dict[str, Any]:
    if original_path.exists() or original_path.is_symlink():
        raise runtime.HarnessError(
            "v2 clean original path reappeared after quarantine"
        )
    if not root.is_dir() or root.is_symlink():
        raise runtime.HarnessError(
            f"v2 clean {role} quarantine root is missing or unsafe"
        )
    _ensure_private_durable_directory(
        staging, label=f"{role} leaf staging directory"
    )
    _require_quarantine_idle(root.parent)
    expected_entries = {
        str(entry["path"]): entry for entry in manifest["entries"]
    }
    current_entries = {
        str(entry["path"]): entry for entry in _clean_tree_snapshot(root)
    }
    extras = sorted(set(current_entries) - set(expected_entries))
    if extras:
        raise runtime.HarnessError(
            f"v2 clean {role} quarantine gained unmanifested entries: "
            + ", ".join(extras[:20])
        )
    for relative, current in current_entries.items():
        if not _clean_entry_matches(expected_entries[relative], current):
            raise runtime.HarnessError(
                f"v2 clean {role} quarantine entry changed: {relative}"
            )

    flags = os.O_RDONLY | int(getattr(os, "O_DIRECTORY", 0))
    flags |= int(getattr(os, "O_CLOEXEC", 0))
    flags |= int(getattr(os, "O_NOFOLLOW", 0))
    root_fd = os.open(root, flags)
    staging_fd = os.open(staging, flags)
    staged_paths: List[str] = []
    missing_paths: List[str] = []
    try:
        leaves = sorted(
            (
                entry
                for entry in manifest["entries"]
                if entry["kind"] != "directory"
            ),
            key=lambda entry: (
                entry["path"] == ".git",
                -str(entry["path"]).count("/"),
                str(entry["path"]),
            ),
        )
        expected_staging = {
            str(entry["staging_name"]): entry for entry in leaves
        }
        staging_names = sorted(
            entry.name for entry in os.scandir(staging_fd)
        )
        unknown_staging = sorted(
            set(staging_names) - set(expected_staging)
        )
        if unknown_staging:
            raise runtime.HarnessError(
                f"v2 clean {role} staging has unknown entries: "
                + ", ".join(unknown_staging[:20])
            )
        for entry in leaves:
            relative = str(entry["path"])
            parent, _, name = relative.rpartition("/")
            parent_fd = _open_clean_parent_fd(root_fd, parent)
            staged_name = str(entry["staging_name"])
            try:
                try:
                    source = (
                        _clean_entry_snapshot_at(
                            parent_fd,
                            name,
                            relative,
                            int(entry["device"]),
                        )
                        if parent_fd is not None
                        else None
                    )
                except FileNotFoundError:
                    source = None
                try:
                    staged = _clean_entry_snapshot_at(
                        staging_fd,
                        staged_name,
                        relative,
                        int(entry["device"]),
                    )
                except FileNotFoundError:
                    staged = None
                if source is not None and staged is not None:
                    raise runtime.HarnessError(
                        f"v2 clean {role} found source and staged "
                        f"copies: {relative}"
                    )
                if source is not None:
                    if not _clean_entry_matches(entry, source):
                        raise runtime.HarnessError(
                            f"v2 clean {role} source changed before "
                            f"staging: {relative}"
                        )
                    os.rename(
                        name,
                        staged_name,
                        src_dir_fd=parent_fd,
                        dst_dir_fd=staging_fd,
                    )
                    os.fsync(parent_fd)
                    os.fsync(staging_fd)
                    staged = _clean_entry_snapshot_at(
                        staging_fd,
                        staged_name,
                        relative,
                        int(entry["device"]),
                    )
                if staged is None:
                    missing_paths.append(relative)
                else:
                    if not _clean_entry_matches(entry, staged):
                        raise runtime.HarnessError(
                            f"v2 clean {role} staged entry changed: "
                            f"{relative}"
                        )
                    staged_paths.append(relative)
            finally:
                if parent_fd is not None:
                    os.close(parent_fd)
    finally:
        os.close(staging_fd)
        os.close(root_fd)

    after_entries = _clean_tree_snapshot(root)
    for entry in after_entries:
        expected = expected_entries.get(str(entry["path"]))
        if (
            expected is None
            or entry["kind"] != "directory"
            or not _clean_entry_matches(expected, entry)
        ):
            raise runtime.HarnessError(
                f"v2 clean {role} root changed during staging"
            )
    leaves = {
        str(entry["path"])
        for entry in manifest["entries"]
        if entry["kind"] != "directory"
    }
    if set(staged_paths) | set(missing_paths) != leaves:
        raise runtime.HarnessError(
            f"v2 clean {role} staged leaf set is incomplete"
        )
    document = {
        "schema_version": 1,
        "role": role,
        "manifest_sha256": manifest["manifest_sha256"],
        "ready_sha256": ready["ready_sha256"],
        "staged": sorted(staged_paths),
        "missing": sorted(missing_paths),
        "stage_sha256": "",
    }
    document["stage_sha256"] = verifier.document_hash(
        document, "stage_sha256"
    )
    stage_path = task_dir / f"clean-stage-{role}.json"
    if _immutable_artifact_exists(
        stage_path, label=f"{role} stage completion"
    ):
        if runtime.load_json(stage_path) != document:
            raise runtime.HarnessError(
                f"v2 clean {role} stage completion is stale"
            )
    else:
        runtime.atomic_create_json(stage_path, document)
    return document


def _load_clean_stage(
    *,
    task_dir: Path,
    role: str,
    manifest: Mapping[str, Any],
    ready: Mapping[str, Any],
) -> Dict[str, Any]:
    path = task_dir / f"clean-stage-{role}.json"
    if not _immutable_artifact_exists(
        path, label=f"{role} stage completion"
    ):
        raise runtime.HarnessError(
            f"v2 clean {role} stage completion is missing"
        )
    document = runtime.load_json(path)
    if (
        document.get("stage_sha256")
        != verifier.document_hash(document, "stage_sha256")
        or document.get("manifest_sha256")
        != manifest["manifest_sha256"]
        or document.get("ready_sha256") != ready["ready_sha256"]
    ):
        raise runtime.HarnessError(
            f"v2 clean {role} stage completion is stale"
        )
    leaves = {
        str(entry["path"])
        for entry in manifest["entries"]
        if entry["kind"] != "directory"
    }
    staged = document.get("staged")
    missing = document.get("missing")
    if (
        not isinstance(staged, list)
        or not isinstance(missing, list)
        or set(staged) & set(missing)
        or set(staged) | set(missing) != leaves
    ):
        raise runtime.HarnessError(
            f"v2 clean {role} stage leaf set is malformed"
        )
    return document


def _validate_frozen_clean_tree(
    root: Path,
    staging: Path,
    manifest: Mapping[str, Any],
    stage: Mapping[str, Any],
    *,
    original_path: Path,
    require_complete: bool,
) -> None:
    if original_path.exists() or original_path.is_symlink():
        raise runtime.HarnessError(
            "v2 clean original path reappeared before global validation"
        )
    expected_entries = {
        str(entry["path"]): entry for entry in manifest["entries"]
    }
    expected_directories = {
        str(entry["path"]): entry
        for entry in manifest["entries"]
        if entry["kind"] == "directory"
    }
    expected_staged = {
        str(entry["staging_name"]): entry
        for entry in manifest["entries"]
        if str(entry["path"]) in set(stage["staged"])
    }
    if root.exists() or root.is_symlink():
        if not root.is_dir() or root.is_symlink():
            raise runtime.HarnessError(
                "v2 clean quarantine root is unsafe"
            )
        metadata = root.lstat()
        if (
            metadata.st_dev != int(manifest["root_device"])
            or metadata.st_ino != int(manifest["root_inode"])
        ):
            raise runtime.HarnessError(
                "v2 clean quarantine root identity changed"
            )
        current_entries = _clean_tree_snapshot(root)
        for entry in current_entries:
            expected = expected_entries.get(str(entry["path"]))
            if (
                expected is None
                or entry["kind"] != "directory"
                or not _clean_entry_matches(expected, entry)
            ):
                raise runtime.HarnessError(
                    "v2 clean quarantine changed after all-roots staging"
                )
        if require_complete and {
            str(entry["path"]) for entry in current_entries
        } != set(expected_directories):
            raise runtime.HarnessError(
                "v2 clean quarantine directories are incomplete before "
                "global validation"
            )
    elif require_complete:
        raise runtime.HarnessError(
            "v2 clean quarantine root disappeared before global validation"
        )
    if staging.exists() or staging.is_symlink():
        if not staging.is_dir() or staging.is_symlink():
            raise runtime.HarnessError("v2 clean staging path is unsafe")
        staging_names = sorted(entry.name for entry in os.scandir(staging))
        extras = sorted(set(staging_names) - set(expected_staged))
        if extras:
            raise runtime.HarnessError(
                "v2 clean staging gained unmanifested entries: "
                + ", ".join(extras[:20])
            )
        flags = os.O_RDONLY | int(getattr(os, "O_DIRECTORY", 0))
        flags |= int(getattr(os, "O_CLOEXEC", 0))
        flags |= int(getattr(os, "O_NOFOLLOW", 0))
        staging_fd = os.open(staging, flags)
        try:
            for staged_name in staging_names:
                entry = expected_staged[staged_name]
                current = _clean_entry_snapshot_at(
                    staging_fd,
                    staged_name,
                    str(entry["path"]),
                    int(entry["device"]),
                )
                if not _clean_entry_matches(entry, current):
                    raise runtime.HarnessError(
                        "v2 clean staged payload changed before global "
                        "validation"
                    )
        finally:
            os.close(staging_fd)
        if require_complete and set(staging_names) != set(expected_staged):
            raise runtime.HarnessError(
                "v2 clean staged payload set is incomplete before global "
                "validation"
            )
    elif require_complete and expected_staged:
        raise runtime.HarnessError(
            "v2 clean staging path disappeared before global validation"
        )


def _purge_frozen_clean_tree(
    root: Path,
    staging: Path,
    manifest: Mapping[str, Any],
    stage: Mapping[str, Any],
    *,
    original_path: Path,
) -> None:
    _validate_frozen_clean_tree(
        root,
        staging,
        manifest,
        stage,
        original_path=original_path,
        require_complete=False,
    )
    expected_entries = {
        str(entry["path"]): entry for entry in manifest["entries"]
    }
    expected_staged = {
        str(entry["staging_name"]): entry
        for entry in manifest["entries"]
        if str(entry["path"]) in set(stage["staged"])
    }
    if staging.exists() or staging.is_symlink():
        staging_names = sorted(entry.name for entry in os.scandir(staging))
        flags = os.O_RDONLY | int(getattr(os, "O_DIRECTORY", 0))
        flags |= int(getattr(os, "O_CLOEXEC", 0))
        flags |= int(getattr(os, "O_NOFOLLOW", 0))
        staging_fd = os.open(staging, flags)
        try:
            for staged_name in staging_names:
                entry = expected_staged[staged_name]
                current = _clean_entry_snapshot_at(
                    staging_fd,
                    staged_name,
                    str(entry["path"]),
                    int(entry["device"]),
                )
                if not _clean_entry_matches(entry, current):
                    raise runtime.HarnessError(
                        "v2 clean staged payload changed before purge"
                    )
            for staged_name in staging_names:
                os.unlink(staged_name, dir_fd=staging_fd)
            os.fsync(staging_fd)
        finally:
            os.close(staging_fd)

    if root.exists():
        flags = os.O_RDONLY | int(getattr(os, "O_DIRECTORY", 0))
        flags |= int(getattr(os, "O_CLOEXEC", 0))
        flags |= int(getattr(os, "O_NOFOLLOW", 0))
        root_fd = os.open(root, flags)
        try:
            directories = sorted(
                (
                    entry
                    for entry in manifest["entries"]
                    if entry["kind"] == "directory"
                ),
                key=lambda entry: (
                    -str(entry["path"]).count("/"),
                    str(entry["path"]),
                ),
            )
            for entry in directories:
                relative = str(entry["path"])
                parent, _, name = relative.rpartition("/")
                parent_fd = _open_clean_parent_fd(root_fd, parent)
                if parent_fd is None:
                    continue
                try:
                    try:
                        current = _clean_entry_snapshot_at(
                            parent_fd,
                            name,
                            relative,
                            int(entry["device"]),
                        )
                    except FileNotFoundError:
                        continue
                    if not _clean_entry_matches(entry, current):
                        raise runtime.HarnessError(
                            f"v2 clean directory changed: {relative}"
                        )
                    os.rmdir(name, dir_fd=parent_fd)
                finally:
                    os.close(parent_fd)
            leftovers = sorted(entry.name for entry in os.scandir(root_fd))
            if leftovers:
                raise runtime.HarnessError(
                    "v2 clean retained unmanifested root entries: "
                    + ", ".join(leftovers[:20])
                )
            root_metadata = os.fstat(root_fd)
            if (
                root_metadata.st_dev != int(manifest["root_device"])
                or root_metadata.st_ino != int(manifest["root_inode"])
            ):
                raise runtime.HarnessError(
                    "v2 clean quarantine root identity changed"
                )
        finally:
            os.close(root_fd)
        parent_fd = os.open(root.parent, flags)
        try:
            os.rmdir(root.name, dir_fd=parent_fd)
        finally:
            os.close(parent_fd)
    if staging.exists():
        os.rmdir(staging)
    if root.parent.exists():
        os.rmdir(root.parent)
    if original_path.exists() or original_path.is_symlink():
        raise runtime.HarnessError(
            "v2 clean original path reappeared during purge"
        )


def _quarantine_clean_root(
    *,
    original: Path,
    quarantine: Path,
    expected_device: int,
    expected_inode: int,
    git_owner: Optional[Path],
) -> bool:
    original_exists = original.exists() or original.is_symlink()
    quarantine_exists = quarantine.exists() or quarantine.is_symlink()
    if original_exists and quarantine_exists:
        raise runtime.HarnessError(
            "v2 clean found both original and quarantined roots"
        )
    if not original_exists and not quarantine_exists:
        return False

    if quarantine.parent.exists() or quarantine.parent.is_symlink():
        metadata = quarantine.parent.lstat()
        if not stat.S_ISDIR(metadata.st_mode) or stat.S_ISLNK(metadata.st_mode):
            raise runtime.HarnessError("v2 clean quarantine parent is unsafe")
        if stat.S_IMODE(metadata.st_mode) & 0o077:
            raise runtime.HarnessError(
                "v2 clean quarantine parent is not private"
            )
    else:
        quarantine.parent.mkdir(mode=0o700)
    os.chmod(quarantine.parent, 0o700)

    if original_exists:
        # Refuse before the rename when the invoking shell, an editor, or any
        # other process still has cwd/fds inside the original tree.
        _require_quarantine_idle(original)
        metadata = original.lstat()
        if (
            not stat.S_ISDIR(metadata.st_mode)
            or stat.S_ISLNK(metadata.st_mode)
            or metadata.st_dev != expected_device
            or metadata.st_ino != expected_inode
        ):
            raise runtime.HarnessError(
                "v2 clean original root identity changed before quarantine"
            )
        parent_metadata = quarantine.parent.lstat()
        if parent_metadata.st_dev != metadata.st_dev:
            raise runtime.HarnessError(
                "v2 clean quarantine crosses a filesystem boundary"
            )
        if any(quarantine.parent.iterdir()):
            raise runtime.HarnessError(
                "v2 clean quarantine parent was not empty before move"
            )
        if git_owner is not None:
            runtime.run_capture(
                [
                    "git",
                    "worktree",
                    "move",
                    "--",
                    str(original),
                    str(quarantine),
                ],
                git_owner,
            )
        else:
            os.rename(original, quarantine)

    if original.exists() or original.is_symlink():
        raise runtime.HarnessError(
            "v2 clean original root remained after quarantine"
        )
    metadata = quarantine.lstat()
    if (
        not stat.S_ISDIR(metadata.st_mode)
        or stat.S_ISLNK(metadata.st_mode)
        or metadata.st_dev != expected_device
        or metadata.st_ino != expected_inode
    ):
        raise runtime.HarnessError(
            "v2 clean quarantined root identity is stale"
        )
    if git_owner is not None:
        worktrees = {
            line.removeprefix("worktree ")
            for line in runtime.git(
                git_owner, "worktree", "list", "--porcelain"
            ).splitlines()
            if line.startswith("worktree ")
        }
        if str(quarantine) not in worktrees or str(original) in worktrees:
            raise runtime.HarnessError(
                "v2 clean Git worktree registration did not move to quarantine"
            )
    return True


def _prune_quarantined_worktree(
    owner: Path,
    *,
    original: Path,
    quarantine: Path,
    admin_path: Path,
) -> None:
    common = Path(
        runtime.git(
            owner,
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        )
    ).resolve()
    if admin_path.parent != common / "worktrees":
        raise runtime.HarnessError("v2 clean Git admin prune target is unsafe")
    if admin_path.exists() or admin_path.is_symlink():
        # The filesystem root is already gone. Non-force remove now performs an
        # exact metadata cleanup and cannot recursively delete task bytes.
        runtime.run_capture(
            ["git", "worktree", "remove", "--", str(quarantine)],
            owner,
        )
    if admin_path.exists() or admin_path.is_symlink():
        raise runtime.HarnessError("v2 clean Git admin path survived prune")
    worktrees = {
        line.removeprefix("worktree ")
        for line in runtime.git(
            owner, "worktree", "list", "--porcelain"
        ).splitlines()
        if line.startswith("worktree ")
    }
    if str(original) in worktrees or str(quarantine) in worktrees:
        raise runtime.HarnessError(
            "v2 clean removed root remains registered as a worktree"
        )


def _verify_clean_root_revision(
    root: Path,
    record: Mapping[str, Any],
    *,
    role: str,
) -> None:
    expected_revision = record.get("expected_revision")
    if expected_revision is None:
        return
    if not root.is_dir() or root.is_symlink():
        raise runtime.HarnessError(
            f"v2 clean {role} revision root is missing or unsafe"
        )
    current_revision = runtime.git(
        root, "rev-parse", "HEAD", check=False
    )
    if current_revision != expected_revision:
        raise runtime.HarnessError(
            f"refusing clean: {role} revision changed"
        )


def _prevalidate_frozen_root_cleanup(
    *,
    task_dir: Path,
    intent: Mapping[str, Any],
    role: str,
    record: Mapping[str, Any],
    git_owner: Optional[Path],
) -> None:
    original = Path(str(record["path"]))
    quarantine = Path(str(record["quarantine_path"]))
    original_exists = original.exists() or original.is_symlink()
    quarantine_exists = quarantine.exists() or quarantine.is_symlink()
    if original_exists and quarantine_exists:
        raise runtime.HarnessError(
            f"v2 clean {role} found both original and quarantine roots"
        )
    started = _immutable_artifact_exists(
        task_dir / f"clean-delete-started-{role}.json",
        label=f"{role} delete-start binding",
    )
    if started:
        if original_exists:
            raise runtime.HarnessError(
                f"v2 clean {role} original root reappeared after "
                "deletion started"
            )
        for required in (
            f"clean-delete-{role}.json",
            f"clean-delete-ready-{role}.json",
            f"clean-git-control-{role}-pre.json",
            f"clean-git-control-{role}-pre-complete.json",
            f"clean-git-control-{role}-final.json",
            f"clean-git-control-{role}-final-complete.json",
        ):
            if not _immutable_artifact_exists(
                task_dir / required,
                label=f"{role} resume evidence",
            ):
                raise runtime.HarnessError(
                    f"v2 clean {role} resume evidence is incomplete"
                )
        _archive_git_control(
            task_dir=task_dir,
            intent=intent,
            archive_role=f"{role}-pre",
            repo_root=original,
            record=record,
            git_owner=git_owner,
            live=False,
        )
        _archive_git_control(
            task_dir=task_dir,
            intent=intent,
            archive_role=f"{role}-final",
            repo_root=quarantine,
            record=record,
            git_owner=git_owner,
            live=False,
        )
        return
    candidate = original if original_exists else quarantine
    if candidate.exists() or candidate.is_symlink():
        _verify_clean_root_revision(candidate, record, role=role)
    elif not _immutable_artifact_exists(
        task_dir / f"clean-delete-{role}.json",
        label=f"{role} deletion manifest",
    ):
        raise runtime.HarnessError(
            f"v2 clean {role} root disappeared before its deletion manifest"
        )
    _archive_git_control(
        task_dir=task_dir,
        intent=intent,
        archive_role=f"{role}-pre",
        repo_root=original,
        record=record,
        git_owner=git_owner,
        live=original_exists,
    )


def _prepare_frozen_root_cleanup(
    *,
    task_dir: Path,
    intent: Mapping[str, Any],
    role: str,
    record: Mapping[str, Any],
    git_owner: Optional[Path],
    ignored_policy: str,
) -> Dict[str, Any]:
    original = Path(str(record["path"]))
    quarantine = Path(str(record["quarantine_path"]))
    staging = Path(str(record["delete_staging_path"]))
    manifest_path = task_dir / f"clean-delete-{role}.json"
    started_path = task_dir / f"clean-delete-started-{role}.json"
    started = _immutable_artifact_exists(
        started_path, label=f"{role} delete-start binding"
    )
    pre_control = _archive_git_control(
        task_dir=task_dir,
        intent=intent,
        archive_role=f"{role}-pre",
        repo_root=original,
        record=record,
        git_owner=git_owner,
        live=(
            not started
            and (original.exists() or original.is_symlink())
        ),
    )
    present = _quarantine_clean_root(
        original=original,
        quarantine=quarantine,
        expected_device=int(record["device"]),
        expected_inode=int(record["inode"]),
        git_owner=None if started else git_owner,
    )
    if not present and not _immutable_artifact_exists(
        manifest_path, label=f"{role} deletion manifest"
    ):
        raise runtime.HarnessError(
            f"v2 clean {role} root disappeared before its deletion manifest"
        )
    if present and not started:
        _verify_clean_root_revision(quarantine, record, role=role)
    manifest = _freeze_clean_delete_manifest(
        task_dir=task_dir,
        role=role,
        root=quarantine,
        staging=staging,
        intent=intent,
        expected_tree=str(record["expected_tree"]),
        expected_revision=record.get("expected_revision"),
        ignored_policy=ignored_policy,
    )
    final_control = _archive_git_control(
        task_dir=task_dir,
        intent=intent,
        archive_role=f"{role}-final",
        repo_root=quarantine,
        record=record,
        git_owner=git_owner,
        live=present and not started,
    )
    ready = _bind_clean_delete_ready(
        task_dir=task_dir,
        intent=intent,
        role=role,
        delete_manifest=manifest,
        pre_control=pre_control,
        final_control=final_control,
    )
    if (
        manifest.get("root_device") != record["device"]
        or manifest.get("root_inode") != record["inode"]
    ):
        raise runtime.HarnessError(
            f"v2 clean {role} manifest root identity is stale"
        )
    return {
        "role": role,
        "record": record,
        "git_owner": git_owner,
        "task_dir": task_dir,
        "intent": intent,
        "original": original,
        "quarantine": quarantine,
        "staging": staging,
        "manifest": manifest,
        "pre_control": pre_control,
        "final_control": final_control,
        "ready": ready,
    }


def _start_frozen_root_cleanup(
    prepared: Mapping[str, Any],
) -> Dict[str, Any]:
    role = str(prepared["role"])
    record = prepared["record"]
    if not isinstance(record, Mapping):
        raise runtime.HarnessError(
            f"v2 clean {role} prepared record is malformed"
        )
    original = Path(str(prepared["original"]))
    quarantine = Path(str(prepared["quarantine"]))
    staging = Path(str(prepared["staging"]))
    manifest = prepared["manifest"]
    if not isinstance(manifest, Mapping):
        raise runtime.HarnessError(
            f"v2 clean {role} prepared manifest is malformed"
        )
    git_owner = prepared.get("git_owner")
    if git_owner is not None and not isinstance(git_owner, Path):
        raise runtime.HarnessError(
            f"v2 clean {role} prepared Git owner is malformed"
        )
    task_dir = prepared.get("task_dir")
    intent = prepared.get("intent")
    if not isinstance(task_dir, Path) or not isinstance(intent, Mapping):
        raise runtime.HarnessError(
            f"v2 clean {role} prepared recovery evidence is malformed"
        )
    ready = prepared.get("ready")
    if not isinstance(ready, Mapping):
        raise runtime.HarnessError(
            f"v2 clean {role} prepared ready binding is malformed"
        )
    started_path = task_dir / f"clean-delete-started-{role}.json"
    started = _immutable_artifact_exists(
        started_path, label=f"{role} delete-start binding"
    )
    _archive_git_control(
        task_dir=task_dir,
        intent=intent,
        archive_role=f"{role}-pre",
        repo_root=original,
        record=record,
        git_owner=git_owner,
        live=False,
    )
    _archive_git_control(
        task_dir=task_dir,
        intent=intent,
        archive_role=f"{role}-final",
        repo_root=quarantine,
        record=record,
        git_owner=git_owner,
        live=(
            not started
            and (quarantine.exists() or quarantine.is_symlink())
        ),
    )
    _bind_clean_delete_ready(
        task_dir=task_dir,
        intent=intent,
        role=role,
        delete_manifest=manifest,
        pre_control=prepared["pre_control"],
        final_control=prepared["final_control"],
    )
    if not started and (quarantine.exists() or quarantine.is_symlink()):
        _verify_clean_root_revision(quarantine, record, role=role)
    started_document = _mark_clean_delete_started(
        task_dir=task_dir,
        role=role,
        ready=ready,
    )
    if started and runtime.load_json(started_path) != started_document:
        raise runtime.HarnessError(
            f"v2 clean {role} delete-start binding changed"
        )
    return started_document


def _stage_prepared_root_cleanup(
    prepared: Mapping[str, Any],
) -> Dict[str, Any]:
    role = str(prepared["role"])
    record = prepared["record"]
    task_dir = prepared["task_dir"]
    ready = prepared["ready"]
    if (
        not isinstance(record, Mapping)
        or not isinstance(task_dir, Path)
        or not isinstance(ready, Mapping)
    ):
        raise runtime.HarnessError(
            f"v2 clean {role} prepared stage evidence is malformed"
        )
    root_stage = _stage_frozen_clean_tree(
        Path(str(prepared["quarantine"])),
        Path(str(prepared["staging"])),
        prepared["manifest"],
        original_path=Path(str(prepared["original"])),
        task_dir=task_dir,
        role=role,
        ready=ready,
    )
    admin_stage = None
    if prepared.get("git_owner") is not None:
        admin_stage = _stage_linked_admin(
            task_dir=task_dir,
            role=role,
            record=record,
            ready=ready,
        )
    return {
        "role": role,
        "root": root_stage,
        "admin": admin_stage,
    }


def _load_prepared_root_stage(
    prepared: Mapping[str, Any],
) -> Dict[str, Any]:
    role = str(prepared["role"])
    task_dir = prepared["task_dir"]
    ready = prepared["ready"]
    if not isinstance(task_dir, Path) or not isinstance(ready, Mapping):
        raise runtime.HarnessError(
            f"v2 clean {role} prepared stage evidence is malformed"
        )
    root_stage = _load_clean_stage(
        task_dir=task_dir,
        role=role,
        manifest=prepared["manifest"],
        ready=ready,
    )
    admin_stage = None
    if prepared.get("git_owner") is not None:
        admin_stage = _load_admin_stage(
            task_dir=task_dir,
            role=role,
            ready=ready,
        )
    return {
        "role": role,
        "root": root_stage,
        "admin": admin_stage,
    }


def _bind_all_roots_started(
    *,
    task_dir: Path,
    intent: Mapping[str, Any],
    prepared: Sequence[Mapping[str, Any]],
    started: Sequence[Mapping[str, Any]],
) -> Dict[str, Any]:
    path = task_dir / "clean-delete-all-started.json"
    document = {
        "schema_version": 1,
        "intent_sha256": intent["intent_sha256"],
        "roots": [
            {
                "role": str(root["role"]),
                "ready_sha256": root["ready"]["ready_sha256"],
                "started_sha256": start["started_sha256"],
            }
            for root, start in zip(prepared, started)
        ],
        "all_started_sha256": "",
    }
    document["all_started_sha256"] = verifier.document_hash(
        document, "all_started_sha256"
    )
    if _immutable_artifact_exists(
        path, label="all-roots delete-start barrier"
    ):
        if runtime.load_json(path) != document:
            raise runtime.HarnessError(
                "v2 clean all-roots delete-start barrier is stale"
            )
    else:
        runtime.atomic_create_json(path, document)
    return document


def _bind_all_roots_staged(
    *,
    task_dir: Path,
    intent: Mapping[str, Any],
    all_started: Mapping[str, Any],
    stages: Sequence[Mapping[str, Any]],
) -> Dict[str, Any]:
    path = task_dir / "clean-delete-all-staged.json"
    roots = []
    for stage in stages:
        root = stage["root"]
        admin = stage.get("admin")
        roots.append(
            {
                "role": stage["role"],
                "stage_sha256": root["stage_sha256"],
                "admin_stage_sha256": (
                    admin["admin_stage_sha256"]
                    if isinstance(admin, Mapping)
                    else None
                ),
            }
        )
    document = {
        "schema_version": 1,
        "intent_sha256": intent["intent_sha256"],
        "all_started_sha256": all_started[
            "all_started_sha256"
        ],
        "roots": roots,
        "all_staged_sha256": "",
    }
    document["all_staged_sha256"] = verifier.document_hash(
        document, "all_staged_sha256"
    )
    if _immutable_artifact_exists(
        path, label="all-roots staged barrier"
    ):
        if runtime.load_json(path) != document:
            raise runtime.HarnessError(
                "v2 clean all-roots staged barrier is stale"
            )
    else:
        runtime.atomic_create_json(path, document)
    return document


def _validate_prepared_root_cleanup(
    prepared: Mapping[str, Any],
    stage: Mapping[str, Any],
    *,
    require_complete: bool,
) -> Dict[str, Any]:
    role = str(prepared["role"])
    record = prepared.get("record")
    task_dir = prepared.get("task_dir")
    intent = prepared.get("intent")
    manifest = prepared.get("manifest")
    ready = prepared.get("ready")
    root_stage = stage.get("root")
    if (
        not isinstance(record, Mapping)
        or not isinstance(task_dir, Path)
        or not isinstance(intent, Mapping)
        or not isinstance(manifest, Mapping)
        or not isinstance(ready, Mapping)
        or not isinstance(root_stage, Mapping)
    ):
        raise runtime.HarnessError(
            f"v2 clean {role} global validation evidence is malformed"
        )
    git_owner = prepared.get("git_owner")
    if git_owner is not None and not isinstance(git_owner, Path):
        raise runtime.HarnessError(
            f"v2 clean {role} prepared Git owner is malformed"
        )

    _validate_frozen_clean_tree(
        Path(str(prepared["quarantine"])),
        Path(str(prepared["staging"])),
        manifest,
        root_stage,
        original_path=Path(str(prepared["original"])),
        require_complete=require_complete,
    )
    admin_stage = stage.get("admin")
    if git_owner is not None:
        if not isinstance(admin_stage, Mapping):
            raise runtime.HarnessError(
                f"v2 clean {role} Git admin stage evidence is missing"
            )
        _validate_staged_git_admin(
            task_dir=task_dir,
            role=role,
            record=record,
            admin_stage=admin_stage,
            require_complete=require_complete,
        )
    elif admin_stage is not None:
        raise runtime.HarnessError(
            f"v2 clean {role} unexpected Git admin stage evidence"
        )

    pre_control = _archive_git_control(
        task_dir=task_dir,
        intent=intent,
        archive_role=f"{role}-pre",
        repo_root=Path(str(prepared["original"])),
        record=record,
        git_owner=git_owner,
        live=False,
    )
    final_control = _archive_git_control(
        task_dir=task_dir,
        intent=intent,
        archive_role=f"{role}-final",
        repo_root=Path(str(prepared["quarantine"])),
        record=record,
        git_owner=git_owner,
        live=False,
    )
    document = {
        "schema_version": 1,
        "role": role,
        "ready_sha256": ready["ready_sha256"],
        "manifest_sha256": manifest["manifest_sha256"],
        "stage_sha256": root_stage["stage_sha256"],
        "admin_stage_sha256": (
            admin_stage["admin_stage_sha256"]
            if isinstance(admin_stage, Mapping)
            else None
        ),
        "pre_control_sha256": pre_control["manifest_sha256"],
        "final_control_sha256": final_control["manifest_sha256"],
        "validation_sha256": "",
    }
    document["validation_sha256"] = verifier.document_hash(
        document, "validation_sha256"
    )
    return document


def _bind_all_roots_validated(
    *,
    task_dir: Path,
    intent: Mapping[str, Any],
    all_staged: Mapping[str, Any],
    validations: Sequence[Mapping[str, Any]],
) -> Dict[str, Any]:
    path = task_dir / "clean-delete-all-validated.json"
    document = {
        "schema_version": 1,
        "intent_sha256": intent["intent_sha256"],
        "all_staged_sha256": all_staged["all_staged_sha256"],
        "roots": [dict(validation) for validation in validations],
        "all_validated_sha256": "",
    }
    document["all_validated_sha256"] = verifier.document_hash(
        document, "all_validated_sha256"
    )
    if _immutable_artifact_exists(
        path, label="all-roots validated barrier"
    ):
        if runtime.load_json(path) != document:
            raise runtime.HarnessError(
                "v2 clean all-roots validated barrier is stale"
            )
    else:
        runtime.atomic_create_json(path, document)
    return document


def _finalize_frozen_root_cleanup(
    prepared: Mapping[str, Any],
    stage: Mapping[str, Any],
) -> None:
    role = str(prepared["role"])
    record = prepared["record"]
    if not isinstance(record, Mapping):
        raise runtime.HarnessError(
            f"v2 clean {role} prepared record is malformed"
        )
    _purge_frozen_clean_tree(
        Path(str(prepared["quarantine"])),
        Path(str(prepared["staging"])),
        prepared["manifest"],
        stage["root"],
        original_path=Path(str(prepared["original"])),
    )
    git_owner = prepared.get("git_owner")
    if git_owner is not None:
        if not isinstance(git_owner, Path):
            raise runtime.HarnessError(
                f"v2 clean {role} prepared Git owner is malformed"
            )
        admin_stage = stage.get("admin")
        if not isinstance(admin_stage, Mapping):
            raise runtime.HarnessError(
                f"v2 clean {role} Git admin stage evidence is missing"
            )
        _purge_staged_git_admin(
            task_dir=prepared["task_dir"],
            role=role,
            record=record,
            admin_stage=admin_stage,
        )
        admin_path = Path(str(record["git_admin_path"]))
        _prune_quarantined_worktree(
            git_owner,
            original=Path(str(prepared["original"])),
            quarantine=Path(str(prepared["quarantine"])),
            admin_path=admin_path,
        )


def _finalize_all_prepared_roots(
    prepared: Sequence[Mapping[str, Any]],
    stages: Sequence[Mapping[str, Any]],
) -> None:
    # Revalidate every recoverability archive and every staged root/admin as
    # one coordinated set immediately before the first irreversible unlink.
    # This closes deterministic late-mutation windows between publishing the
    # durable all-validated barrier and entering finalization. A malicious
    # same-UID process can still race any point-in-time filesystem check.
    if len(prepared) != len(stages) or any(
        str(root.get("role")) != str(stage.get("role"))
        for root, stage in zip(prepared, stages)
    ):
        raise runtime.HarnessError(
            "v2 clean coordinated finalization set is malformed"
        )
    for root, stage in zip(prepared, stages):
        _validate_prepared_root_cleanup(
            root,
            stage,
            require_complete=False,
        )
    for root, stage in zip(prepared, stages):
        _finalize_frozen_root_cleanup(root, stage)


def _execute_clean_intent(
    contract: Mapping[str, Any],
    task_dir: Path,
    intent: Mapping[str, Any],
) -> None:
    primary = Path(str(contract["repo_realpath"])).resolve()
    worktree = Path(str(contract["worktree_path"]))
    archive_ref = str(intent["archive_ref"])
    snapshot_sha = str(intent["snapshot_sha"])
    tree_sha = str(intent["tree_sha"])
    if runtime.git(
        primary, "rev-parse", "--verify", archive_ref, check=False
    ) != snapshot_sha:
        raise runtime.HarnessError("v2 clean archive ref is missing or moved")
    if runtime.git(primary, "rev-parse", f"{snapshot_sha}^{{tree}}") != tree_sha:
        raise runtime.HarnessError("v2 clean archive tree is stale")

    expected_snapshots = [
        {
            "path": str(path),
            "snapshot_ref": reference,
            "snapshot_commit": commit,
            "snapshot_head": head_revision,
        }
        for (
            path,
            reference,
            commit,
            head_revision,
        ) in _verification_snapshots_for_cleanup(
            contract, task_dir, validate_worktrees=False
        )
    ]
    intent_snapshots = intent.get("verification_snapshots")
    if not isinstance(intent_snapshots, list) or [
        {
            "path": entry.get("path"),
            "snapshot_ref": entry.get("snapshot_ref"),
            "snapshot_commit": entry.get("snapshot_commit"),
            "snapshot_head": entry.get("expected_revision"),
        }
        for entry in intent_snapshots
        if isinstance(entry, Mapping)
    ] != expected_snapshots:
        raise runtime.HarnessError(
            "v2 clean intent snapshot set differs from task evidence"
        )

    client_record = {
        "path": str(worktree),
        "device": intent["worktree_device"],
        "inode": intent["worktree_inode"],
        "quarantine_path": intent["quarantine_path"],
        "delete_staging_path": intent["delete_staging_path"],
        "expected_tree": tree_sha,
        "expected_revision": intent["worktree_revision"],
        "git_admin_path": intent["git_admin_path"],
        "git_admin_staging_path": intent[
            "git_admin_staging_path"
        ],
    }
    cleanup_jobs: List[Dict[str, Any]] = [
        {
            "role": "client",
            "record": client_record,
            "git_owner": primary,
            "ignored_policy": IGNORED_POLICY_FULL,
        }
    ]

    server_record = intent.get("server_cleanup")
    if isinstance(server_record, Mapping):
        server_mode = intent.get("server_mode")
        runtime_doc = runtime.load_json(task_dir / "runtime.json")
        for key, expected in (
            ("server_worktree", intent.get("server_worktree")),
            ("server_source", intent.get("server_source")),
            ("server_checkout_mode", server_mode),
            ("server_revision", server_record.get("expected_revision")),
        ):
            if runtime_doc.get(key) != expected:
                raise runtime.HarnessError(
                    f"v2 clean runtime {key} differs from durable intent"
                )
        server_owner = (
            Path(str(intent["server_source"]))
            if server_mode == "linked-worktree"
            else None
        )
        cleanup_jobs.append(
            {
                "role": "server",
                "record": server_record,
                "git_owner": server_owner,
                "ignored_policy": IGNORED_POLICY_NONE,
            }
        )

    for index, snapshot_record in enumerate(intent_snapshots):
        if not isinstance(snapshot_record, Mapping):
            raise runtime.HarnessError(
                "v2 clean snapshot record is malformed"
            )
        if snapshot_record.get("present") is False:
            snapshot_path = Path(str(snapshot_record["path"]))
            if snapshot_path.exists() or snapshot_path.is_symlink():
                raise runtime.HarnessError(
                    "v2 clean absent verification snapshot appeared after intent"
                )
            continue
        cleanup_jobs.append(
            {
                "role": f"snapshot-{index:04d}",
                "record": snapshot_record,
                "git_owner": None,
                "ignored_policy": IGNORED_POLICY_SNAPSHOT_HELPERS,
            }
        )

    # Refuse every known revision drift before moving any root. Each root is
    # then quarantined and frozen before deletion starts, so a failure on an
    # auxiliary checkout cannot destroy the client first.
    for job in cleanup_jobs:
        _prevalidate_frozen_root_cleanup(
            task_dir=task_dir,
            intent=intent,
            role=str(job["role"]),
            record=job["record"],
            git_owner=job["git_owner"],
        )
    prepared = [
        _prepare_frozen_root_cleanup(
            task_dir=task_dir,
            intent=intent,
            role=str(job["role"]),
            record=job["record"],
            git_owner=job["git_owner"],
            ignored_policy=str(job["ignored_policy"]),
        )
        for job in cleanup_jobs
    ]
    started = [
        _start_frozen_root_cleanup(root) for root in prepared
    ]
    all_started = _bind_all_roots_started(
        task_dir=task_dir,
        intent=intent,
        prepared=prepared,
        started=started,
    )
    all_staged_path = task_dir / "clean-delete-all-staged.json"
    if _immutable_artifact_exists(
        all_staged_path, label="all-roots staged barrier"
    ):
        stages = [
            _load_prepared_root_stage(root) for root in prepared
        ]
    else:
        stages = [
            _stage_prepared_root_cleanup(root)
            for root in prepared
        ]
    all_staged = _bind_all_roots_staged(
        task_dir=task_dir,
        intent=intent,
        all_started=all_started,
        stages=stages,
    )
    validated_path = task_dir / "clean-delete-all-validated.json"
    require_complete = not _immutable_artifact_exists(
        validated_path, label="all-roots validated barrier"
    )
    validations = [
        _validate_prepared_root_cleanup(
            root,
            stage,
            require_complete=require_complete,
        )
        for root, stage in zip(prepared, stages)
    ]
    _bind_all_roots_validated(
        task_dir=task_dir,
        intent=intent,
        all_staged=all_staged,
        validations=validations,
    )
    _finalize_all_prepared_roots(prepared, stages)

    runtime.delete_local_task_branch(
        primary,
        str(contract["branch"]),
        snapshot_sha,
        archive_ref,
    )
    for entry in expected_snapshots:
        reference = entry["snapshot_ref"]
        commit_sha = entry["snapshot_commit"]
        current = runtime.git(
            primary, "rev-parse", "--verify", reference, check=False
        )
        if current:
            if current != commit_sha:
                raise runtime.HarnessError(
                    "v2 verification snapshot ref moved before cleanup"
                )
            runtime.run_capture(
                ["git", "update-ref", "-d", reference, commit_sha],
                primary,
            )


def cmd_clean(args: argparse.Namespace) -> int:
    contract, task_dir, _ = load_v2_task(args.task_id, Path.cwd())
    # The operator must invoke clean from the driver/primary, not from a shell
    # whose cwd is inside a cleanup root; the pre-intent lsof gate rejects that
    # parent shell without mutation. Move this child outside every cleanup root
    # so its own inherited cwd never creates a false positive.
    os.chdir(Path(str(contract["repo_realpath"])).resolve())
    lock = acquire_v2_run_lock(task_dir, "clean")
    try:
        state = load_v2_state(task_dir)
        if state.get("status") in verifier.V2_TERMINAL_STATES:
            removed = runtime.prune_task_runtime(task_dir)
            runtime_note = (
                f"; pruned runtime: {', '.join(removed)}"
                if removed
                else ""
            )
            print(
                f"{contract['task_id']}: {state['status']} "
                f"(cleanup already complete{runtime_note})"
            )
            return 0
        worktree = Path(str(contract["worktree_path"]))
        intent = _load_clean_intent(contract, task_dir)
        if intent is None:
            if not worktree.is_dir() or worktree.is_symlink():
                raise runtime.HarnessError(
                    "v2 task lost its worktree before a durable clean intent"
                )
            ignored = _non_disposable_ignored_paths(worktree)
            if ignored:
                shown = ", ".join(ignored[:20])
                suffix = (
                    "" if len(ignored) <= 20 else f" (+{len(ignored) - 20} more)"
                )
                raise runtime.HarnessError(
                    "refusing clean: ignored task bytes are not archived: "
                    + shown
                    + suffix
                )
            _require_quarantine_idle(worktree)
            dirty_after_commit = bool(runtime.changed_paths(worktree))
            clean_close = (
                state.get("status") == "COMMITTED" and not dirty_after_commit
            )
            if clean_close:
                verify_v2_committed(
                    contract,
                    task_dir,
                    allow_test_adapter=bool(
                        getattr(args, "_allow_test_adapter", False)
                    ),
                )
            if not clean_close and not args.abandon:
                raise runtime.HarnessError(
                    "uncommitted/dirty v2 clean requires explicit --abandon"
                )
            server = _server_cleanup_preflight(contract, task_dir)
            verification_snapshots = _verification_snapshots_for_cleanup(
                contract, task_dir
            )
            primary = Path(str(contract["repo_realpath"])).resolve()
            archive_ref, snapshot, tree, worktree_revision = (
                _archive_all_visible_bytes(
                primary, worktree, contract, task_dir
                )
            )
            intent = _clean_intent_document(
                contract,
                worktree=worktree,
                final_status="CLOSED" if clean_close else "ABANDONED",
                previous_status=str(state.get("status")),
                archive_ref=archive_ref,
                snapshot_sha=snapshot,
                tree_sha=tree,
                worktree_revision=worktree_revision,
                server=server,
                verification_snapshots=verification_snapshots,
            )
            # The immutable intent is the recovery authority before any root is
            # quarantined. Its directory entry must survive power loss.
            runtime.atomic_create_json(task_dir / "clean-intent.json", intent)
            _checkpoint_event(
                task_dir,
                "clean-intent",
                final_status=intent["final_status"],
                archive_ref=archive_ref,
                snapshot_sha=snapshot,
                tree_sha=tree,
                intent_sha256=intent["intent_sha256"],
            )
        _execute_clean_intent(contract, task_dir, intent)
        removed = runtime.prune_task_runtime(task_dir)
        final = str(intent["final_status"])
        set_v2_state(
            task_dir,
            final,
            phase="clean",
            archive_ref=intent["archive_ref"],
            snapshot_sha=intent["snapshot_sha"],
            tree_sha=intent["tree_sha"],
            previous_status=intent["previous_status"],
            clean_intent_sha256=intent["intent_sha256"],
            runtime_removed=removed,
        )
    finally:
        release_v2_run_lock(lock)
    print(
        f"{contract['task_id']}: {load_v2_state(task_dir)['status']} "
        f"(snapshot preserved: {intent['archive_ref']})"
    )
    return 0


def _v2_task_for_worktree(cwd: Path) -> Tuple[Dict[str, Any], Path]:
    top = Path(runtime.git(cwd, "rev-parse", "--show-toplevel")).resolve()
    _, common = runtime.repo_context(cwd)
    root = v2_tasks(common)
    matches: List[Tuple[Dict[str, Any], Path]] = []
    malformed_claim = False
    if root.is_dir():
        for task_dir in sorted(root.iterdir()):
            manifest = task_dir / "task.json"
            if not manifest.is_file():
                continue
            try:
                contract = runtime.load_json(manifest)
                verifier.validate_hashed_document(
                    contract, "v2-task", "contract_sha256", "v2 task"
                )
            except Exception:
                # A manifest physically claiming this path may be malformed in
                # the very field needed for discovery. Fail closed whenever a
                # v2 task dir exists but cannot be validated.
                malformed_claim = True
                continue
            if Path(str(contract["worktree_path"])).resolve() == top:
                matches.append((contract, task_dir))
    if malformed_claim and not matches:
        raise runtime.HarnessError(
            "malformed v2 task claim exists; refusing no-task fallback"
        )
    if len(matches) != 1:
        raise runtime.HarnessError(
            f"expected exactly one v2 task for {top}, found {len(matches)}"
        )
    return matches[0]


def cmd_v2_guard(args: argparse.Namespace) -> int:
    if args.task_id:
        contract, task_dir, _ = load_v2_task(args.task_id, Path.cwd())
    else:
        contract, task_dir = _v2_task_for_worktree(Path.cwd())
    lock = acquire_v2_run_lock(task_dir, "guard-commit")
    try:
        evidence = cmd_v2_guard_commit(contract, task_dir)
    finally:
        release_v2_run_lock(lock)
    if not getattr(args, "quiet", False):
        print(
            f"agent-harness: v2 commit gate PASS for {contract['task_id']} "
            f"({evidence['diff_sha256'][:12]}…)"
        )
    return 0


def lock_liveness(task_dir: Path) -> Tuple[str, Optional[Dict[str, Any]]]:
    lock_path = task_dir / "run.lock"
    flags = os.O_RDWR
    if hasattr(os, "O_CLOEXEC"):
        flags |= os.O_CLOEXEC
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    try:
        descriptor = os.open(lock_path, flags)
    except FileNotFoundError:
        return "ABSENT", None
    except OSError as exc:
        raise runtime.HarnessError(
            f"refusing unsafe v2 task lock: {lock_path}"
        ) from exc
    handle = os.fdopen(descriptor, "r+b", buffering=0)
    try:
        opened = os.fstat(handle.fileno())
        if not stat.S_ISREG(opened.st_mode):
            raise runtime.HarnessError(
                f"v2 task lock is not a regular file: {lock_path}"
            )
        current = os.stat(lock_path, follow_symlinks=False)
        if (opened.st_dev, opened.st_ino) != (current.st_dev, current.st_ino):
            raise runtime.HarnessError(
                "v2 task lock inode changed during status inspection"
            )
        try:
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            handle.seek(0)
            try:
                owner = json.loads(handle.read().decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError, OSError):
                owner = None
            return "LIVE", owner if isinstance(owner, dict) else None
        finally:
            try:
                fcntl.flock(handle.fileno(), fcntl.LOCK_UN)
            except OSError:
                pass
        handle.seek(0)
        try:
            owner = json.loads(handle.read().decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError, OSError):
            owner = None
        return "STALE", owner if isinstance(owner, dict) else None
    finally:
        handle.close()


def v2_status(contract: Mapping[str, Any], task_dir: Path) -> Dict[str, Any]:
    state = load_v2_state(task_dir)
    liveness, owner = lock_liveness(task_dir)
    effective = state["status"]
    if state["status"] == "VERIFYING" and liveness != "LIVE":
        effective = "STALE"
    return {
        "generation": 2,
        "contract": contract,
        "state": state,
        "effective_status": effective,
        "lock": {"liveness": liveness, "owner": owner},
        "task_dir": str(task_dir),
    }


def cmd_status(args: argparse.Namespace) -> int:
    contract, task_dir, _ = load_v2_task(args.task_id, Path.cwd())
    result = v2_status(contract, task_dir)
    if args.json:
        print(json.dumps(result, indent=2, sort_keys=True))
    else:
        print(
            f"{contract['task_id']}: {result['effective_status']} "
            f"(stored {result['state']['status']}, lock {result['lock']['liveness']})"
        )
        print(f"worktree: {contract['worktree_path']}")
        if result["state"].get("reason"):
            print(f"reason: {result['state']['reason']}")
        if result["lock"]["owner"]:
            print("owner: " + json.dumps(result["lock"]["owner"], sort_keys=True))
    return 0


def _lifecycle_audit(primary: Path, common: Path) -> Dict[str, Any]:
    claimed_client: set = set()
    claimed_server: set = set()
    claimed_verification: set = set()
    ghosts: List[str] = []
    gc_debt: List[str] = []
    root = v2_tasks(common)
    if root.is_dir():
        for task_dir in sorted(path for path in root.iterdir() if path.is_dir()):
            manifest = task_dir / "task.json"
            if not manifest.is_file():
                ghosts.append(f"{task_dir.name}:missing-task")
                continue
            try:
                contract = runtime.load_json(manifest)
                worktree = Path(str(contract["worktree_path"])).resolve()
                claimed_client.add(worktree)
                runtime_path = task_dir / "runtime.json"
                if runtime_path.is_file():
                    runtime_record = runtime.load_json(runtime_path)
                    server_worktree = runtime_record.get("server_worktree")
                    if isinstance(server_worktree, str) and server_worktree:
                        claimed_server.add(Path(server_worktree).resolve())
                attempts = task_dir / "attempts"
                if attempts.is_dir():
                    for attempt in attempts.iterdir():
                        snapshot_manifest = attempt / "snapshot.json"
                        if not snapshot_manifest.is_file():
                            continue
                        snapshot_doc = runtime.load_json(snapshot_manifest)
                        snapshot_path = Path(
                            str(snapshot_doc.get("path", ""))
                        ).resolve()
                        if snapshot_path != _verification_snapshot_path(
                            contract, attempt
                        ):
                            raise runtime.HarnessError(
                                "snapshot path differs from attempt"
                            )
                        claimed_verification.add(snapshot_path)
            except Exception as exc:  # noqa: BLE001 - doctor reports malformed debt
                ghosts.append(
                    f"{task_dir.name}:malformed-task:{type(exc).__name__}"
                )
                continue
            try:
                event_state = _last_state_event(task_dir)
                if event_state is None:
                    raise runtime.HarnessError("missing event state")
                status = str(event_state.get("status"))
                projection = (
                    runtime.load_json(task_dir / "state.json")
                    if (task_dir / "state.json").is_file()
                    else None
                )
                if projection != event_state:
                    ghosts.append(f"{task_dir.name}:projection-gap")
            except Exception:
                ghosts.append(f"{task_dir.name}:malformed-state")
                status = "UNKNOWN"
            liveness, _owner = lock_liveness(task_dir)
            if status == "VERIFYING" and liveness != "LIVE":
                ghosts.append(f"{task_dir.name}:stale-verifying")
            if status in verifier.V2_TERMINAL_STATES and worktree.exists():
                gc_debt.append(f"{task_dir.name}:terminal-worktree")
            if status in verifier.V2_TERMINAL_STATES and any(
                path.exists()
                for path in claimed_verification
                if path.parent == worktree.parent
            ):
                gc_debt.append(
                    f"{task_dir.name}:terminal-verification-snapshot"
                )

    orphans: List[str] = []
    repositories = [(primary, claimed_client)]
    sibling_server = primary.parent / "murmur-server"
    if (sibling_server / ".git").exists():
        repositories.append((sibling_server, claimed_server))
    for repository, claimed in repositories:
        output = runtime.run_capture(
            ["git", "worktree", "list", "--porcelain"], repository
        ).stdout
        for line in output.splitlines():
            if not line.startswith("worktree "):
                continue
            path = Path(line.removeprefix("worktree ")).resolve()
            if ".murmur-agent-tasks" in path.parts and path not in claimed:
                orphans.append(str(path))
    return {
        "ghosts": sorted(set(ghosts)),
        "gc_debt": sorted(set(gc_debt)),
        "orphan_worktrees": sorted(set(orphans)),
        "verification_snapshots": sorted(
            str(path) for path in claimed_verification if path.exists()
        ),
    }


def cmd_doctor(args: argparse.Namespace) -> int:
    primary, common = runtime.repo_context(Path.cwd())
    checks: List[Dict[str, Any]] = []

    def record(name: str, ok: bool, detail: str, required: bool = True) -> None:
        checks.append(
            {"name": name, "ok": ok, "required": required, "detail": detail}
        )

    record("git-repository", True, f"{primary} ({common})")
    record("python", sys.version_info >= (3, 9), sys.version.split()[0])
    sandbox = Path("/usr/bin/sandbox-exec")
    record(
        "check-sandbox",
        sys.platform == "darwin" and sandbox.is_file() and os.access(sandbox, os.X_OK),
        f"{sys.platform}: {sandbox}",
    )
    try:
        config = runtime.load_config()
        record("config", True, str(runtime.CONFIG_PATH))
    except runtime.HarnessError as exc:
        config = {}
        record("config", False, str(exc))
    for name in (
        "v2-task",
        "v2-plan",
        "v2-review",
        "v2-evidence",
        "v2-commit-intent",
        "v2-commit",
    ):
        try:
            runtime.load_schema(name)
            checks.append(
                {
                    "name": f"schema:{name}",
                    "ok": True,
                    "required": True,
                    "detail": str(runtime.SCHEMAS_DIR / f"{name}.schema.json"),
                }
            )
        except runtime.HarnessError as exc:
            checks.append(
                {
                    "name": f"schema:{name}",
                    "ok": False,
                    "required": True,
                    "detail": str(exc),
                }
            )
    for name in (
        "combined-reviewer",
        "lock-security-reviewer",
        "egress-security-reviewer",
        "protocol-security-reviewer",
    ):
        path = runtime.PROMPTS_DIR / f"{name}.md"
        record(f"prompt:{name}", path.is_file(), str(path))
    wrapper = runtime.HARNESS_ROOT.parent.parent / "scripts" / "agent-harness"
    record(
        "wrapper:agent-harness",
        wrapper.is_file() and os.access(wrapper, os.X_OK),
        str(wrapper),
    )
    if runtime.has_murmur_server_path_dependency(primary):
        pin_path = primary / ".murmur-server-revision"
        pin = pin_path.read_text(encoding="utf-8").strip() if pin_path.is_file() else ""
        record(
            "dependency-pin",
            bool(runtime.SHA1_RE.fullmatch(pin)),
            f"{pin_path}: {pin or 'missing'}",
        )
        server_source = primary.parent / "murmur-server"
        server_has_pin = (
            server_source.is_dir()
            and bool(runtime.SHA1_RE.fullmatch(pin))
            and runtime.run_capture(
                ["git", "cat-file", "-e", f"{pin}^{{commit}}"],
                server_source,
                check=False,
            ).returncode
            == 0
        )
        record(
            "dependency-object",
            server_has_pin,
            f"{server_source}: {pin or 'missing'}",
        )
    configured_cli = config.get("cli", {}) if isinstance(config, dict) else {}
    available_vendors = 0
    for vendor in ("codex", "claude"):
        version = runtime.command_version(vendor)
        minimum = str(configured_cli.get(vendor, {}).get("minimum_version", "0.0.0"))
        ok = (
            version is not None
            and runtime.version_tuple(version) >= runtime.version_tuple(minimum)
        )
        available_vendors += int(ok)
        record(
            f"cli:{vendor}",
            ok,
            f"{version or 'missing'} (minimum {minimum})",
            required=False,
        )
    record("cli:any-real-adapter", available_vendors > 0, f"{available_vendors} available")
    lifecycle = _lifecycle_audit(primary, common)
    ok = (
        all(item.get("ok") for item in checks if item.get("required", True))
        and not lifecycle["ghosts"]
        and not lifecycle["orphan_worktrees"]
    )
    result = {"ok": ok, "checks": checks, "lifecycle": lifecycle}
    if args.json:
        print(json.dumps(result, indent=2, sort_keys=True))
    else:
        for item in checks:
            marker = "OK" if item.get("ok") else (
                "WARN" if not item.get("required", True) else "FAIL"
            )
            print(f"[{marker}] {item['name']}: {item['detail']}")
        for name, values in lifecycle.items():
            marker = "OK" if not values else ("WARN" if name == "gc_debt" else "FAIL")
            print(f"[{marker}] lifecycle:{name}: {', '.join(values) if values else 'none'}")
    return 0 if ok else 1


def cmd_selftest(args: argparse.Namespace) -> int:
    for script in (
        "v2_selftest.py",
        "v2_fault_selftest.py",
        "metrics_selftest.py",
    ):
        completed = subprocess.run(
            [sys.executable, str(Path(__file__).with_name(script))],
            cwd=str(Path.cwd()),
            check=False,
        )
        if completed.returncode != 0:
            return completed.returncode
    return 0


def cmd_metrics(args: argparse.Namespace) -> int:
    # Metrics is deliberately a lazy, read-only extension: verification and
    # commit commands do not import non-protocol telemetry code.
    import metrics as harness_metrics

    if args.limit < 1:
        raise runtime.HarnessError("metrics --limit must be positive")
    _, common = runtime.repo_context(Path.cwd())
    report = harness_metrics.collect_metrics(common, limit=args.limit)
    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        print(harness_metrics.render_text(report))
    return 0


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(prog="agent-harness", description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)
    open_parser = subparsers.add_parser(
        "open", help="create a verifier-only v2 task and isolated client worktree"
    )
    open_parser.add_argument("task_id")
    open_parser.add_argument(
        "--kind",
        choices=["bug", "feature", "refactor", "docs", "harness"],
        default="feature",
    )
    prompt = open_parser.add_mutually_exclusive_group(required=True)
    prompt.add_argument("--prompt")
    prompt.add_argument("--prompt-file")
    open_parser.add_argument("--owned", action="append", required=True)
    open_parser.add_argument(
        "--claim", action="append", choices=["runtime", "performance"], default=[]
    )
    open_parser.add_argument("--reviewer", choices=["codex", "claude"])
    open_parser.add_argument(
        "--allow-same-vendor-high-risk",
        action="store_true",
        help=(
            "bind an explicit exception that assigns sensitive specialist reviews "
            "to the selected reviewer vendor; default remains cross-vendor"
        ),
    )
    open_parser.add_argument("--base")
    open_parser.add_argument("--branch")
    open_parser.set_defaults(expected_change=True)
    open_parser.set_defaults(handler=cmd_open)

    plan_parser = subparsers.add_parser(
        "plan", help="derive checks and reviews from the exact current diff"
    )
    plan_parser.add_argument("task_id")
    plan_parser.set_defaults(handler=cmd_plan)

    status_parser = subparsers.add_parser("status", help="show task state")
    status_parser.add_argument("task_id")
    status_parser.add_argument("--json", action="store_true")
    status_parser.set_defaults(handler=cmd_status)

    commit_parser = subparsers.add_parser(
        "commit", help="commit the exact PASS receipt"
    )
    commit_parser.add_argument("task_id")
    commit_parser.add_argument("-m", "--message", required=True)
    commit_parser.set_defaults(handler=cmd_v2_commit)
    guard_parser = subparsers.add_parser(
        "guard-commit", help="fail closed unless the exact current v2 diff has PASS"
    )
    guard_parser.add_argument("task_id", nargs="?")
    guard_parser.set_defaults(handler=cmd_v2_guard)

    verify_parser = subparsers.add_parser(
        "verify", help="resume exact-diff checks/reviews and produce v2 evidence"
    )
    verify_parser.add_argument("task_id")
    verify_parser.set_defaults(handler=cmd_verify)
    resume_parser = subparsers.add_parser(
        "resume", help="resume only missing evidence for the current exact diff"
    )
    resume_parser.add_argument("task_id")
    resume_parser.set_defaults(handler=cmd_resume)
    clean_parser = subparsers.add_parser(
        "clean", help="archive every visible byte, then close or abandon a task"
    )
    clean_parser.add_argument("task_id")
    clean_parser.add_argument("--abandon", action="store_true")
    clean_parser.set_defaults(handler=cmd_clean)
    doctor_parser = subparsers.add_parser(
        "doctor", help="audit dependencies plus ghost and GC debt"
    )
    doctor_parser.add_argument("--json", action="store_true")
    doctor_parser.set_defaults(handler=cmd_doctor)
    metrics_parser = subparsers.add_parser(
        "metrics", help="roll up append-only operational telemetry"
    )
    metrics_parser.add_argument("--json", action="store_true")
    metrics_parser.add_argument("--limit", type=int, default=20)
    metrics_parser.set_defaults(handler=cmd_metrics)
    selftest_parser = subparsers.add_parser(
        "selftest", help="run deterministic lifecycle and fault tests"
    )
    selftest_parser.add_argument("--ci", action="store_true")
    selftest_parser.set_defaults(handler=cmd_selftest)
    return parser


def main(argv: Optional[Sequence[str]] = None) -> int:
    arguments = list(argv if argv is not None else sys.argv[1:])
    parser = build_parser()
    args = parser.parse_args(arguments)
    return int(args.handler(args))


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except runtime.HarnessError as exc:
        print(f"agent-harness: {exc}", file=sys.stderr)
        raise SystemExit(exc.exit_code)
