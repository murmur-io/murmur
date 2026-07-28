#!/usr/bin/env python3
"""Generation-aware dispatcher and lifecycle for Murmur Harness v2.

Legacy v1 commands continue to execute in ``task_runner.py``.  V2 is a separate,
verifier-only lifecycle: a developer edits the isolated worktree, then ``plan``
and ``verify`` bind checks/reviews to the exact diff.  There is no writer or
repair loop in this module.
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
import shutil
import signal
import stat
import subprocess
import sys
import time
from typing import Any, Dict, Iterable, List, Mapping, Optional, Sequence, Tuple

import task_runner as legacy
import verifier


V1_COMMANDS = {
    "init",
    "run",
    "seal-prepared",
    "verify-attestation",
    "reap",
    "gc",
    "eval",
    "close",
}
DUAL_COMMANDS = {"status", "commit", "guard-commit", "doctor", "selftest"}
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


def v1_task_dir(common: Path, task_id: str) -> Path:
    return common / "agent-harness" / "tasks" / task_id


def _single_write_jsonl(path: Path, document: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    payload = legacy.canonical_json(document) + b"\n"
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_APPEND, 0o600)
    try:
        written = os.write(descriptor, payload)
        if written != len(payload):
            raise legacy.HarnessError(f"short append to {path}")
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def set_v2_state(task_dir: Path, status: str, **details: Any) -> Dict[str, Any]:
    if status not in verifier.V2_STATES:
        raise legacy.HarnessError(f"invalid v2 state: {status}")
    prior: Dict[str, Any] = {}
    state_path = task_dir / "state.json"
    if (task_dir / "events.jsonl").is_file():
        prior = load_v2_state(task_dir)
    elif state_path.is_file():
        raise legacy.HarnessError(
            "v2 state projection exists without its authoritative event ledger"
        )
    if prior:
        if prior.get("status") in verifier.V2_TERMINAL_STATES:
            raise legacy.HarnessError(
                f"v2 task is terminal ({prior.get('status')}); state cannot change"
            )
    state = {
        "schema_version": 2,
        "task_id": task_dir.name,
        "status": status,
        "updated_at": legacy.utc_now(),
        **{
            key: value
            for key, value in prior.items()
            if key not in {"schema_version", "task_id", "status", "updated_at"}
        },
        **details,
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
    legacy.atomic_write_json(state_path, state)
    return state


def _last_state_event(task_dir: Path) -> Optional[Dict[str, Any]]:
    return verifier.last_state_event(task_dir)


def load_v2_state(task_dir: Path) -> Dict[str, Any]:
    return verifier.load_v2_state(task_dir)


def _valid_task_id(task_id: str) -> None:
    if not legacy.TASK_ID_RE.fullmatch(task_id):
        raise legacy.HarnessError(
            "task id must match [a-z0-9][a-z0-9._-]{1,63}"
        )


def load_v2_task(
    task_id: str, cwd: Path
) -> Tuple[Dict[str, Any], Path, Path]:
    _valid_task_id(task_id)
    primary, common = legacy.repo_context(cwd)
    task_dir = v2_task_dir(common, task_id)
    contract = legacy.load_json(task_dir / "task.json")
    contract_schema: Optional[Mapping[str, Any]] = None
    if (task_dir / "events.jsonl").is_file():
        state = load_v2_state(task_dir)
        receipt_path = task_dir / "commit.json"
        if state.get("status") in {"COMMITTED", "CLOSED"} and receipt_path.is_file():
            receipt = legacy.load_json(receipt_path)
            attested_commit = str(receipt.get("commit_sha", ""))
            if not legacy.SHA1_RE.fullmatch(attested_commit):
                raise legacy.HarnessError("v2 committed receipt commit is malformed")
            if receipt.get("task_id") != task_id:
                raise legacy.HarnessError(
                    "v2 committed receipt task differs from its task store"
                )
            if receipt.get("contract_sha256") != contract.get(
                "contract_sha256"
            ):
                raise legacy.HarnessError(
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
        raise legacy.HarnessError("v2 task belongs to another Git common directory")
    return contract, task_dir, common


def _legacy_contract(common: Path, task_id: str) -> Optional[Dict[str, Any]]:
    path = v1_task_dir(common, task_id) / "task.json"
    if not path.is_file():
        return None
    contract = legacy.load_json(path)
    legacy.validate_schema(contract, legacy.load_schema("task"), label="v1 task")
    if legacy.contract_hash(contract) != contract.get("contract_sha256"):
        raise legacy.HarnessError("v1 task contract hash is stale")
    return contract


def resolve_generation(task_id: str, cwd: Path) -> Tuple[int, Dict[str, Any], Path]:
    """Resolve an ID without ever falling back from a malformed v2 claim."""

    _valid_task_id(task_id)
    _, common = legacy.repo_context(cwd)
    v2_dir = v2_task_dir(common, task_id)
    legacy_contract = _legacy_contract(common, task_id)
    if v2_dir.exists():
        contract, task_dir, _ = load_v2_task(task_id, cwd)
        if legacy_contract is not None:
            supersedes = contract.get("supersedes")
            expected = {
                "generation": 1,
                "task_id": task_id,
                "contract_sha256": legacy_contract["contract_sha256"],
            }
            if supersedes != expected:
                raise legacy.HarnessError(
                    "v1/v2 task id collision is ambiguous; v2 has no exact supersedes binding"
                )
        return 2, contract, task_dir
    if legacy_contract is not None:
        return 1, legacy_contract, v1_task_dir(common, task_id)
    raise legacy.HarnessError(f"no v1 or v2 task exists: {task_id}")


def _fetch_base(cwd: Path, requested: Optional[str]) -> str:
    if requested:
        return legacy.git(
            cwd,
            "rev-parse",
            "--verify",
            "--end-of-options",
            f"{requested}^{{commit}}",
        )
    config = legacy.load_config()
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
        return legacy.git(
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
        return legacy.git(cwd, "rev-parse", "HEAD")


def _read_description(args: argparse.Namespace) -> str:
    prompt = getattr(args, "prompt", None)
    prompt_file = getattr(args, "prompt_file", None)
    if bool(prompt) == bool(prompt_file):
        raise legacy.HarnessError("provide exactly one of --prompt or --prompt-file")
    if prompt_file:
        path = Path(prompt_file)
        try:
            prompt = path.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as exc:
            raise legacy.HarnessError(f"cannot read prompt file {path}: {exc}") from exc
    result = str(prompt).strip()
    if not result:
        raise legacy.HarnessError("task prompt must not be empty")
    return result


def _protected_v2_paths(
    owned: Sequence[str], config: Optional[Mapping[str, Any]] = None
) -> List[str]:
    protected = [
        legacy.normalize_owned_path(path)
        for path in (config or legacy.load_config()).get("protected_paths", [])
    ]
    return sorted(
        path
        for path in owned
        if any(legacy.path_overlaps(path, guard) for guard in protected)
    )


def _link_node_modules(primary: Path, worktree: Path) -> Optional[str]:
    source = primary / "node_modules"
    target = worktree / "node_modules"
    if not source.is_dir() or source.is_symlink():
        return None
    ignored = legacy.run_capture(
        ["git", "check-ignore", "--quiet", "--no-index", "--", "node_modules/"],
        worktree,
        check=False,
    )
    if ignored.returncode != 0:
        raise legacy.HarnessError("node_modules is not ignored in the v2 worktree")
    target.symlink_to(source.resolve(), target_is_directory=True)
    return str(source.resolve())


def _local_branch_oid(primary: Path, branch: str) -> Optional[str]:
    branch_ref = f"refs/heads/{branch}"
    result = legacy.run_capture(
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
        raise legacy.HarnessError(
            f"cannot inspect local branch {branch}: {detail}"
        )
    oid = result.stdout.strip()
    if not legacy.SHA1_RE.fullmatch(oid):
        raise legacy.HarnessError(f"local branch {branch} has an invalid OID")
    return oid


def _create_open_branch(primary: Path, branch: str, base_sha: str) -> str:
    branch_ref = f"refs/heads/{branch}"
    result = legacy.run_capture(
        ["git", "update-ref", "--no-deref", branch_ref, base_sha, "0" * 40],
        primary,
        check=False,
    )
    if result.returncode != 0:
        detail = (result.stderr or result.stdout).strip()
        raise legacy.HarnessError(
            f"could not create task branch {branch}: {detail}"
        )
    return base_sha


def _delete_open_branch_if_unchanged(
    primary: Path, branch: str, expected_oid: str
) -> None:
    branch_ref = f"refs/heads/{branch}"
    legacy.run_capture(
        ["git", "update-ref", "--no-deref", "-d", branch_ref, expected_oid],
        primary,
        check=False,
    )


def _standalone_driver_urls(driver: Path, *, push: bool) -> List[str]:
    argv = ["git", "remote", "get-url"]
    if push:
        argv.append("--push")
    argv.extend(["--all", "origin"])
    result = legacy.run_capture(argv, driver, check=False)
    urls = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    if result.returncode != 0 or not urls:
        raise legacy.HarnessError(
            "standalone driver origin must be canonical GitHub "
            "murmur-io/murmur HTTPS or SSH; local/file origins are forbidden"
        )
    return urls


def _standalone_driver_context(cwd: Path) -> Tuple[Path, Path]:
    top = Path(
        legacy.git(
            cwd,
            "rev-parse",
            "--path-format=absolute",
            "--show-toplevel",
        )
    ).resolve()
    primary, common = legacy.repo_context(cwd)
    if top.name != STANDALONE_DRIVER_NAME:
        raise legacy.HarnessError(
            "Harness v2 open requires the dedicated "
            f"{STANDALONE_DRIVER_NAME} standalone clone"
        )
    if top != primary:
        raise legacy.HarnessError(
            "standalone driver must be the primary worktree of its own "
            "Git common directory; linked driver worktrees are forbidden"
        )

    expected_common = top / ".git"
    git_dir = Path(
        legacy.git(
            cwd,
            "rev-parse",
            "--path-format=absolute",
            "--git-dir",
        )
    ).resolve()
    try:
        common_metadata = expected_common.lstat()
    except OSError as exc:
        raise legacy.HarnessError(
            "standalone driver Git common directory is missing"
        ) from exc
    if (
        stat.S_ISLNK(common_metadata.st_mode)
        or not stat.S_ISDIR(common_metadata.st_mode)
        or common != expected_common
        or git_dir != expected_common
    ):
        raise legacy.HarnessError(
            "standalone driver Git common directory must be exactly "
            f"{expected_common}"
        )

    symbolic_head = legacy.run_capture(
        ["git", "symbolic-ref", "--quiet", "HEAD"],
        top,
        check=False,
    )
    if symbolic_head.returncode == 0:
        raise legacy.HarnessError("standalone driver HEAD must be detached")
    if symbolic_head.returncode != 1:
        detail = (symbolic_head.stderr or symbolic_head.stdout).strip()
        raise legacy.HarnessError(
            f"cannot prove standalone driver HEAD is detached: {detail}"
        )
    legacy.git(top, "rev-parse", "--verify", "HEAD^{commit}")

    if legacy.git_bytes(
        top,
        "status",
        "--porcelain=v1",
        "--untracked-files=all",
    ).strip():
        raise legacy.HarnessError("standalone driver must be clean before open")

    alternates = expected_common / "objects" / "info" / "alternates"
    try:
        alternates_metadata = alternates.lstat()
    except FileNotFoundError:
        alternates_metadata = None
    except OSError as exc:
        raise legacy.HarnessError(
            "cannot inspect standalone driver objects/info/alternates"
        ) from exc
    if alternates_metadata is not None:
        if (
            stat.S_ISLNK(alternates_metadata.st_mode)
            or not stat.S_ISREG(alternates_metadata.st_mode)
        ):
            raise legacy.HarnessError(
                "standalone driver objects/info/alternates must be absent "
                "or empty"
            )
        try:
            nonempty_alternates = bool(alternates.read_bytes())
        except OSError as exc:
            raise legacy.HarnessError(
                "cannot inspect standalone driver objects/info/alternates"
            ) from exc
        if nonempty_alternates:
            raise legacy.HarnessError(
                "standalone driver objects/info/alternates must be absent "
                "or empty"
            )

    origin_urls = _standalone_driver_urls(top, push=False)
    push_urls = _standalone_driver_urls(top, push=True)
    if any(
        url not in CANONICAL_MURMUR_ORIGINS
        for url in [*origin_urls, *push_urls]
    ):
        raise legacy.HarnessError(
            "standalone driver origin must be canonical GitHub "
            "murmur-io/murmur HTTPS or SSH; local/file origins are forbidden"
        )
    return top, common


def _require_safe_new_task_root(driver: Path, task_root: Path) -> None:
    task_parent = driver.parent / ".murmur-agent-tasks"
    expected = task_parent / "v2" / task_root.name
    if task_root != expected:
        raise legacy.HarnessError("v2 task root escaped its dedicated parent")

    for component in (driver.parent, task_parent, task_parent / "v2"):
        try:
            metadata = component.lstat()
        except FileNotFoundError:
            continue
        except OSError as exc:
            raise legacy.HarnessError(
                f"cannot inspect v2 task root component: {component}"
            ) from exc
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
            raise legacy.HarnessError(
                f"v2 task root component is unsafe or symlinked: {component}"
            )

    try:
        root_metadata = task_root.lstat()
    except FileNotFoundError:
        return
    except OSError as exc:
        raise legacy.HarnessError(
            f"cannot inspect v2 task root: {task_root}"
        ) from exc
    if stat.S_ISLNK(root_metadata.st_mode) or not stat.S_ISDIR(
        root_metadata.st_mode
    ):
        raise legacy.HarnessError(
            f"v2 task root is unsafe or symlinked: {task_root}"
        )
    raise legacy.HarnessError(f"v2 task root already exists: {task_root}")


def cmd_open(args: argparse.Namespace) -> int:
    cwd = Path.cwd()
    primary, common = _standalone_driver_context(cwd)
    _valid_task_id(args.task_id)
    task_dir = v2_task_dir(common, args.task_id)
    task_root = primary.parent / ".murmur-agent-tasks" / "v2" / args.task_id
    _require_safe_new_task_root(primary, task_root)
    if task_dir.exists() or v1_task_dir(common, args.task_id).exists():
        raise legacy.HarnessError(
            f"task id already exists in a harness generation: {args.task_id}"
        )
    description = _read_description(args)
    owned = sorted(
        set(legacy.normalize_owned_path(path) for path in args.owned)
    )
    protected = _protected_v2_paths(owned)
    if protected:
        raise legacy.HarnessError(
            "Harness v2 is not allowed to certify its own protected control plane "
            f"({', '.join(protected)}); use a v1 --kind harness task with "
            "seal-prepared until an externally anchored v2 bootstrap replaces it"
        )
    claims = sorted(set(args.claim or []))
    reviewer = args.reviewer or str(legacy.load_config()["default_reviewer"])
    if reviewer not in legacy.REAL_MODEL_VENDORS:
        raise legacy.HarnessError("v2 reviewer must be codex or claude")
    if "runtime" in claims:
        legacy.runtime_preflight(primary)
    base_sha = _fetch_base(cwd, args.base)
    if not legacy.SHA1_RE.fullmatch(base_sha):
        raise legacy.HarnessError("v2 base did not resolve to a commit")
    branch = args.branch or f"agent/v2/{args.task_id}"
    if branch in {"murmur", "main", "master"}:
        raise legacy.HarnessError("v2 task cannot use a protected branch")
    if (
        legacy.run_capture(
            ["git", "check-ref-format", "--branch", branch], cwd, check=False
        ).returncode
        != 0
    ):
        raise legacy.HarnessError(f"invalid task branch: {branch}")
    if _local_branch_oid(primary, branch) is not None:
        raise legacy.HarnessError(f"branch already exists: {branch}")
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
        "expected_change": bool(args.expected_change),
        "degraded_provenance": [],
        "created_at": legacy.utc_now(),
    }
    branch_created = False
    branch_expected_oid: Optional[str] = None
    task_dir_created = False
    task_root_created = False
    try:
        task_root.mkdir(parents=True)
        task_root_created = True
        task_dir.mkdir(parents=True)
        task_dir_created = True
        legacy.run_capture(["git", "worktree", "prune"], primary)
        branch_expected_oid = _create_open_branch(primary, branch, base_sha)
        branch_created = True
        legacy.run_capture(
            ["git", "worktree", "add", str(worktree), branch],
            primary,
        )
        unsafe = [
            path
            for path in owned
            if legacy.path_has_symlink_component(worktree, path)
        ]
        if unsafe:
            raise legacy.HarnessError(
                "owned paths traverse symlinks: " + ", ".join(unsafe)
            )
        shared_node_modules = _link_node_modules(primary, worktree)
        contract["contract_sha256"] = verifier.document_hash(
            contract, "contract_sha256"
        )
        legacy.validate_schema(
            contract, legacy.load_schema("v2-task"), label="v2 task"
        )
        legacy.atomic_write_json(task_dir / "task.json", contract)
        legacy.atomic_write_json(
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
        set_v2_state(task_dir, "OPEN", phase="open")
    except Exception:
        if server_worktree.exists():
            server_source = primary.parent / "murmur-server"
            if server_source.is_dir():
                legacy.run_capture(
                    ["git", "worktree", "remove", "--force", str(server_worktree)],
                    server_source,
                    check=False,
                )
        if worktree.exists():
            legacy.run_capture(
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
    print(
        json.dumps(
            {
                "task_id": args.task_id,
                "generation": 2,
                "status": "OPEN",
                "base_sha": base_sha,
                "worktree": str(worktree),
                "server_worktree": None,
            },
            indent=2,
        )
    )
    return 0


def prepare_plan(
    contract: Mapping[str, Any], task_dir: Path
) -> Tuple[Dict[str, Any], Path, bytes]:
    worktree = Path(str(contract["worktree_path"]))
    if "runtime" in contract.get("claims", []):
        legacy.runtime_preflight(worktree)
    state = load_v2_state(task_dir)
    if state.get("status") in verifier.V2_TERMINAL_STATES | {"COMMITTED"}:
        raise legacy.HarnessError(
            f"cannot plan v2 task in state {state.get('status')}"
        )
    paths, diff, tree_sha = verifier.snapshot_scoped_diff(
        worktree, contract, task_dir
    )
    if not paths or not diff:
        raise legacy.HarnessError(
            "Harness v2 verifies changes only; the exact diff is empty"
        )
    plan, bundle = verifier.build_plan(
        contract, worktree, paths, diff, tree_sha, legacy.load_config()
    )
    current_id = verifier.attempt_id(plan)
    attempt_dir = task_dir / "attempts" / current_id
    attempt_dir.mkdir(parents=True, exist_ok=True)
    plan_path = attempt_dir / "plan.json"
    if plan_path.is_file():
        existing = legacy.load_json(plan_path)
        verifier.validate_hashed_document(
            existing, "v2-plan", "plan_sha256", "v2 plan"
        )
        if existing != plan:
            raise legacy.HarnessError(
                "attempt-key collision: existing plan differs from the exact profile"
            )
    else:
        legacy.atomic_write_json(plan_path, plan)
        legacy.atomic_write_json(attempt_dir / "protocol.json", bundle)
        legacy.atomic_write_bytes(attempt_dir / "diff.patch", diff)
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
        raise legacy.HarnessError("v2 attempt directory layout is malformed")
    if re.fullmatch(r"[0-9a-f]{64}", attempt_dir.name) is None:
        raise legacy.HarnessError("v2 attempt id is malformed")
    return worktree.parent / f"verify-{attempt_dir.name}"


def _verification_snapshot_ref(task_id: str, attempt_id: str) -> str:
    safe = re.sub(r"[^a-zA-Z0-9._-]+", "-", task_id).strip(".-")
    safe = safe.replace("..", "-") or "task"
    suffix = legacy.sha256_bytes(task_id.encode("utf-8"))[:12]
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
    current = legacy.git(
        primary, "rev-parse", "--verify", reference, check=False
    )
    if current:
        commit_sha = current
    else:
        identity = legacy.load_config()["commit_identity"]
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
            raise legacy.HarnessError(
                "could not anchor v2 verification tree: "
                + completed.stderr.strip()
            )
        commit_sha = completed.stdout.strip()
        if not legacy.SHA1_RE.fullmatch(commit_sha):
            raise legacy.HarnessError(
                "v2 verification anchor did not produce a commit"
            )
        # Create-only publication. A concurrent publisher may only win with
        # the same deterministic commit; otherwise update-ref fails closed.
        legacy.run_capture(
            ["git", "update-ref", reference, commit_sha, "0" * 40],
            primary,
        )
    if legacy.git(primary, "rev-parse", f"{commit_sha}^{{tree}}") != plan["tree_sha"]:
        raise legacy.HarnessError("v2 verification anchor tree is stale")
    parents = legacy.git(
        primary, "show", "-s", "--format=%P", commit_sha
    ).split()
    if parents != [plan["base_sha"]]:
        raise legacy.HarnessError("v2 verification anchor parent is stale")
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
        raise legacy.HarnessError("v2 verification snapshot is missing or unsafe")
    if snapshot.resolve() != _verification_snapshot_path(contract, attempt_dir):
        raise legacy.HarnessError("v2 verification snapshot escaped its task root")
    common = Path(
        legacy.git(
            snapshot,
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
        )
    ).resolve()
    if common != (snapshot / ".git").resolve():
        raise legacy.HarnessError(
            "v2 verification snapshot unexpectedly shares mutable Git metadata"
        )
    alternates = common / "objects" / "info" / "alternates"
    try:
        alternates_metadata = alternates.lstat()
    except FileNotFoundError:
        alternates_metadata = None
    except OSError as exc:
        raise legacy.HarnessError(
            "cannot inspect v2 verification snapshot object alternates"
        ) from exc
    if alternates_metadata is not None:
        if (
            stat.S_ISLNK(alternates_metadata.st_mode)
            or not stat.S_ISREG(alternates_metadata.st_mode)
        ):
            raise legacy.HarnessError(
                "v2 verification snapshot object alternates must be absent "
                "or empty"
            )
        try:
            nonempty_alternates = bool(alternates.read_bytes())
        except OSError as exc:
            raise legacy.HarnessError(
                "cannot inspect v2 verification snapshot object alternates"
            ) from exc
        if nonempty_alternates:
            raise legacy.HarnessError(
                "v2 verification snapshot object alternates must be absent "
                "or empty"
            )
    if (
        Path(legacy.git(snapshot, "rev-parse", "--show-toplevel")).resolve()
        != snapshot.resolve()
    ):
        raise legacy.HarnessError("v2 verification snapshot is not its Git root")
    if legacy.git(snapshot, "rev-parse", "HEAD") != plan["base_sha"]:
        raise legacy.HarnessError("v2 verification snapshot parent changed")
    paths, diff, tree_sha = verifier.snapshot_scoped_diff(
        snapshot, contract, task_dir
    )
    if paths != plan["changed_paths"]:
        raise legacy.HarnessError("v2 verification snapshot paths changed")
    if legacy.sha256_bytes(diff) != plan["diff_sha256"]:
        raise legacy.HarnessError("v2 verification snapshot diff changed")
    if tree_sha != plan["tree_sha"]:
        raise legacy.HarnessError("v2 verification snapshot tree changed")


def _discard_claimed_verification_snapshot(
    contract: Mapping[str, Any], attempt_dir: Path, snapshot: Path
) -> None:
    if snapshot.resolve() != _verification_snapshot_path(contract, attempt_dir):
        raise legacy.HarnessError(
            "refusing to discard a verification snapshot outside its task root"
        )
    if not snapshot.exists() and not snapshot.is_symlink():
        return
    if not snapshot.is_dir() or snapshot.is_symlink():
        raise legacy.HarnessError(
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
        manifest = legacy.load_json(manifest_path)
        if manifest != expected_manifest:
            raise legacy.HarnessError(
                "v2 verification snapshot manifest differs from the exact plan"
            )
        try:
            _validate_verification_snapshot(
                contract, task_dir, plan, attempt_dir, snapshot
            )
            return snapshot
        except legacy.HarnessError:
            # The durable manifest was published before clone creation. A
            # parent crash may leave a partial clone; discard only that exact
            # runner-claimed path and reconstruct from the anchored tree.
            _discard_claimed_verification_snapshot(
                contract, attempt_dir, snapshot
            )
    else:
        if snapshot.exists() or snapshot.is_symlink():
            raise legacy.HarnessError(
                "unclaimed v2 verification snapshot path already exists"
            )
        legacy.atomic_write_json(manifest_path, expected_manifest)
    primary = Path(str(contract["repo_realpath"])).resolve()
    if not primary.is_dir() or primary.is_symlink():
        raise legacy.HarnessError("v2 primary repository is missing or unsafe")
    try:
        local_snapshot_ref = "refs/agent-harness/v2/source"
        legacy.run_capture(
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
        legacy.run_capture(
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
        fetched_commit = legacy.git(
            snapshot,
            "rev-parse",
            "--verify",
            "--end-of-options",
            f"{local_snapshot_ref}^{{commit}}",
        )
        if fetched_commit != snapshot_commit:
            raise legacy.HarnessError(
                "v2 verification snapshot fetched a stale anchor"
            )
        if (
            legacy.git(
                snapshot, "rev-parse", f"{fetched_commit}^{{tree}}"
            )
            != plan["tree_sha"]
        ):
            raise legacy.HarnessError(
                "v2 verification snapshot fetched a stale tree"
            )
        fetched_parents = legacy.git(
            snapshot, "show", "-s", "--format=%P", fetched_commit
        ).split()
        if fetched_parents != [plan["base_sha"]]:
            raise legacy.HarnessError(
                "v2 verification snapshot fetched a stale parent"
            )
        legacy.run_capture(
            ["git", "checkout", "--quiet", "--detach", plan["base_sha"]],
            snapshot,
        )
        legacy.run_capture(
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
) -> legacy.TaskRunLock:
    """Use the one hardened create-only lock protocol for both generations.

    ``command`` remains part of the call-site API so diagnostics can name the
    attempted operation without inventing a second, weaker on-disk protocol.
    """

    del command
    return legacy.acquire_run_lock(task_dir)


def release_v2_run_lock(lock: legacy.TaskRunLock) -> None:
    legacy.release_run_lock(lock)


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
    runtime = legacy.load_json(runtime_path)
    server_worktree = worktree.parent / "murmur-server"
    server_source = Path(str(runtime.get("server_source", "")))
    expected_source = Path(str(contract["repo_realpath"])).parent / "murmur-server"
    if server_source.resolve() != expected_source.resolve():
        raise legacy.HarnessError("v2 server source is not the canonical sibling repository")
    revision_path = worktree / ".murmur-server-revision"
    try:
        revision = revision_path.read_text(encoding="utf-8").strip()
    except (OSError, UnicodeError) as exc:
        raise legacy.HarnessError("v2 Rust/protocol check needs .murmur-server-revision") from exc
    if not legacy.SHA1_RE.fullmatch(revision):
        raise legacy.HarnessError(".murmur-server-revision is malformed")
    if not server_source.is_dir():
        raise legacy.HarnessError(f"pinned sibling server repository is missing: {server_source}")
    resolved = legacy.git(
        server_source,
        "rev-parse",
        "--verify",
        "--end-of-options",
        f"{revision}^{{commit}}",
    )
    if resolved != revision:
        raise legacy.HarnessError("pinned server revision did not resolve exactly")
    if server_worktree.exists():
        if not server_worktree.is_dir() or server_worktree.is_symlink():
            raise legacy.HarnessError("v2 server worktree path is unsafe")
        if legacy.git_bytes(server_worktree, "status", "--porcelain").strip():
            raise legacy.HarnessError("existing v2 server worktree is dirty")
        checkout_mode = str(
            runtime.get("server_checkout_mode") or "linked-worktree"
        )
        if legacy.git(server_worktree, "rev-parse", "HEAD") != revision:
            if checkout_mode != "local-shared-clone":
                raise legacy.HarnessError(
                    "existing linked v2 server worktree is at another revision"
                )
            common = Path(
                legacy.git(
                    server_worktree,
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-common-dir",
                )
            ).resolve()
            if common != (server_worktree / ".git").resolve():
                raise legacy.HarnessError(
                    "existing v2 server clone shares mutable Git metadata"
                )
            shutil.rmtree(server_worktree)
    if not server_worktree.exists():
        # Do not register a linked worktree in the sibling repository. Agent
        # sandboxes correctly deny that cross-workspace .git mutation. A local
        # shared clone writes only inside the task root while reusing objects.
        legacy.run_capture(
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
        legacy.run_capture(
            ["git", "checkout", "--quiet", "--detach", revision],
            server_worktree,
        )
        checkout_mode = "local-shared-clone"
    runtime.update(
        {
            "server_worktree": str(server_worktree),
            "server_source": str(server_source.resolve()),
            "server_revision": revision,
            "server_checkout_mode": checkout_mode,
        }
    )
    legacy.atomic_write_json(runtime_path, runtime)
    return server_worktree


def _checkpoint_event(
    task_dir: Path, event: str, **details: Any
) -> None:
    _single_write_jsonl(
        task_dir / "events.jsonl",
        {"at": legacy.utc_now(), "event": event, **details},
    )


def _load_bound_record(
    path: Path, plan: Mapping[str, Any]
) -> Optional[Dict[str, Any]]:
    if not path.is_file():
        return None
    try:
        record = legacy.load_json(path)
    except (legacy.HarnessError, OSError, UnicodeError):
        return None
    if not verifier.binding_matches(record, plan):
        return None
    return record


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
        and legacy.sha256_bytes(diff) == plan["diff_sha256"]
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
        except legacy.HarnessError:
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
    evidence = legacy.run_check(
        verification_worktree,
        task_dir,
        declared,
        f"v2-{attempt_dir.name[:12]}",
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
    legacy.atomic_write_json(record_path, record)
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
            raise legacy.HarnessError(
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
        for path in sorted(probes_dir.glob("*.json")):
            record = _load_bound_record(path, plan)
            if record is None:
                continue
            try:
                declared_probe = verifier.canonical_check(
                    str(record.get("id")), legacy.load_config()
                )
                verifier.validate_check_checkpoint(
                    record, declared_probe, plan, task_dir
                )
            except legacy.HarnessError:
                continue
            prior = record.get("evidence", {})
            if prior.get("outcome") == "BLOCKED" or prior.get("timed_out"):
                continue
            probe_records.append(record)
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
                    [*check_records, *probe_records],
                    str(declared["kind"]),
                    worktree,
                )
                try:
                    verifier.validate_review_checkpoint(
                        record,
                        declared,
                        plan,
                        task_dir,
                        expected_prompt_sha256=legacy.sha256_bytes(
                            prompt.encode("utf-8")
                        ),
                        allow_test_adapter=allow_test_adapter,
                    )
                except legacy.HarnessError:
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
                    int(legacy.load_config().get("v2_max_parallel_reviews", 3)),
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
                    attempt_dir=run_dir,
                    diff=diff,
                    checks=[*check_records, *probe_records],
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
                    legacy.atomic_write_json(record_path, record)
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
        )
        existing_probe_ids = {
            str(record.get("id")) for record in probe_records
        }
        passed_checks = {
            str(record.get("id")): record
            for record in check_records
            if record.get("evidence", {}).get("passed")
        }
        aliased_probe_ids = [
            probe_id
            for probe_id in requested_probe_ids
            if probe_id not in existing_probe_ids and probe_id in passed_checks
        ]
        for probe_id in aliased_probe_ids:
            record_path = probes_dir / f"{probe_id}.json"
            alias = {
                **passed_checks[probe_id],
                "source": "planned-check",
                "created_at": legacy.utc_now(),
            }
            legacy.atomic_write_json(record_path, alias)
            _checkpoint_event(
                task_dir,
                "probe-alias-checkpoint",
                attempt_id=attempt_dir.name,
                probe_id=probe_id,
                record_path=str(record_path),
                source="planned-check",
            )
        missing_probe_ids = [
            probe_id
            for probe_id in requested_probe_ids
            if probe_id not in existing_probe_ids
            and probe_id not in passed_checks
        ]
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
            config = legacy.load_config()
            probe_pause = False
            probe_failure = False
            for probe_id in missing_probe_ids:
                declared = verifier.canonical_check(probe_id, config)
                record_path = probes_dir / f"{probe_id}.json"
                evidence = legacy.run_check(
                    worktree,
                    task_dir,
                    declared,
                    f"v2-{attempt_dir.name[:12]}-probe",
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
                record = verifier.check_record(declared, plan, evidence)
                legacy.atomic_write_json(record_path, record)
                _checkpoint_event(
                    task_dir,
                    "probe-checkpoint",
                    attempt_id=attempt_dir.name,
                    probe_id=probe_id,
                    record_path=str(record_path),
                    passed=bool(evidence.get("passed")),
                )
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
                    "allowlisted probe evidence collected; resume will run fresh "
                    "reviews against it"
                ),
                attempt_id=attempt_dir.name,
            )
            return "NEEDS_EVIDENCE"
        if aliased_probe_ids:
            set_v2_state(
                task_dir,
                "NEEDS_EVIDENCE",
                phase="probes",
                reason=(
                    "reviewer-requested proof was already a planned green "
                    "check; a bound alias was recorded and resume will run a "
                    "fresh review without repeating the command"
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
        legacy.atomic_write_json(evidence_path, evidence)
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
    except (KeyboardInterrupt, legacy.HarnessCancellation):
        current = load_v2_state(task_dir)
        if current.get("status") not in verifier.V2_TERMINAL_STATES:
            set_v2_state(
                task_dir,
                "INTERRUPTED",
                phase=current.get("phase", "verify"),
                reason="verifier interrupted; resume preserves completed checkpoints",
            )
        raise
    except legacy.HarnessError as exc:
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


def _legacy_degraded_provenance(task_dir: Path) -> List[Dict[str, Any]]:
    values: List[Dict[str, Any]] = []
    provenance_known = False
    attestation_rounds = 0
    attestation_path = task_dir / "attestation.json"
    if attestation_path.is_file():
        attestation = legacy.load_json(attestation_path)
        provenance_known = attestation.get("provenance_schema_version") == 2
        attestation_rounds = int(attestation.get("rounds", 0) or 0)
        for item in attestation.get("degraded_provenance", []):
            if isinstance(item, dict):
                values.append({"source": "attestation", **item})
        writer = attestation.get("writer", {})
        if isinstance(writer, dict) and (
            writer.get("degraded") or writer.get("timed_out")
        ):
            values.append(
                {
                    "source": "attestation-writer",
                    "round": writer.get("round"),
                    "label": writer.get("label"),
                    "degraded": writer.get("degraded"),
                    "timed_out": bool(writer.get("timed_out")),
                }
            )
    model_exit_events = 0
    events_path = task_dir / "events.jsonl"
    if events_path.is_file():
        with events_path.open("r", encoding="utf-8", errors="replace") as handle:
            for line in handle:
                try:
                    event = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if not isinstance(event, dict):
                    continue
                if event.get("event") == "model-process-exit":
                    model_exit_events += 1
                if event.get("event") in {
                    "writer-timed-out-recovered",
                    "writer-report-degraded",
                } or (
                    event.get("event") == "model-process-exit"
                    and event.get("role") == "writer"
                    and event.get("timed_out")
                ):
                    values.append(
                        {
                            "source": "event",
                            "event": event.get("event"),
                            "label": event.get("label"),
                            "degraded": (
                                "timeout"
                                if event.get("timed_out")
                                or event.get("event") == "writer-timed-out-recovered"
                                else "unparseable-report"
                            ),
                            "timed_out": bool(event.get("timed_out")),
                            "at": event.get("at"),
                        }
                    )
    state_missing = not (task_dir / "state.json").is_file()
    raw_model_logs = len(
        [
            path
            for path in (task_dir / "logs").glob("*.jsonl")
            if path.is_file() and not path.is_symlink()
        ]
    ) if (task_dir / "logs").is_dir() else 0
    if (
        state_missing
        or (not provenance_known and attestation_rounds > 1)
        or raw_model_logs > model_exit_events
        or (raw_model_logs > 0 and not provenance_known)
    ):
        values.append(
            {
                "source": "legacy-import",
                "degraded": "legacy-provenance-unknown",
                "state_missing": state_missing,
                "attestation_rounds": attestation_rounds,
                "raw_model_logs": raw_model_logs,
                "model_exit_events": model_exit_events,
            }
        )
    unique: Dict[bytes, Dict[str, Any]] = {}
    for item in values:
        unique[legacy.canonical_json(item)] = item
    return [unique[key] for key in sorted(unique)]


def _directory_digest(root: Path) -> str:
    digest = hashlib.sha256()
    if not root.is_dir() or root.is_symlink():
        raise legacy.HarnessError(f"unsafe legacy task directory: {root}")
    for path in sorted(root.rglob("*"), key=lambda item: item.as_posix()):
        relative = path.relative_to(root).as_posix()
        # The import holds this ephemeral liveness lock for the whole
        # byte-preserving adoption. Its PID/timestamp are not v1 evidence and
        # would otherwise make every later idempotency digest differ.
        if relative == "run.lock":
            continue
        metadata = path.lstat()
        digest.update(relative.encode("utf-8", "surrogateescape"))
        digest.update(b"\x00")
        digest.update(str(stat.S_IFMT(metadata.st_mode)).encode("ascii"))
        digest.update(b"\x00")
        if path.is_symlink():
            digest.update(os.readlink(path).encode("utf-8", "surrogateescape"))
        elif path.is_file():
            with path.open("rb") as handle:
                for chunk in iter(lambda: handle.read(1024 * 1024), b""):
                    digest.update(chunk)
    return digest.hexdigest()


def _legacy_state_for_import(
    source_dir: Path,
) -> Tuple[Dict[str, Any], Optional[str]]:
    state_path = source_dir / "state.json"
    if state_path.is_file():
        return legacy.load_json(state_path), legacy.sha256_file(state_path)
    status = "UNKNOWN"
    last_event: Dict[str, Any] = {}
    events_path = source_dir / "events.jsonl"
    if events_path.is_file():
        with events_path.open("r", encoding="utf-8", errors="strict") as handle:
            for line in handle:
                event = json.loads(line)
                if (
                    isinstance(event, dict)
                    and event.get("event") == "state"
                    and isinstance(event.get("status"), str)
                ):
                    last_event = event
                    status = event["status"]
    return {
        "task_id": source_dir.name,
        "status": status,
        "round": last_event.get("round", 0),
        "phase": last_event.get("phase", "ghost-import"),
        "reason": "v1 state.json is missing; status recovered from append-only events",
        "ghost": True,
    }, None


def _private_visible_tree(worktree: Path, runtime_dir: Path) -> str:
    runtime_dir.mkdir(parents=True, exist_ok=True)
    import tempfile

    descriptor, raw_index = tempfile.mkstemp(
        prefix="v1-import-tree-", dir=str(runtime_dir)
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
        raise legacy.HarnessError(
            f"could not reconstruct v1 visible tree: {detail}"
        ) from exc
    finally:
        index.unlink(missing_ok=True)


def _restore_v1_worktree(
    primary: Path,
    source: Mapping[str, Any],
    source_dir: Path,
    target_runtime: Path,
) -> bool:
    worktree = Path(str(source["worktree_path"])).resolve()
    if worktree.exists():
        return worktree.is_dir() and not worktree.is_symlink()
    archive: Dict[str, Any] = {}
    if (source_dir / "archive.json").is_file():
        archive = legacy.load_json(source_dir / "archive.json")
    candidates = [
        str(archive.get("snapshot_sha", "")),
        legacy.git(
            primary,
            "show-ref",
            "--verify",
            "--hash",
            f"refs/heads/{source['branch']}",
            check=False,
        ),
        legacy.git(
            primary,
            "rev-parse",
            "--verify",
            legacy.task_archive_ref(source),
            check=False,
        ),
    ]
    snapshot = next(
        (
            candidate
            for candidate in candidates
            if isinstance(candidate, str)
            and legacy.SHA1_RE.fullmatch(candidate)
            and legacy.run_capture(
                ["git", "cat-file", "-e", f"{candidate}^{{commit}}"],
                primary,
                check=False,
            ).returncode
            == 0
        ),
        None,
    )
    if snapshot is None:
        return False
    snapshot_tree = legacy.git(primary, "rev-parse", f"{snapshot}^{{tree}}")
    recorded_tree = archive.get("tree_sha")
    if recorded_tree and recorded_tree != snapshot_tree:
        raise legacy.HarnessError(
            "v1 archive snapshot tree differs from its recorded tree"
        )
    base = str(source["base_sha"])
    if (
        legacy.run_capture(
            ["git", "cat-file", "-e", f"{base}^{{commit}}"],
            primary,
            check=False,
        ).returncode
        != 0
    ):
        raise legacy.HarnessError("v1 base commit is unavailable for reconstruction")
    archive_ref = legacy.task_archive_ref(source)
    legacy.git(primary, "update-ref", archive_ref, snapshot)
    worktree.parent.mkdir(parents=True, exist_ok=True)
    legacy._prune_worktree_registrations(primary)
    legacy.run_capture(
        [
            "git",
            "worktree",
            "add",
            "-B",
            str(source["branch"]),
            str(worktree),
            base,
        ],
        primary,
    )
    try:
        legacy.git(worktree, "read-tree", "--reset", "-u", snapshot)
        legacy.git(worktree, "reset", "--quiet", "HEAD", "--", ".")
        if legacy.git(worktree, "rev-parse", "HEAD") != base:
            raise legacy.HarnessError("reconstructed v1 worktree is not at task base")
        if _private_visible_tree(worktree, target_runtime) != snapshot_tree:
            raise legacy.HarnessError(
                "reconstructed v1 worktree bytes differ from archived tree"
            )
    except Exception:
        # The archive ref remains the lossless recovery point.  Do not silently
        # adopt a partial reconstruction.
        raise
    return True


def cmd_import_v1(args: argparse.Namespace) -> int:
    _primary, common = legacy.repo_context(Path.cwd())
    _valid_task_id(args.task_id)
    source_dir = v1_task_dir(common, args.task_id)
    if not (source_dir / "task.json").is_file():
        raise legacy.HarnessError(f"v1 task does not exist: {args.task_id}")
    lock = legacy.acquire_run_lock(source_dir)
    try:
        return _cmd_import_v1_locked(args)
    finally:
        legacy.release_run_lock(lock)


def _cmd_import_v1_locked(args: argparse.Namespace) -> int:
    cwd = Path.cwd()
    primary, common = legacy.repo_context(cwd)
    _valid_task_id(args.task_id)
    source_dir = v1_task_dir(common, args.task_id)
    source_path = source_dir / "task.json"
    if not source_path.is_file():
        raise legacy.HarnessError(f"v1 task does not exist: {args.task_id}")
    source = legacy.load_json(source_path)
    legacy.validate_schema(source, legacy.load_schema("task"), label="v1 task")
    source_hash = legacy.contract_hash(source)
    if source.get("contract_sha256") != source_hash:
        raise legacy.HarnessError("cannot import a stale v1 task contract")
    if not source.get("expected_change", True):
        raise legacy.HarnessError(
            "Harness v2 does not import no-change tasks; retain the v1 record"
        )
    protected = _protected_v2_paths(list(source.get("owned_paths", [])))
    if protected:
        raise legacy.HarnessError(
            "protected v1 control-plane work cannot be imported into the "
            "self-verifying v2 candidate; finish it through v1 seal-prepared "
            f"({', '.join(protected)})"
        )
    source_bytes_before = _directory_digest(source_dir)
    target_dir = v2_task_dir(common, args.task_id)
    expected_supersedes = {
        "generation": 1,
        "task_id": args.task_id,
        "contract_sha256": source_hash,
    }
    if target_dir.exists():
        existing, _, _ = load_v2_task(args.task_id, cwd)
        if existing.get("supersedes") != expected_supersedes:
            raise legacy.HarnessError("v2 import collision does not supersede this exact v1 contract")
        imported = legacy.load_json(target_dir / "imports" / "v1.json")
        if imported.get("source_directory_sha256") != source_bytes_before:
            raise legacy.HarnessError(
                "v1 source artifacts changed after import; idempotent adoption refused"
            )
        print(
            json.dumps(
                {
                    "task_id": args.task_id,
                    "generation": 2,
                    "status": load_v2_state(target_dir)["status"],
                    "idempotent": True,
                    "source_unchanged": True,
                },
                indent=2,
            )
        )
        return 0
    source_state, source_state_sha256 = _legacy_state_for_import(source_dir)
    if source_state.get("status") in {"PASSED", "COMMITTED"} and not args.invalidate_pass:
        raise legacy.HarnessError(
            "PASSED/COMMITTED v1 tasks should finish in v1; pass --invalidate-pass to resume in v2"
        )
    worktree = Path(str(source["worktree_path"]))
    worktree_present = worktree.is_dir() and not worktree.is_symlink()
    if not worktree_present:
        import_runtime = v2_store(common) / "runtime" / f"import-{args.task_id}"
        try:
            worktree_present = _restore_v1_worktree(
                primary,
                source,
                source_dir,
                import_runtime,
            )
        finally:
            shutil.rmtree(import_runtime, ignore_errors=True)
    if worktree_present:
        if Path(legacy.git(worktree, "rev-parse", "--show-toplevel")).resolve() != worktree.resolve():
            raise legacy.HarnessError("v1 worktree path is not its Git root")
        actual_base = legacy.git(worktree, "rev-parse", "HEAD")
        actual_branch = legacy.git(worktree, "branch", "--show-current")
        if actual_branch != source["branch"]:
            raise legacy.HarnessError("v1 worktree branch changed before import")
        if actual_base != source["base_sha"]:
            raise legacy.HarnessError(
                "v1 worktree HEAD moved beyond its contracted base; refusing "
                "to hide committed bytes as a new v2 base. Preserve/archive the "
                "branch, restore the original base with its visible tree, then import."
            )
    else:
        actual_base = source["base_sha"]
        actual_branch = source["branch"]
    reviewer = args.reviewer or source.get("reviewer")
    if reviewer not in legacy.REAL_MODEL_VENDORS and not (
        reviewer == "fake" and os.environ.get("MURMUR_HARNESS_SELFTEST") == "1"
    ):
        raise legacy.HarnessError("imported v2 reviewer must be codex or claude")
    contract: Dict[str, Any] = {
        "schema_version": 2,
        "task_id": args.task_id,
        "description": str(source["description"]),
        "kind": "import",
        "base_sha": actual_base,
        "contract_sha256": "",
        "repo_realpath": str(primary.resolve()),
        "git_common_dir": str(common.resolve()),
        "worktree_path": str(worktree.resolve()),
        "branch": actual_branch,
        "owned_paths": list(source["owned_paths"]),
        "claims": sorted(set(args.claim or [])),
        "reviewer": reviewer,
        "expected_change": bool(source["expected_change"]),
        "degraded_provenance": _legacy_degraded_provenance(source_dir),
        "supersedes": expected_supersedes,
        "created_at": legacy.utc_now(),
    }
    contract["contract_sha256"] = verifier.document_hash(
        contract, "contract_sha256"
    )
    legacy.validate_schema(
        contract, legacy.load_schema("v2-task"), label="imported v2 task"
    )
    target_dir.mkdir(parents=True)
    try:
        legacy.atomic_write_json(target_dir / "task.json", contract)
        source_runtime = (
            legacy.load_json(source_dir / "runtime.json")
            if (source_dir / "runtime.json").is_file()
            else {}
        )
        legacy.atomic_write_json(
            target_dir / "runtime.json",
            {
                "schema_version": 2,
                "task_root": str(worktree.parent),
                "shared_node_modules": source_runtime.get("shared_node_modules"),
                "server_worktree": source_runtime.get("server_worktree"),
                "server_source": source_runtime.get(
                    "server_source", str(primary.parent / "murmur-server")
                ),
                "server_revision": source.get("dependency_revisions", {}).get(
                    "murmur-server.expected"
                ),
                # Missing means an imported legacy linked worktree.
                "server_checkout_mode": source_runtime.get(
                    "server_checkout_mode"
                ),
            },
        )
        legacy.atomic_write_json(
            target_dir / "imports" / "v1.json",
            {
                "schema_version": 2,
                "task_id": args.task_id,
                "source_contract_sha256": source_hash,
                "source_task_path": str(source_path),
                "source_state_sha256": source_state_sha256,
                "source_events_sha256": (
                    legacy.sha256_file(source_dir / "events.jsonl")
                    if (source_dir / "events.jsonl").is_file()
                    else None
                ),
                "source_directory_sha256": source_bytes_before,
                "source_worktree_present": worktree_present,
                "imported_at": legacy.utc_now(),
            },
        )
        set_v2_state(
            target_dir,
            "OPEN" if worktree_present else "NEEDS_EVIDENCE",
            phase="import",
            reason=(
                "v1 worktree adopted without re-running a writer"
                if worktree_present
                else "v1 worktree is missing; import is history-only and cannot PASS"
            ),
        )
        if _directory_digest(source_dir) != source_bytes_before:
            raise legacy.HarnessError(
                "v1 source artifacts changed during import; refusing adoption"
            )
    except Exception:
        shutil.rmtree(target_dir, ignore_errors=True)
        raise
    print(
        json.dumps(
            {
                "task_id": args.task_id,
                "generation": 2,
                "status": load_v2_state(target_dir)["status"],
                "worktree_present": worktree_present,
                "source_unchanged": True,
                "degraded_attempts": len(contract["degraded_provenance"]),
            },
            indent=2,
        )
    )
    return 0


def _stage_v2_commit(
    contract: Mapping[str, Any], task_dir: Path, evidence: Mapping[str, Any]
) -> None:
    worktree = Path(str(contract["worktree_path"]))
    # This is the one intentional real-index mutation in v2.  It happens only
    # after exact evidence is verified, immediately before commit.
    legacy.git(worktree, "reset", "--quiet", "HEAD", "--", ".")
    if evidence["changed_paths"]:
        legacy.git(worktree, "add", "-A", "--", *contract["owned_paths"])
    diff = legacy.staged_diff(worktree)
    if legacy.sha256_bytes(diff) != evidence["diff_sha256"]:
        raise legacy.HarnessError("real staged index differs from v2 evidence")
    if legacy.git(worktree, "write-tree") != evidence["tree_sha"]:
        raise legacy.HarnessError("real staged tree differs from v2 evidence")


def _v2_commit_message(message: str, evidence: Mapping[str, Any]) -> str:
    subject = message.strip()
    if not subject or "\x00" in subject:
        raise legacy.HarnessError("commit message must be non-empty and contain no NUL")
    if re.search(
        r"(?im)^\s*(?:Harness-(?:Version|Task|Verdict|Base|Diff-Sha256|"
        r"Evidence-Sha256|Attestation-Sha256)|Co-Authored-By):",
        subject,
    ):
        raise legacy.HarnessError(
            "commit message must not contain receipt or co-author trailers"
        )
    degraded = evidence.get("degraded_provenance", [])
    trailers = [
        "Harness-Version: 2",
        f"Harness-Task: {evidence['task_id']}",
        "Harness-Verdict: PASS",
        f"Harness-Base: {evidence['parent_sha']}",
        f"Harness-Diff-Sha256: {evidence['diff_sha256']}",
        f"Harness-Evidence-Sha256: {evidence['evidence_sha256']}",
        # Transitional alias so an older presence-only gate sees a receipt;
        # strict v2 verification requires the alias to equal Evidence.
        f"Harness-Attestation-Sha256: {evidence['evidence_sha256']}",
    ]
    if degraded:
        labels = sorted(
            {
                str(item.get("degraded") or item.get("event") or "degraded")
                for item in degraded
                if isinstance(item, Mapping)
            }
        )
        trailers.insert(3, "Harness-Writer-Degraded: " + ",".join(labels))
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
        "Harness-Writer-Degraded",
    }
    values: Dict[str, str] = {}
    for line in message.splitlines():
        match = re.fullmatch(r"(Harness-[A-Za-z0-9-]+): ([^\r\n]+)", line)
        if match is None:
            if re.match(r"(?i)^\s*Harness-", line):
                raise legacy.HarnessError(
                    "v2 commit contains a malformed receipt trailer"
                )
            continue
        key, value = match.groups()
        if key not in keys:
            raise legacy.HarnessError(
                f"v2 commit contains an unknown receipt trailer: {key}"
            )
        if key in values:
            raise legacy.HarnessError(
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
    degraded = evidence.get("degraded_provenance", [])
    if degraded:
        labels = sorted(
            {
                str(item.get("degraded") or item.get("event") or "degraded")
                for item in degraded
                if isinstance(item, Mapping)
            }
        )
        expected["Harness-Writer-Degraded"] = ",".join(labels)
    if values != expected:
        raise legacy.HarnessError(
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
        "message_sha256": legacy.sha256_bytes(message.encode("utf-8")),
        # Intent is deterministically bound to the immutable PASS evidence so
        # retrying before/after git commit reproduces the exact same artifact.
        "created_at": evidence["created_at"],
        "intent_sha256": "",
    }
    intent["intent_sha256"] = verifier.document_hash(intent, "intent_sha256")
    path = task_dir / "commit-intent.json"
    if path.is_file():
        existing = legacy.load_json(path)
        verifier.validate_hashed_document(
            existing,
            "v2-commit-intent",
            "intent_sha256",
            "v2 commit intent",
        )
        if existing != intent:
            raise legacy.HarnessError(
                "existing v2 commit intent differs; resume with the exact original message"
            )
        return existing
    legacy.validate_schema(
        intent,
        legacy.load_schema("v2-commit-intent"),
        label="v2 commit intent",
    )
    legacy.atomic_write_json(path, intent)
    return intent


def _validate_v2_commit_head(
    worktree: Path,
    evidence: Mapping[str, Any],
    intent: Mapping[str, Any],
    expected_identity: Mapping[str, str],
) -> Dict[str, Any]:
    commit_sha = legacy.git(worktree, "rev-parse", "HEAD")
    parent_sha = legacy.git(worktree, "rev-parse", "HEAD^")
    tree_sha = legacy.git(worktree, "rev-parse", "HEAD^{tree}")
    actual_diff = legacy.git_bytes(
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
        "name": legacy.git(worktree, "log", "-1", "--format=%an"),
        "email": legacy.git(worktree, "log", "-1", "--format=%ae"),
    }
    committer = {
        "name": legacy.git(worktree, "log", "-1", "--format=%cn"),
        "email": legacy.git(worktree, "log", "-1", "--format=%ce"),
    }
    actual_message = legacy.git(
        worktree, "log", "-1", "--format=%B"
    ).rstrip("\n")
    if parent_sha != evidence["parent_sha"] or parent_sha != intent["parent_sha"]:
        raise legacy.HarnessError("v2 commit actual parent differs from evidence/intent")
    if tree_sha != evidence["tree_sha"] or tree_sha != intent["tree_sha"]:
        raise legacy.HarnessError("v2 commit tree differs from evidence/intent")
    diff_sha = legacy.sha256_bytes(actual_diff)
    if diff_sha != evidence["diff_sha256"] or diff_sha != intent["diff_sha256"]:
        raise legacy.HarnessError("v2 commit diff differs from evidence/intent")
    if actual_message != intent["message"]:
        raise legacy.HarnessError("v2 commit message differs from the durable intent")
    if legacy.sha256_bytes(actual_message.encode("utf-8")) != intent["message_sha256"]:
        raise legacy.HarnessError("v2 commit message hash differs from the durable intent")
    if author != expected_identity or committer != expected_identity:
        raise legacy.HarnessError("v2 commit author/committer is not QueaT")
    if legacy.git_bytes(worktree, "status", "--porcelain").strip():
        raise legacy.HarnessError("v2 committed worktree/index is not clean")
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
        raise legacy.HarnessError("only a PASSED v2 task can be committed")
    worktree = Path(str(contract["worktree_path"]))
    identity = legacy.load_config().get("commit_identity", {})
    name = identity.get("name") if isinstance(identity, Mapping) else None
    email = identity.get("email") if isinstance(identity, Mapping) else None
    if (name, email) != ("QueaT", "kgm004a@gmail.com"):
        raise legacy.HarnessError("v2 commit identity contract changed")
    expected_identity = {"name": name, "email": email}
    current_head = legacy.git(worktree, "rev-parse", "HEAD")
    if current_head == contract["base_sha"]:
        evidence = cmd_v2_guard_commit(
            contract,
            task_dir,
            allow_test_adapter=allow_test_adapter,
        )
        message = _v2_commit_message(args.message, evidence)
        intent = _commit_intent(contract, task_dir, evidence, message)
        legacy.run_capture(
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
        intent = legacy.load_json(task_dir / "commit-intent.json")
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
            raise legacy.HarnessError(
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
        "authored_at": legacy.git(worktree, "log", "-1", "--format=%aI"),
        "committed_at": legacy.git(worktree, "log", "-1", "--format=%cI"),
        "recorded_at": legacy.utc_now(),
    }
    legacy.validate_schema(
        receipt, legacy.load_schema("v2-commit"), label="v2 commit receipt"
    )
    legacy.atomic_write_json(task_dir / "commit.json", receipt)
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
    suffix = legacy.sha256_bytes(task_id.encode("utf-8"))[:12]
    return f"refs/agent-harness/v2/archive/{safe}-{suffix}"


def _archive_all_visible_bytes(
    primary: Path,
    worktree: Path,
    contract: Mapping[str, Any],
    task_dir: Path,
) -> Tuple[str, str, str]:
    """Archive HEAD plus every tracked/untracked byte via a private index."""

    runtime = task_dir / "runtime"
    runtime.mkdir(parents=True, exist_ok=True)
    import tempfile

    descriptor, raw_index = tempfile.mkstemp(prefix="v2-clean-index-", dir=str(runtime))
    os.close(descriptor)
    index_path = Path(raw_index)
    environment = {**os.environ, "GIT_INDEX_FILE": str(index_path)}
    head = legacy.git(worktree, "rev-parse", "HEAD")
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
    except subprocess.CalledProcessError as exc:
        detail = (
            exc.stderr.decode("utf-8", "replace")
            if isinstance(exc.stderr, bytes)
            else str(exc.stderr)
        )
        raise legacy.HarnessError(
            "could not archive every Git-visible v2 byte: " + detail
        ) from exc
    finally:
        index_path.unlink(missing_ok=True)
    if tree == legacy.git(worktree, "rev-parse", "HEAD^{tree}"):
        snapshot = head
    else:
        identity = legacy.load_config()["commit_identity"]
        snapshot = legacy.git(
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
    legacy.git(primary, "update-ref", archive_ref, snapshot)
    if legacy.git(primary, "rev-parse", archive_ref) != snapshot:
        raise legacy.HarnessError("v2 archive ref verification failed")
    if legacy.git(primary, "rev-parse", f"{snapshot}^{{tree}}") != tree:
        raise legacy.HarnessError("v2 archive tree verification failed")
    legacy.atomic_write_json(
        task_dir / "archive.json",
        {
            "schema_version": 2,
            "task_id": contract["task_id"],
            "archive_ref": archive_ref,
            "snapshot_sha": snapshot,
            "original_head_sha": head,
            "tree_sha": tree,
            "created_at": legacy.utc_now(),
        },
    )
    return archive_ref, snapshot, tree


def _non_disposable_ignored_paths(worktree: Path) -> List[str]:
    raw = legacy.git_bytes(
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
    if legacy.managed_node_modules_link(worktree):
        values = [
            path
            for path in values
            if path != "node_modules" and not path.startswith("node_modules/")
        ]
    return values


def _clean_intent_document(
    contract: Mapping[str, Any],
    *,
    final_status: str,
    previous_status: str,
    archive_ref: str,
    snapshot_sha: str,
    tree_sha: str,
    server: Tuple[Optional[Path], Optional[Path], Optional[str]],
    verification_snapshots: Sequence[Tuple[Path, str, str]],
) -> Dict[str, Any]:
    server_worktree, server_source, server_mode = server
    document: Dict[str, Any] = {
        "schema_version": 2,
        "task_id": contract["task_id"],
        "contract_sha256": contract["contract_sha256"],
        "final_status": final_status,
        "previous_status": previous_status,
        "archive_ref": archive_ref,
        "snapshot_sha": snapshot_sha,
        "tree_sha": tree_sha,
        "worktree_path": contract["worktree_path"],
        "branch": contract["branch"],
        "server_worktree": (
            str(server_worktree) if server_worktree is not None else None
        ),
        "server_source": str(server_source) if server_source is not None else None,
        "server_mode": server_mode,
        "verification_snapshots": [
            {
                "path": str(path),
                "snapshot_ref": reference,
                "snapshot_commit": commit,
            }
            for path, reference, commit in verification_snapshots
        ],
        "created_at": legacy.utc_now(),
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
    document = legacy.load_json(path)
    if document.get("intent_sha256") != verifier.document_hash(
        document, "intent_sha256"
    ):
        raise legacy.HarnessError("v2 clean intent hash mismatch")
    for key, expected in (
        ("schema_version", 2),
        ("task_id", contract["task_id"]),
        ("contract_sha256", contract["contract_sha256"]),
        ("worktree_path", contract["worktree_path"]),
        ("branch", contract["branch"]),
    ):
        if document.get(key) != expected:
            raise legacy.HarnessError(f"v2 clean intent {key} is stale")
    if document.get("final_status") not in {"CLOSED", "ABANDONED"}:
        raise legacy.HarnessError("v2 clean intent final status is malformed")
    return document


def _server_cleanup_preflight(
    contract: Mapping[str, Any], task_dir: Path
) -> Tuple[Optional[Path], Optional[Path], Optional[str]]:
    runtime = legacy.load_json(task_dir / "runtime.json")
    raw_worktree = runtime.get("server_worktree")
    raw_source = runtime.get("server_source")
    if not raw_worktree:
        return None, None, None
    server_worktree = Path(str(raw_worktree))
    server_source = Path(str(raw_source))
    expected = Path(str(contract["worktree_path"])).parent / "murmur-server"
    if server_worktree.resolve() != expected.resolve():
        raise legacy.HarnessError("recorded v2 server worktree escapes the task root")
    if not server_worktree.is_dir() or server_worktree.is_symlink():
        raise legacy.HarnessError("recorded v2 server worktree is missing or unsafe")
    if legacy.git_bytes(server_worktree, "status", "--porcelain").strip():
        raise legacy.HarnessError(
            "refusing clean: pinned server worktree is dirty; nothing was removed"
        )
    mode = str(runtime.get("server_checkout_mode") or "linked-worktree")
    if mode not in {"linked-worktree", "local-shared-clone"}:
        raise legacy.HarnessError(
            f"recorded v2 server checkout mode is unsupported: {mode}"
        )
    if mode == "local-shared-clone":
        common = Path(
            legacy.git(
                server_worktree,
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir",
            )
        ).resolve()
        if common != (server_worktree / ".git").resolve():
            raise legacy.HarnessError(
                "local v2 server clone unexpectedly shares mutable Git metadata"
            )
    return server_worktree, server_source, mode


def _verification_snapshots_for_cleanup(
    contract: Mapping[str, Any], task_dir: Path
) -> List[Tuple[Path, str, str]]:
    values: List[Tuple[Path, str, str]] = []
    attempts = task_dir / "attempts"
    if not attempts.is_dir():
        return values
    primary = Path(str(contract["repo_realpath"])).resolve()
    for attempt_dir in sorted(attempts.iterdir()):
        manifest_path = attempt_dir / "snapshot.json"
        if not manifest_path.is_file():
            continue
        manifest = legacy.load_json(manifest_path)
        if (
            manifest.get("snapshot_sha256")
            != verifier.document_hash(manifest, "snapshot_sha256")
        ):
            raise legacy.HarnessError(
                f"v2 snapshot manifest is corrupt: {manifest_path}"
            )
        expected_path = _verification_snapshot_path(contract, attempt_dir)
        snapshot = Path(str(manifest.get("path", "")))
        reference = str(manifest.get("snapshot_ref", ""))
        commit_sha = str(manifest.get("snapshot_commit", ""))
        if snapshot.resolve() != expected_path:
            raise legacy.HarnessError("v2 cleanup snapshot path is stale")
        if reference != _verification_snapshot_ref(
            str(contract["task_id"]), attempt_dir.name
        ):
            raise legacy.HarnessError("v2 cleanup snapshot ref is stale")
        if not legacy.SHA1_RE.fullmatch(commit_sha):
            raise legacy.HarnessError("v2 cleanup snapshot commit is malformed")
        current_ref = legacy.git(
            primary, "rev-parse", "--verify", reference, check=False
        )
        if current_ref and current_ref != commit_sha:
            raise legacy.HarnessError("v2 cleanup snapshot ref moved")
        if snapshot.exists() or snapshot.is_symlink():
            if not snapshot.is_dir() or snapshot.is_symlink():
                raise legacy.HarnessError(
                    "v2 cleanup snapshot path is not a safe directory"
                )
            common = Path(
                legacy.git(
                    snapshot,
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-common-dir",
                )
            ).resolve()
            if common != (snapshot / ".git").resolve():
                raise legacy.HarnessError(
                    "v2 cleanup snapshot shares mutable Git metadata"
                )
        values.append((snapshot, reference, commit_sha))
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
        legacy.run_capture(
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
        raise legacy.HarnessError(
            "v2 task commit is not an ancestor of the current branch tip; "
            "rebases and replacement commits require fresh verification"
        )
    base_tip = legacy.git(
        worktree,
        "rev-parse",
        "--verify",
        "--end-of-options",
        f"{default_base}^{{commit}}",
        check=False,
    )
    if not base_tip:
        raise legacy.HarnessError(
            f"cannot validate v2 catch-up merges: fetch {default_base} first"
        )

    merges: List[str] = []
    cursor = current_head
    seen: set[str] = set()
    while cursor != attested_commit:
        if cursor in seen:
            raise legacy.HarnessError("cycle detected in v2 first-parent history")
        seen.add(cursor)
        raw = legacy.git(worktree, "show", "-s", "--format=%P", cursor)
        parents = raw.split()
        if len(parents) != 2:
            raise legacy.HarnessError(
                "v2 branch contains a non-merge commit after its attested task "
                f"commit ({cursor[:12]}); re-verify branch-authored content"
            )
        first_parent, side_parent = parents
        if (
            legacy.run_capture(
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
            raise legacy.HarnessError(
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
            raise legacy.HarnessError(
                f"v2 catch-up merge {cursor[:12]} had conflicts or manual "
                "resolution; verify the resulting diff as a new task"
            )
        output = completed.stdout.strip().splitlines()
        expected_tree = output[0] if output else ""
        actual_tree = legacy.git(worktree, "rev-parse", f"{cursor}^{{tree}}")
        if not expected_tree or expected_tree != actual_tree:
            raise legacy.HarnessError(
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
    receipt = legacy.load_json(task_dir / "commit.json")
    worktree = Path(str(contract["worktree_path"]))
    if not worktree.is_dir() or worktree.is_symlink():
        raise legacy.HarnessError("v2 committed worktree is missing or unsafe")
    if (
        Path(legacy.git(worktree, "rev-parse", "--show-toplevel")).resolve()
        != worktree.resolve()
    ):
        raise legacy.HarnessError("v2 committed worktree path is not its Git root")
    if legacy.git(worktree, "branch", "--show-current") != contract["branch"]:
        raise legacy.HarnessError("v2 committed task branch changed")
    if legacy.git_bytes(worktree, "status", "--porcelain").strip():
        raise legacy.HarnessError("v2 committed worktree/index is not clean")

    attested_commit = str(receipt.get("commit_sha", ""))
    if not legacy.SHA1_RE.fullmatch(attested_commit):
        raise legacy.HarnessError("v2 committed receipt commit is malformed")
    legacy.validate_schema(
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
        raise legacy.HarnessError("v2 attested default_base is malformed")
    current_head = legacy.git(worktree, "rev-parse", "HEAD")
    _v2_clean_catchup_merges(
        worktree,
        attested_commit,
        current_head,
        default_base=default_base,
    )
    parents = legacy.git(
        worktree, "show", "-s", "--format=%P", attested_commit
    ).split()
    if len(parents) != 1:
        raise legacy.HarnessError("v2 attested task commit must have exactly one parent")
    parent = parents[0]
    tree = legacy.git(worktree, "rev-parse", f"{attested_commit}^{{tree}}")
    diff = legacy.git_bytes(
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
        ("diff_sha256", legacy.sha256_bytes(diff)),
    ):
        if receipt.get(key) != value:
            raise legacy.HarnessError(f"v2 committed receipt {key} is stale")
    state = load_v2_state(task_dir)
    for key, value in (
        ("commit_sha", attested_commit),
        ("parent_sha", parent),
        ("tree_sha", tree),
        ("evidence_sha256", receipt["evidence_sha256"]),
    ):
        if state.get(key) != value:
            raise legacy.HarnessError(
                f"v2 committed state {key} differs from its receipt"
            )
    evidence = verifier.verify_v2_evidence(
        contract,
        task_dir,
        allow_test_adapter=allow_test_adapter,
        attested_commit_sha=attested_commit,
    )
    if receipt.get("evidence_sha256") != evidence.get("evidence_sha256"):
        raise legacy.HarnessError("v2 commit no longer binds its exact evidence")
    if receipt.get("author") != {
        "name": "QueaT",
        "email": "kgm004a@gmail.com",
    } or receipt.get("committer") != {
        "name": "QueaT",
        "email": "kgm004a@gmail.com",
    }:
        raise legacy.HarnessError("v2 committed receipt identity is invalid")
    actual_author = {
        "name": legacy.git(worktree, "show", "-s", "--format=%an", attested_commit),
        "email": legacy.git(worktree, "show", "-s", "--format=%ae", attested_commit),
    }
    actual_committer = {
        "name": legacy.git(worktree, "show", "-s", "--format=%cn", attested_commit),
        "email": legacy.git(worktree, "show", "-s", "--format=%ce", attested_commit),
    }
    actual_message = legacy.git(
        worktree, "show", "-s", "--format=%B", attested_commit
    ).rstrip("\n")
    _strict_v2_receipt_trailers(actual_message, evidence)
    if receipt.get("author") != actual_author or receipt.get("committer") != actual_committer:
        raise legacy.HarnessError("v2 committed receipt identity is stale")
    if receipt.get("message") != actual_message:
        raise legacy.HarnessError("v2 committed receipt message is stale")
    if receipt.get("authored_at") != legacy.git(
        worktree, "show", "-s", "--format=%aI", attested_commit
    ):
        raise legacy.HarnessError("v2 committed receipt authored_at is stale")
    if receipt.get("committed_at") != legacy.git(
        worktree, "show", "-s", "--format=%cI", attested_commit
    ):
        raise legacy.HarnessError("v2 committed receipt committed_at is stale")
    return receipt


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
    if legacy.git(
        primary, "rev-parse", "--verify", archive_ref, check=False
    ) != snapshot_sha:
        raise legacy.HarnessError("v2 clean archive ref is missing or moved")
    if legacy.git(primary, "rev-parse", f"{snapshot_sha}^{{tree}}") != tree_sha:
        raise legacy.HarnessError("v2 clean archive tree is stale")

    expected_snapshots = [
        {
            "path": str(path),
            "snapshot_ref": reference,
            "snapshot_commit": commit,
        }
        for path, reference, commit in _verification_snapshots_for_cleanup(
            contract, task_dir
        )
    ]
    if intent.get("verification_snapshots") != expected_snapshots:
        raise legacy.HarnessError(
            "v2 clean intent snapshot set differs from task evidence"
        )

    server_raw = intent.get("server_worktree")
    source_raw = intent.get("server_source")
    server_mode = intent.get("server_mode")
    if server_raw is not None:
        server_worktree = Path(str(server_raw))
        server_source = Path(str(source_raw))
        expected_server = worktree.parent / "murmur-server"
        if server_worktree.resolve() != expected_server.resolve():
            raise legacy.HarnessError("v2 clean server path escaped task root")
        if server_worktree.exists() or server_worktree.is_symlink():
            if not server_worktree.is_dir() or server_worktree.is_symlink():
                raise legacy.HarnessError("v2 clean server path is unsafe")
            if legacy.git_bytes(
                server_worktree, "status", "--porcelain"
            ).strip():
                raise legacy.HarnessError(
                    "refusing clean: pinned server checkout became dirty"
                )
            if server_mode == "local-shared-clone":
                common = Path(
                    legacy.git(
                        server_worktree,
                        "rev-parse",
                        "--path-format=absolute",
                        "--git-common-dir",
                    )
                ).resolve()
                if common != (server_worktree / ".git").resolve():
                    raise legacy.HarnessError(
                        "v2 clean server clone shares mutable Git metadata"
                    )
                shutil.rmtree(server_worktree)
            elif server_mode == "linked-worktree":
                legacy.run_capture(
                    ["git", "worktree", "remove", str(server_worktree)],
                    server_source,
                )
            else:
                raise legacy.HarnessError(
                    "v2 clean intent server mode is unsupported"
                )

    for entry in expected_snapshots:
        snapshot = Path(entry["path"])
        if snapshot.exists() or snapshot.is_symlink():
            if not snapshot.is_dir() or snapshot.is_symlink():
                raise legacy.HarnessError(
                    "v2 verification snapshot became unsafe before cleanup"
                )
            common = Path(
                legacy.git(
                    snapshot,
                    "rev-parse",
                    "--path-format=absolute",
                    "--git-common-dir",
                )
            ).resolve()
            if common != (snapshot / ".git").resolve():
                raise legacy.HarnessError(
                    "v2 verification snapshot shares mutable Git metadata"
                )
            shutil.rmtree(snapshot)

    if worktree.exists() or worktree.is_symlink():
        if not worktree.is_dir() or worktree.is_symlink():
            raise legacy.HarnessError("v2 clean client worktree is unsafe")
        ignored = _non_disposable_ignored_paths(worktree)
        if ignored:
            shown = ", ".join(ignored[:20])
            suffix = "" if len(ignored) <= 20 else f" (+{len(ignored) - 20} more)"
            raise legacy.HarnessError(
                "refusing clean: ignored task bytes are not archived: "
                + shown
                + suffix
            )
        current_tree = _private_visible_tree(worktree, task_dir / "runtime")
        if current_tree != tree_sha:
            raise legacy.HarnessError(
                "refusing clean: task bytes changed after the durable archive"
            )
        node_modules = worktree / "node_modules"
        if legacy.managed_node_modules_link(worktree):
            node_modules.unlink()
        legacy.run_capture(
            ["git", "worktree", "remove", "--force", str(worktree)], primary
        )

    legacy.delete_local_task_branch(
        primary,
        str(contract["branch"]),
        snapshot_sha,
        archive_ref,
    )
    for entry in expected_snapshots:
        reference = entry["snapshot_ref"]
        commit_sha = entry["snapshot_commit"]
        current = legacy.git(
            primary, "rev-parse", "--verify", reference, check=False
        )
        if current:
            if current != commit_sha:
                raise legacy.HarnessError(
                    "v2 verification snapshot ref moved before cleanup"
                )
            legacy.run_capture(
                ["git", "update-ref", "-d", reference, commit_sha],
                primary,
            )


def cmd_clean(args: argparse.Namespace) -> int:
    contract, task_dir, _ = load_v2_task(args.task_id, Path.cwd())
    lock = acquire_v2_run_lock(task_dir, "clean")
    try:
        state = load_v2_state(task_dir)
        if state.get("status") in verifier.V2_TERMINAL_STATES:
            print(
                f"{contract['task_id']}: {state['status']} "
                "(cleanup already complete)"
            )
            return 0
        worktree = Path(str(contract["worktree_path"]))
        intent = _load_clean_intent(contract, task_dir)
        if intent is None:
            if not worktree.is_dir() or worktree.is_symlink():
                raise legacy.HarnessError(
                    "v2 task lost its worktree before a durable clean intent"
                )
            ignored = _non_disposable_ignored_paths(worktree)
            if ignored:
                shown = ", ".join(ignored[:20])
                suffix = (
                    "" if len(ignored) <= 20 else f" (+{len(ignored) - 20} more)"
                )
                raise legacy.HarnessError(
                    "refusing clean: ignored task bytes are not archived: "
                    + shown
                    + suffix
                )
            dirty_after_commit = bool(legacy.changed_paths(worktree))
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
                raise legacy.HarnessError(
                    "uncommitted/dirty v2 clean requires explicit --abandon"
                )
            server = _server_cleanup_preflight(contract, task_dir)
            verification_snapshots = _verification_snapshots_for_cleanup(
                contract, task_dir
            )
            primary = Path(str(contract["repo_realpath"])).resolve()
            archive_ref, snapshot, tree = _archive_all_visible_bytes(
                primary, worktree, contract, task_dir
            )
            intent = _clean_intent_document(
                contract,
                final_status="CLOSED" if clean_close else "ABANDONED",
                previous_status=str(state.get("status")),
                archive_ref=archive_ref,
                snapshot_sha=snapshot,
                tree_sha=tree,
                server=server,
                verification_snapshots=verification_snapshots,
            )
            legacy.atomic_write_json(task_dir / "clean-intent.json", intent)
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
        )
    finally:
        release_v2_run_lock(lock)
    print(
        f"{contract['task_id']}: {load_v2_state(task_dir)['status']} "
        f"(snapshot preserved: {intent['archive_ref']})"
    )
    return 0


def _v2_task_for_worktree(cwd: Path) -> Tuple[Dict[str, Any], Path]:
    top = Path(legacy.git(cwd, "rev-parse", "--show-toplevel")).resolve()
    _, common = legacy.repo_context(cwd)
    root = v2_tasks(common)
    matches: List[Tuple[Dict[str, Any], Path]] = []
    malformed_claim = False
    if root.is_dir():
        for task_dir in sorted(root.iterdir()):
            manifest = task_dir / "task.json"
            if not manifest.is_file():
                continue
            try:
                contract = legacy.load_json(manifest)
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
        raise legacy.HarnessError(
            "malformed v2 task claim exists; refusing no-task fallback"
        )
    if len(matches) != 1:
        raise legacy.HarnessError(
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
        raise legacy.HarnessError(
            f"refusing unsafe v2 task lock: {lock_path}"
        ) from exc
    handle = os.fdopen(descriptor, "r+b", buffering=0)
    try:
        opened = os.fstat(handle.fileno())
        if not stat.S_ISREG(opened.st_mode):
            raise legacy.HarnessError(
                f"v2 task lock is not a regular file: {lock_path}"
            )
        current = os.stat(lock_path, follow_symlinks=False)
        if (opened.st_dev, opened.st_ino) != (current.st_dev, current.st_ino):
            raise legacy.HarnessError(
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
    generation, contract, task_dir = resolve_generation(args.task_id, Path.cwd())
    if generation == 1:
        return legacy.main(["status", args.task_id, *(["--json"] if args.json else [])])
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


def _dual_lifecycle_audit(primary: Path, common: Path) -> Dict[str, Any]:
    claimed_client: set = set()
    claimed_server: set = set()
    claimed_verification: set = set()
    ghosts: List[str] = []
    gc_debt: List[str] = []
    roots = (
        (1, common / "agent-harness" / "tasks"),
        (2, v2_tasks(common)),
    )
    for generation, root in roots:
        if not root.is_dir():
            continue
        for task_dir in sorted(path for path in root.iterdir() if path.is_dir()):
            manifest = task_dir / "task.json"
            if not manifest.is_file():
                ghosts.append(f"v{generation}:{task_dir.name}:missing-task")
                continue
            try:
                contract = legacy.load_json(manifest)
                worktree = Path(str(contract["worktree_path"])).resolve()
                claimed_client.add(worktree)
                if generation == 1:
                    claimed_server.add((worktree.parent / "murmur-server").resolve())
                else:
                    runtime_path = task_dir / "runtime.json"
                    if runtime_path.is_file():
                        runtime = legacy.load_json(runtime_path)
                        server_worktree = runtime.get("server_worktree")
                        if isinstance(server_worktree, str) and server_worktree:
                            claimed_server.add(Path(server_worktree).resolve())
                    attempts = task_dir / "attempts"
                    if attempts.is_dir():
                        for attempt in attempts.iterdir():
                            snapshot_manifest = attempt / "snapshot.json"
                            if not snapshot_manifest.is_file():
                                continue
                            snapshot_doc = legacy.load_json(snapshot_manifest)
                            snapshot_path = Path(
                                str(snapshot_doc.get("path", ""))
                            ).resolve()
                            if snapshot_path != _verification_snapshot_path(
                                contract, attempt
                            ):
                                raise legacy.HarnessError(
                                    "snapshot path differs from attempt"
                                )
                            claimed_verification.add(snapshot_path)
            except Exception as exc:  # noqa: BLE001 - doctor reports malformed debt
                ghosts.append(
                    f"v{generation}:{task_dir.name}:malformed-task:{type(exc).__name__}"
                )
                continue
            if generation == 1:
                state_path = task_dir / "state.json"
                if not state_path.is_file():
                    ghosts.append(f"v1:{task_dir.name}:missing-state")
                    status = "UNKNOWN"
                else:
                    try:
                        status = str(legacy.load_json(state_path).get("status"))
                    except Exception:
                        ghosts.append(f"v1:{task_dir.name}:malformed-state")
                        status = "UNKNOWN"
                if status in {"RUNNING", "CHECKING", "REVIEWING", "REPAIRING"} and not legacy.task_run_lock_blocks_reap(task_dir):
                    ghosts.append(f"v1:{task_dir.name}:stale-{status.lower()}")
                if status in {"CLOSED", "REAPED"} and worktree.exists():
                    gc_debt.append(f"v1:{task_dir.name}:terminal-worktree")
            else:
                try:
                    event_state = _last_state_event(task_dir)
                    if event_state is None:
                        raise legacy.HarnessError("missing event state")
                    status = str(event_state.get("status"))
                    projection = (
                        legacy.load_json(task_dir / "state.json")
                        if (task_dir / "state.json").is_file()
                        else None
                    )
                    if projection != event_state:
                        ghosts.append(f"v2:{task_dir.name}:projection-gap")
                except Exception:
                    ghosts.append(f"v2:{task_dir.name}:malformed-state")
                    status = "UNKNOWN"
                liveness, _owner = lock_liveness(task_dir)
                if status == "VERIFYING" and liveness != "LIVE":
                    ghosts.append(f"v2:{task_dir.name}:stale-verifying")
                if status in verifier.V2_TERMINAL_STATES and worktree.exists():
                    gc_debt.append(f"v2:{task_dir.name}:terminal-worktree")
                if status in verifier.V2_TERMINAL_STATES and any(
                    path.exists()
                    for path in claimed_verification
                    if path.parent == worktree.parent
                ):
                    gc_debt.append(
                        f"v2:{task_dir.name}:terminal-verification-snapshot"
                    )

    orphans: List[str] = []
    repositories = [(primary, claimed_client)]
    sibling_server = primary.parent / "murmur-server"
    if (sibling_server / ".git").exists():
        repositories.append((sibling_server, claimed_server))
    for repository, claimed in repositories:
        output = legacy.run_capture(
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
    completed = subprocess.run(
        [sys.executable, str(Path(legacy.__file__).resolve()), "doctor", "--json"],
        cwd=str(Path.cwd()),
        text=True,
        capture_output=True,
        check=False,
    )
    try:
        payload = json.loads(completed.stdout)
    except json.JSONDecodeError as exc:
        raise legacy.HarnessError(
            "legacy doctor did not return JSON: " + completed.stderr.strip()
        ) from exc
    primary, common = legacy.repo_context(Path.cwd())
    lifecycle = _dual_lifecycle_audit(primary, common)
    checks = list(payload.get("checks", []))
    for name in (
        "v2-task",
        "v2-plan",
        "v2-review",
        "v2-evidence",
        "v2-commit-intent",
        "v2-commit",
    ):
        try:
            legacy.load_schema(name)
            checks.append(
                {
                    "name": f"schema:{name}",
                    "ok": True,
                    "required": True,
                    "detail": str(legacy.SCHEMAS_DIR / f"{name}.schema.json"),
                }
            )
        except legacy.HarnessError as exc:
            checks.append(
                {
                    "name": f"schema:{name}",
                    "ok": False,
                    "required": True,
                    "detail": str(exc),
                }
            )
    ok = (
        completed.returncode == 0
        and all(item.get("ok") for item in checks if item.get("required", True))
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
    legacy_args = ["selftest", *(["--ci"] if args.ci else [])]
    legacy_result = legacy.main(legacy_args)
    if legacy_result != 0:
        return legacy_result
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
        raise legacy.HarnessError("metrics --limit must be positive")
    _, common = legacy.repo_context(Path.cwd())
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
    open_parser.add_argument("--base")
    open_parser.add_argument("--branch")
    open_parser.set_defaults(expected_change=True)
    open_parser.set_defaults(handler=cmd_open)

    plan_parser = subparsers.add_parser(
        "plan", help="derive checks and reviews from the exact current diff"
    )
    plan_parser.add_argument("task_id")
    plan_parser.set_defaults(handler=cmd_plan)

    status_parser = subparsers.add_parser("status", help="show v1 or v2 state")
    status_parser.add_argument("task_id")
    status_parser.add_argument("--json", action="store_true")
    status_parser.set_defaults(handler=cmd_status)

    commit_parser = subparsers.add_parser(
        "commit", help="commit the exact PASS receipt for v1 or v2"
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
    import_parser = subparsers.add_parser(
        "import-v1", help="adopt a v1 diff/history without mutating v1 evidence"
    )
    import_parser.add_argument("task_id")
    import_parser.add_argument(
        "--invalidate-pass",
        action="store_true",
        help="explicitly invalidate a v1 PASS/COMMITTED lifecycle before adopting it",
    )
    import_parser.add_argument(
        "--claim", action="append", choices=["runtime", "performance"], default=[]
    )
    import_parser.add_argument("--reviewer", choices=["codex", "claude"])
    import_parser.set_defaults(handler=cmd_import_v1)
    clean_parser = subparsers.add_parser(
        "clean", help="archive every visible byte, then close or abandon a v2 task"
    )
    clean_parser.add_argument("task_id")
    clean_parser.add_argument("--abandon", action="store_true")
    clean_parser.set_defaults(handler=cmd_clean)
    doctor_parser = subparsers.add_parser(
        "doctor", help="audit dependencies plus v1/v2 ghost and GC debt"
    )
    doctor_parser.add_argument("--json", action="store_true")
    doctor_parser.set_defaults(handler=cmd_doctor)
    metrics_parser = subparsers.add_parser(
        "metrics", help="roll up append-only v1/v2 operational telemetry"
    )
    metrics_parser.add_argument("--json", action="store_true")
    metrics_parser.add_argument("--limit", type=int, default=20)
    metrics_parser.set_defaults(handler=cmd_metrics)
    selftest_parser = subparsers.add_parser(
        "selftest", help="run legacy and v2 deterministic fault tests"
    )
    selftest_parser.add_argument("--ci", action="store_true")
    selftest_parser.set_defaults(handler=cmd_selftest)
    return parser


def main(argv: Optional[Sequence[str]] = None) -> int:
    arguments = list(argv if argv is not None else sys.argv[1:])
    if arguments and arguments[0] in V1_COMMANDS:
        return legacy.main(arguments)
    if arguments and arguments[0] in {"commit", "guard-commit"}:
        task_id = next((item for item in arguments[1:] if not item.startswith("-")), "")
        if task_id:
            generation, _, _ = resolve_generation(task_id, Path.cwd())
            if generation == 1:
                return legacy.main(arguments)
        elif arguments[0] == "guard-commit":
            try:
                _v2_task_for_worktree(Path.cwd())
            except legacy.HarnessError as exc:
                if "found 0" in str(exc):
                    return legacy.main(arguments)
                raise
    parser = build_parser()
    args = parser.parse_args(arguments)
    return int(args.handler(args))


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except legacy.HarnessError as exc:
        print(f"agent-harness: {exc}", file=sys.stderr)
        raise SystemExit(exc.exit_code)
