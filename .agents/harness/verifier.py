#!/usr/bin/env python3
"""Trust kernel for the thin, resumable Murmur Harness v2.

This module deliberately has no writer or repair primitive.  It derives a
verification plan from the exact staged diff, runs only runner-owned checks and
fresh read-only reviews, and verifies the resulting exact-diff evidence bundle.
The lifecycle/worktree commands live in :mod:`cli`.
"""

from __future__ import annotations

import copy
import fnmatch
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import time
from typing import Any, Callable, Dict, Iterable, List, Mapping, Optional, Sequence, Tuple

import task_runner as legacy


V2_STATES = {
    "OPEN",
    "VERIFYING",
    "NEEDS_FIX",
    "NEEDS_EVIDENCE",
    "PAUSED_RETRYABLE",
    "INTERRUPTED",
    "PASSED",
    "COMMITTED",
    "CLOSED",
    "ABANDONED",
}
V2_TERMINAL_STATES = {"CLOSED", "ABANDONED"}
V2_RESUMABLE_STATES = V2_STATES - V2_TERMINAL_STATES - {"COMMITTED"}
SEVERE_FINDINGS = {"MAJOR", "BLOCKER"}
ALLOWED_PROBES = {
    "rust-lib",
    "protocol-server",
    "npm-lock",
    "tauri-boot",
    "ng-lint",
    "ng-build",
    "playwright",
    "perf-contracts",
    "harness-python",
    "harness-v2-selftest",
    "receipt-selftest",
    "hook-selftest",
    "config-audit",
}
MAX_PROBE_EXECUTIONS_PER_ID = 2
DEFAULT_REVIEW_STREAM_BYTES = 2_048
NPM_LOCK_REVIEW_STREAM_BYTES = 65_536
SPECIALIST_TEST_FOCUS_LINES_PER_TERM = 12
SPECIALIST_TEST_FOCUS_BYTES = 32_768
SPECIALIST_SOURCE_CONTEXT_BYTES = 32_768
SPECIALIST_SOURCE_CONTEXT_LABEL = (
    "Canonical unchanged risk-seam source context "
    "(snapshot-derived source evidence only; not runtime proof): "
)
SPECIALIST_TEST_FOCUS_TERMS = {
    "lock-security": (
        "lock",
        "unlock",
        "seal",
        "visibility",
        "org",
        "member",
        "context",
        "tombstone",
        "revok",
    ),
    "egress-security": (
        "egress",
        "consent",
        "redact",
        "provider",
        "ollama",
        "anthropic",
        "gateway",
        "remote",
        "loopback",
        "ledger",
    ),
}
SPECIALIST_SOURCE_SEAMS = {
    "egress-security": (
        {
            "path": "src-tauri/src/summarize/mod.rs",
            "start": r"(?m)^fn make_provider_resolved\(",
            "symbol": "make_provider_resolved",
        },
    ),
}
TRANSIENT_HTTP_STATUSES = {408, 409, 425, 429, 500, 502, 503, 504}
PROTOCOL_FILES = (
    "AGENTS.md",
    "CLAUDE.md",
    ".claude/settings.json",
    ".codex/config.toml",
    ".codex/hooks.json",
    ".codex/rules/agentic-workflow.md",
    ".claude/rules/agentic-workflow.md",
    ".agents/harness/cli.py",
    ".agents/harness/verifier.py",
    ".agents/harness/task_runner.py",
    ".agents/harness/process_guardian.py",
    ".agents/harness/hook_guard.py",
    ".agents/harness/config_audit.py",
    ".agents/harness/resource_policy.py",
    ".agents/harness/v2_selftest.py",
    ".agents/harness/config.json",
    ".agents/harness/prompts/combined-reviewer.md",
    ".agents/harness/prompts/lock-security-reviewer.md",
    ".agents/harness/prompts/egress-security-reviewer.md",
    ".agents/harness/prompts/protocol-security-reviewer.md",
    ".agents/harness/schemas/v2-task.schema.json",
    ".agents/harness/schemas/v2-plan.schema.json",
    ".agents/harness/schemas/v2-review.schema.json",
    ".agents/harness/schemas/v2-evidence.schema.json",
    ".agents/harness/schemas/v2-commit-intent.schema.json",
    ".agents/harness/schemas/v2-commit.schema.json",
    "scripts/agent-harness",
    "scripts/agent-resource-run",
    "scripts/agent-config-audit",
    "scripts/harness-runtime-smoke",
    "scripts/harness-runtime-smoke.py",
    "scripts/verify-harness-attestation",
    "scripts/ci.sh",
    ".github/workflows/ci.yml",
    ".codex/hooks/finish-guard.sh",
    ".codex/hooks/selftest.sh",
    ".claude/hooks/finish-guard.sh",
    ".claude/hooks/selftest.sh",
)
SOURCE_ROOT = Path(__file__).resolve().parents[2]


class ReviewPaused(legacy.HarnessError):
    """A reviewer infrastructure failure exhausted the one bounded retry."""

    def __init__(self, message: str, attempts: Sequence[Mapping[str, Any]]) -> None:
        super().__init__(message)
        self.attempts = [dict(item) for item in attempts]


def last_state_event(task_dir: Path) -> Optional[Dict[str, Any]]:
    path = task_dir / "events.jsonl"
    if not path.is_file():
        return None
    result: Optional[Dict[str, Any]] = None
    try:
        with path.open("r", encoding="utf-8", errors="strict") as handle:
            for line in handle:
                document = json.loads(line)
                if (
                    isinstance(document, dict)
                    and document.get("event") == "state"
                    and isinstance(document.get("state"), dict)
                ):
                    result = document["state"]
    except (OSError, UnicodeError, json.JSONDecodeError) as exc:
        raise legacy.HarnessError(
            f"v2 event ledger is malformed: {path}: {exc}"
        ) from exc
    return result


def load_v2_state(task_dir: Path) -> Dict[str, Any]:
    """Resolve the append-only ledger and repair only a missing/stale projection."""

    event_state = last_state_event(task_dir)
    if event_state is None:
        raise legacy.HarnessError("v2 state has no authoritative event")
    if (
        event_state.get("schema_version") != 2
        or event_state.get("task_id") != task_dir.name
        or event_state.get("status") not in V2_STATES
        or not isinstance(event_state.get("updated_at"), str)
    ):
        raise legacy.HarnessError("v2 authoritative state event is malformed")
    event_time = legacy.parse_timestamp(
        event_state["updated_at"], "v2 state event updated_at"
    )
    state_path = task_dir / "state.json"
    if not state_path.is_file():
        legacy.atomic_write_json(state_path, event_state)
        return event_state
    projected = legacy.load_json(state_path)
    if (
        projected.get("schema_version") != 2
        or projected.get("task_id") != task_dir.name
        or projected.get("status") not in V2_STATES
        or not isinstance(projected.get("updated_at"), str)
    ):
        raise legacy.HarnessError("v2 state projection is malformed")
    projected_time = legacy.parse_timestamp(
        projected["updated_at"], "v2 state projection updated_at"
    )
    event_revision = event_state.get("state_revision")
    projected_revision = projected.get("state_revision")
    for label, revision in (
        ("event", event_revision),
        ("projection", projected_revision),
    ):
        if revision is not None and (
            isinstance(revision, bool)
            or not isinstance(revision, int)
            or revision < 1
        ):
            raise legacy.HarnessError(
                f"v2 state {label} revision is malformed"
            )
    if event_revision is not None:
        if projected_revision is not None:
            if projected_revision > event_revision:
                raise legacy.HarnessError(
                    "v2 state projection is newer than its event ledger"
                )
            if (
                projected_revision == event_revision
                and projected != event_state
            ):
                raise legacy.HarnessError(
                    "v2 state projection conflicts at the same revision"
                )
    elif projected_revision is not None:
        raise legacy.HarnessError(
            "v2 revised state projection has a legacy event ledger"
        )
    elif projected_time > event_time or (
        projected_time == event_time and projected != event_state
    ):
        raise legacy.HarnessError(
            "v2 state projection is newer than or conflicts with its event ledger"
        )
    if projected != event_state:
        legacy.atomic_write_json(state_path, event_state)
    return event_state


def canonical_hash(value: Any) -> str:
    return legacy.sha256_bytes(legacy.canonical_json(value))


def document_hash(document: Mapping[str, Any], hash_field: str) -> str:
    payload = copy.deepcopy(dict(document))
    payload[hash_field] = ""
    return canonical_hash(payload)


def validate_hashed_document(
    document: Mapping[str, Any],
    schema_name: str,
    hash_field: str,
    label: str,
    *,
    schema: Optional[Mapping[str, Any]] = None,
) -> None:
    selected_schema = (
        dict(schema) if schema is not None else legacy.load_schema(schema_name)
    )
    legacy.validate_schema(dict(document), selected_schema, label=label)
    expected = document_hash(document, hash_field)
    if document.get(hash_field) != expected:
        raise legacy.HarnessError(
            f"{label} {hash_field} mismatch: expected {expected}, found {document.get(hash_field)}"
        )


def protocol_relative_paths(worktree: Path) -> List[str]:
    values = set(PROTOCOL_FILES)
    harness_root = worktree / ".agents" / "harness"
    if not harness_root.is_dir() or harness_root.is_symlink():
        raise legacy.HarnessError(
            "v2 protocol directory is missing or unsafe: .agents/harness"
        )
    # New runner modules must enter the protocol automatically.  A hand-kept
    # list silently omitted exactly the sort of executable helper that can
    # change a verdict (metrics, migrations, or fault-test entrypoints).
    python_modules = sorted(harness_root.glob("*.py"))
    unsafe_python = [
        path.relative_to(worktree).as_posix()
        for path in python_modules
        if not path.is_file() or path.is_symlink()
    ]
    if unsafe_python:
        raise legacy.HarnessError(
            "v2 protocol Python module is missing or unsafe: "
            + ", ".join(unsafe_python)
        )
    values.update(
        path.relative_to(worktree).as_posix() for path in python_modules
    )
    for directory in (
        ".agents/harness/checks",
        ".agents/harness/prompts",
        ".agents/harness/schemas",
    ):
        root = worktree / directory
        if not root.is_dir() or root.is_symlink():
            raise legacy.HarnessError(
                f"v2 protocol directory is missing or unsafe: {directory}"
            )
        values.update(
            path.relative_to(worktree).as_posix()
            for path in root.rglob("*")
            if path.is_file() and not path.is_symlink()
        )
    return sorted(values)


def protocol_bundle(worktree: Path) -> Dict[str, Any]:
    files: List[Dict[str, str]] = []
    for relative in protocol_relative_paths(worktree):
        path = worktree / relative
        if not path.is_file() or path.is_symlink():
            raise legacy.HarnessError(f"v2 protocol file is missing or unsafe: {relative}")
        files.append(
            {
                "path": relative,
                "sha256": legacy.sha256_file(path),
            }
        )
    bundle: Dict[str, Any] = {
        "schema_version": 2,
        "files": files,
    }
    bundle["protocol_sha256"] = canonical_hash(bundle)
    return bundle


def executable_protocol_bundle(worktree: Path) -> Dict[str, Any]:
    """Require the running verifier to equal the task-pinned protocol bytes."""

    executing = protocol_bundle(SOURCE_ROOT)
    pinned = protocol_bundle(worktree)
    if executing != pinned:
        raise legacy.HarnessError(
            "the executing Harness v2 protocol differs from the task worktree; "
            f"run {worktree / 'scripts' / 'agent-harness'} for this task"
        )
    return pinned


def git_file_at_commit(repo: Path, commit_sha: str, relative: str) -> bytes:
    completed = subprocess.run(
        ["git", "show", f"{commit_sha}:{relative}"],
        cwd=str(repo),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if completed.returncode != 0:
        raise legacy.HarnessError(
            f"v2 attested commit is missing required file: {relative}"
        )
    return completed.stdout


def attested_json_object(
    repo: Path, commit_sha: str, relative: str, label: str
) -> Dict[str, Any]:
    try:
        document = json.loads(
            git_file_at_commit(repo, commit_sha, relative).decode("utf-8")
        )
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise legacy.HarnessError(
            f"v2 attested {label} is malformed"
        ) from exc
    if not isinstance(document, dict):
        raise legacy.HarnessError(f"v2 attested {label} is not an object")
    return document


def attested_schema(
    repo: Path, commit_sha: str, schema_name: str
) -> Dict[str, Any]:
    return attested_json_object(
        repo,
        commit_sha,
        f".agents/harness/schemas/{schema_name}.schema.json",
        f"{schema_name} schema",
    )


def _matches(path: str, patterns: Iterable[str]) -> bool:
    return any(
        re.fullmatch(legacy._glob_pattern_to_regex(pattern), path)
        for pattern in patterns
    )


def actual_risks(paths: Sequence[str], config: Mapping[str, Any]) -> List[str]:
    result: List[str] = []
    classification = config.get("risk_classification", {})
    if not isinstance(classification, Mapping):
        raise legacy.HarnessError("risk_classification is malformed")
    for risk in ("lock", "egress", "protocol"):
        patterns = classification.get(risk, [])
        if not isinstance(patterns, list):
            raise legacy.HarnessError(f"risk_classification.{risk} is malformed")
        if any(
            isinstance(pattern, str)
            and re.fullmatch(legacy._glob_pattern_to_regex(pattern), path)
            for path in paths
            for pattern in patterns
        ):
            result.append(risk)
    if _protocol_surface(paths) and "protocol" not in result:
        result.append("protocol")
    return result


def _rust_surface(paths: Sequence[str]) -> bool:
    patterns = (
        "src-tauri/src/**",
        "src-tauri/crates/**",
        "crates/**/*.rs",
        "crates/**/Cargo.toml",
        "src-tauri/Cargo.toml",
        "src-tauri/Cargo.lock",
        "Cargo.toml",
        "Cargo.lock",
        ".cargo/**",
        ".murmur-server-revision",
    )
    return any(_matches(path, patterns) for path in paths)


def _angular_surface(paths: Sequence[str]) -> bool:
    patterns = (
        "src/app/**",
        "src/design-tokens/**",
        "src/main.ts",
        "src/styles.*",
        "angular.json",
        "package.json",
        "package-lock.json",
        "tsconfig*.json",
        "eslint.config.*",
    )
    return any(_matches(path, patterns) for path in paths)


def _package_lock_surface(paths: Sequence[str]) -> bool:
    return any(path in {"package.json", "package-lock.json"} for path in paths)


def _ui_behavior_surface(paths: Sequence[str]) -> bool:
    for path in paths:
        if path.startswith("e2e/") or path == "playwright.config.ts":
            return True
        if path.startswith(("src/app/features/", "src/app/core/", "src/app/services/")):
            return Path(path).suffix in {".ts", ".html"}
    return False


def _protocol_surface(paths: Sequence[str]) -> bool:
    return any(
        path.startswith("src-tauri/src/share/")
        or path == ".murmur-server-revision"
        or path.startswith("crates/murmur-protocol/")
        for path in paths
    )


def _harness_surface(paths: Sequence[str]) -> bool:
    patterns = (
        ".agents/harness/**",
        ".agents/skills/harness/**",
        ".agents/skills/ship-feature/**",
        ".claude/skills/harness/**",
        ".claude/skills/ship-feature/**",
        ".codex/rules/agentic-workflow.md",
        ".claude/rules/agentic-workflow.md",
        ".codex/hooks/**",
        ".claude/hooks/**",
        "scripts/agent-harness",
        "scripts/agent-resource-run",
        "scripts/harness-runtime-smoke*",
        "scripts/verify-harness-attestation",
        "scripts/ci.sh",
        ".github/workflows/ci.yml",
    )
    return any(_matches(path, patterns) for path in paths)


def derive_profile(
    paths: Sequence[str],
    claims: Sequence[str],
    config: Mapping[str, Any],
    *,
    reviewer: str,
    allow_same_vendor_high_risk: bool = False,
) -> Tuple[List[Dict[str, Any]], List[Dict[str, str]], List[str]]:
    """Return checks, reviews and actual sensitive risks for an exact path set.

    Language/build checks are selected independently from semantic sensitivity.
    Runtime and performance are selected only from explicit claims.
    """

    claim_set = set(claims)
    unknown_claims = claim_set - {"runtime", "performance"}
    if unknown_claims:
        raise legacy.HarnessError("unknown v2 claims: " + ", ".join(sorted(unknown_claims)))
    canonical = config.get("canonical_checks", {})
    if not isinstance(canonical, Mapping):
        raise legacy.HarnessError("canonical_checks is malformed")
    timeout = int(config.get("check_timeout_seconds", 1800))
    check_ids: List[str] = []

    def require(check_id: str) -> None:
        if check_id not in check_ids:
            check_ids.append(check_id)

    if _rust_surface(paths):
        require("rust-lib")
    if _package_lock_surface(paths):
        require("npm-lock")
    if _angular_surface(paths):
        require("ng-lint")
        require("ng-build")
    if _ui_behavior_surface(paths):
        require("playwright")
    if _protocol_surface(paths):
        require("rust-lib")
        require("protocol-server")
    if "runtime" in claim_set:
        require("tauri-boot")
    if "performance" in claim_set:
        require("perf-contracts")
    if _harness_surface(paths):
        for check_id in (
            "harness-python",
            "harness-v2-selftest",
            "receipt-selftest",
            "hook-selftest",
            "config-audit",
        ):
            require(check_id)

    checks = [canonical_check(check_id, config) for check_id in check_ids]

    risks = actual_risks(paths, config)
    reviews = [{"kind": "combined", "vendor": reviewer}]
    opposite = {"claude": "codex", "codex": "claude", "fake": "fake"}.get(reviewer)
    if opposite is None:
        raise legacy.HarnessError(f"unsupported v2 reviewer: {reviewer}")
    specialist_vendor = reviewer if allow_same_vendor_high_risk else opposite
    mapping = config.get("risk_reviews", {})
    for risk in risks:
        kind = mapping.get(risk) if isinstance(mapping, Mapping) else None
        if not isinstance(kind, str):
            raise legacy.HarnessError(f"no specialist review configured for {risk}")
        reviews.append({"kind": kind, "vendor": specialist_vendor})
    return checks, reviews, risks


def canonical_check(
    check_id: str, config: Mapping[str, Any]
) -> Dict[str, Any]:
    if check_id not in ALLOWED_PROBES:
        raise legacy.HarnessError(f"probe/check id is not allowlisted: {check_id}")
    canonical = config.get("canonical_checks", {})
    command = canonical.get(check_id) if isinstance(canonical, Mapping) else None
    if not isinstance(command, str) or not command.strip():
        raise legacy.HarnessError(f"no canonical command for v2 check {check_id}")
    return {
        "id": check_id,
        "command": command,
        "timeout_seconds": int(config.get("check_timeout_seconds", 1800)),
    }


def allowed_probe_ids(plan: Mapping[str, Any]) -> List[str]:
    """Return the only probes meaningful for this exact derived plan.

    The static schema is a protocol vocabulary, not authority to execute every
    check for every task.  A missing claim/profile must be amended explicitly;
    an unrelated globally-green command can never stand in for missing proof.
    """

    checks = plan.get("checks", [])
    if not isinstance(checks, list):
        raise legacy.HarnessError("v2 plan checks are malformed")
    result: List[str] = []
    for check in checks:
        if not isinstance(check, Mapping):
            raise legacy.HarnessError("v2 plan check is malformed")
        check_id = check.get("id")
        if not isinstance(check_id, str) or check_id not in ALLOWED_PROBES:
            raise legacy.HarnessError("v2 plan contains a non-canonical probe id")
        if check_id not in result:
            result.append(check_id)
    return result


def probe_evidence_hash(records: Sequence[Mapping[str, Any]]) -> str:
    ordered = sorted(
        (dict(record) for record in records),
        key=lambda item: str(item.get("id", "")),
    )
    return canonical_hash(ordered)


def build_plan(
    contract: Mapping[str, Any],
    worktree: Path,
    paths: Sequence[str],
    diff: bytes,
    tree_sha: str,
    config: Mapping[str, Any],
) -> Tuple[Dict[str, Any], Dict[str, Any]]:
    bundle = executable_protocol_bundle(worktree)
    checks, reviews, risks = derive_profile(
        paths,
        list(contract.get("claims", [])),
        config,
        reviewer=str(contract["reviewer"]),
        allow_same_vendor_high_risk=bool(
            contract.get("allow_same_vendor_high_risk", False)
        ),
    )
    head_sha = legacy.git(worktree, "rev-parse", "HEAD")
    plan: Dict[str, Any] = {
        "schema_version": 2,
        "task_id": contract["task_id"],
        "contract_sha256": contract["contract_sha256"],
        "base_sha": head_sha,
        "diff_sha256": legacy.sha256_bytes(diff),
        "tree_sha": tree_sha,
        "protocol_sha256": bundle["protocol_sha256"],
        "changed_paths": list(paths),
        "claims": list(contract.get("claims", [])),
        "actual_risk_flags": risks,
        "checks": checks,
        "reviews": reviews,
        "server_required": any(
            check["id"] in {"rust-lib", "protocol-server"} for check in checks
        ),
        # The plan is a content binding, not an execution event.  Replanning the
        # exact same contract and diff must reproduce the same plan/attempt IDs.
        "created_at": contract["created_at"],
        "plan_sha256": "",
    }
    plan["plan_sha256"] = document_hash(plan, "plan_sha256")
    legacy.validate_schema(plan, legacy.load_schema("v2-plan"), label="v2 plan")
    return plan, bundle


def snapshot_scoped_diff(
    worktree: Path,
    contract: Mapping[str, Any],
    task_dir: Path,
) -> Tuple[List[str], bytes, str]:
    """Snapshot all Git-visible task bytes through a private temporary index.

    Unlike the v1 staging primitive this never resets or writes the developer's
    real index.  Untracked files and deletions are represented exactly, and the
    resulting tree SHA is the one a later v2 commit must reproduce.
    """

    if legacy.git(worktree, "rev-parse", "HEAD") != contract["base_sha"]:
        raise legacy.HarnessError(
            "v2 task HEAD changed; import/re-open against the actual parent before verification"
        )
    paths = legacy.changed_paths(worktree)
    violations = [
        path
        for path in paths
        if not legacy.path_is_owned(path, contract["owned_paths"])
    ]
    if violations:
        raise legacy.HarnessError(
            "out-of-scope v2 changes: " + ", ".join(violations)
        )
    unsafe = legacy.unsafe_changed_nodes(worktree, paths)
    if unsafe:
        raise legacy.HarnessError(
            "unsafe changed nodes in v2 task: " + ", ".join(unsafe)
        )
    contaminated = legacy.tool_output_contaminated_paths(worktree, paths)
    if contaminated:
        raise legacy.HarnessError(
            "renderer-truncated output exists in changed files: "
            + ", ".join(contaminated)
        )
    runtime = task_dir / "runtime"
    runtime.mkdir(parents=True, exist_ok=True)
    descriptor, raw_index = tempfile.mkstemp(prefix="v2-index-", dir=str(runtime))
    os.close(descriptor)
    index_path = Path(raw_index)
    environment = {**os.environ, "GIT_INDEX_FILE": str(index_path)}
    try:
        index_path.unlink(missing_ok=True)
        subprocess.run(
            ["git", "read-tree", "HEAD"],
            cwd=str(worktree),
            env=environment,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        if paths:
            subprocess.run(
                ["git", "add", "-A", "--", *contract["owned_paths"]],
                cwd=str(worktree),
                env=environment,
                check=True,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
            )
        diff = subprocess.run(
            [
                "git",
                "diff",
                "--cached",
                "--binary",
                "--full-index",
                "--no-ext-diff",
                "--no-renames",
                "--",
            ],
            cwd=str(worktree),
            env=environment,
            check=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        ).stdout
        tree_sha = subprocess.run(
            ["git", "write-tree"],
            cwd=str(worktree),
            env=environment,
            check=True,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        ).stdout.strip()
    except subprocess.CalledProcessError as exc:
        stderr = exc.stderr.decode("utf-8", "replace") if isinstance(exc.stderr, bytes) else str(exc.stderr)
        raise legacy.HarnessError(f"could not snapshot v2 diff: {stderr.strip()}") from exc
    finally:
        index_path.unlink(missing_ok=True)
    if bool(diff) != bool(contract["expected_change"]):
        expectation = "requires a change" if contract["expected_change"] else "is no-change"
        raise legacy.HarnessError(f"v2 task {expectation}, but exact diff presence disagrees")
    return paths, diff, tree_sha


def attempt_id(plan: Mapping[str, Any]) -> str:
    return canonical_hash(
        {
            "diff_sha256": plan["diff_sha256"],
            "plan_sha256": plan["plan_sha256"],
            "protocol_sha256": plan["protocol_sha256"],
        }
    )


def evidence_binding(plan: Mapping[str, Any]) -> Dict[str, str]:
    return {
        "diff_sha256": str(plan["diff_sha256"]),
        "plan_sha256": str(plan["plan_sha256"]),
        "protocol_sha256": str(plan["protocol_sha256"]),
    }


def binding_matches(document: Mapping[str, Any], plan: Mapping[str, Any]) -> bool:
    return all(document.get(key) == value for key, value in evidence_binding(plan).items())


def _walk_json(value: Any) -> Iterable[Tuple[str, Any]]:
    if isinstance(value, Mapping):
        for key, child in value.items():
            yield str(key), child
            yield from _walk_json(child)
    elif isinstance(value, list):
        for child in value:
            yield from _walk_json(child)


def model_telemetry(log_path: Path, *, timed_out: bool = False) -> Dict[str, Any]:
    terminal_reason: Optional[str] = "timeout" if timed_out else None
    http_status: Optional[int] = None
    retry_after_seconds: Optional[float] = None
    cost_usd: Optional[float] = None
    turns: Optional[int] = None
    usage: Any = None
    if log_path.is_file():
        try:
            lines = log_path.read_text(encoding="utf-8", errors="replace").splitlines()
        except OSError:
            lines = []
        for line in lines:
            try:
                event = json.loads(line)
            except json.JSONDecodeError:
                continue
            for key, value in _walk_json(event):
                lowered = key.lower()
                if lowered in {"api_error_status", "status_code", "http_status"}:
                    try:
                        http_status = int(value)
                    except (TypeError, ValueError):
                        pass
                elif lowered in {"retry_after", "retry_after_seconds"}:
                    try:
                        retry_after_seconds = max(0.0, float(value))
                    except (TypeError, ValueError):
                        pass
                elif lowered in {"total_cost_usd", "cost_usd"}:
                    try:
                        cost_usd = float(value)
                    except (TypeError, ValueError):
                        pass
                elif lowered in {"num_turns", "turns"}:
                    try:
                        turns = int(value)
                    except (TypeError, ValueError):
                        pass
                elif lowered == "usage" and isinstance(value, Mapping):
                    usage = value
                elif lowered in {"terminal_reason", "stop_reason", "subtype"} and isinstance(value, str):
                    terminal_reason = value
    return {
        "terminal_reason": terminal_reason,
        "http_status": http_status,
        "retry_after_seconds": retry_after_seconds,
        "cost_usd": cost_usd,
        "turns": turns,
        "usage": usage,
    }


def transient_failure(message: str, telemetry: Mapping[str, Any], *, timed_out: bool) -> bool:
    if timed_out:
        return True
    status = telemetry.get("http_status")
    if isinstance(status, int) and status in TRANSIENT_HTTP_STATUSES:
        return True
    text = " ".join(
        str(item)
        for item in (message, telemetry.get("terminal_reason"))
        if item is not None
    ).lower()
    markers = (
        "rate limit",
        "429",
        "timed out",
        "timeout",
        "temporar",
        "connection reset",
        "connection refused",
        "network",
        "service unavailable",
        "overloaded",
        "502",
        "503",
        "504",
    )
    return any(marker in text for marker in markers)


def retry_call(
    invoke: Callable[[int], Mapping[str, Any]],
    *,
    sleep: Callable[[float], None] = time.sleep,
    max_retry_delay_seconds: float = 60.0,
) -> Tuple[Mapping[str, Any], List[Dict[str, Any]]]:
    """Invoke at most twice, retaining a typed record for both attempts.

    The callable may return ``{"ok": False, "transient": ...}`` or raise.  This
    small injectable seam is also used by deterministic fault tests; production
    review invocation remains the read-only model adapter below.
    """

    attempts: List[Dict[str, Any]] = []
    for number in (1, 2):
        try:
            outcome = dict(invoke(number))
        except Exception as exc:  # noqa: BLE001 - converted to typed retry evidence
            outcome = {
                "ok": False,
                "transient": False,
                "error": f"{type(exc).__name__}: {exc}",
                "retry_after_seconds": None,
            }
        outcome["attempt"] = number
        attempts.append(outcome)
        if outcome.get("ok"):
            return outcome, attempts
        if not outcome.get("transient"):
            raise legacy.HarnessError(
                f"review invocation failed permanently: {outcome.get('error', 'unknown error')}"
            )
        if number == 2:
            raise ReviewPaused(
                "review infrastructure failed twice; resume will retry only the missing review",
                attempts,
            )
        delay = outcome.get("retry_after_seconds")
        if not isinstance(delay, (int, float)):
            delay = 1.0
        sleep(min(max(0.0, float(delay)), max_retry_delay_seconds))
    raise AssertionError("unreachable")


def review_result_state(result: Mapping[str, Any]) -> str:
    findings = result.get("findings", [])
    if any(
        isinstance(finding, Mapping) and finding.get("severity") in SEVERE_FINDINGS
        for finding in findings
    ):
        return "NEEDS_FIX"
    if result.get("verdict") == "FAIL":
        return "NEEDS_FIX"
    if result.get("verdict") == "BLOCKED":
        return "NEEDS_EVIDENCE"
    proof_gaps = result.get("proof_gaps", [])
    probes = result.get("probe_requests", [])
    if proof_gaps or probes:
        return "NEEDS_EVIDENCE"
    if result.get("verdict") != "PASS":
        return "NEEDS_EVIDENCE"
    return "PASSED"


def _bounded_stream_summary(
    task_dir: Path,
    raw_path: Any,
    expected_hash: Any,
    label: str,
    limit: int,
    *,
    test_focus_terms: Sequence[str] = (),
) -> Dict[str, Any]:
    path = _artifact_inside(task_dir, raw_path, expected_hash, label)
    size = path.stat().st_size
    if size <= limit:
        with path.open("rb") as handle:
            excerpt = handle.read(limit + 1)
        truncated = False
    else:
        head = limit // 2
        tail = limit - head
        omitted = size - limit
        with path.open("rb") as handle:
            prefix = handle.read(head)
            handle.seek(-tail, os.SEEK_END)
            suffix = handle.read(tail)
        excerpt = prefix + (
            f"\n... <{omitted} evidence bytes omitted> ...\n".encode("utf-8")
        ) + suffix
        truncated = True
    summary = {
        "sha256": expected_hash,
        "bytes": size,
        "truncated": truncated,
        "excerpt_included": True,
        "excerpt": excerpt.decode("utf-8", "replace"),
    }
    if test_focus_terms:
        normalized_terms = tuple(
            dict.fromkeys(term.strip().lower() for term in test_focus_terms if term.strip())
        )
        per_term_total = {term: 0 for term in normalized_terms}
        per_term_lines: Dict[str, List[str]] = {
            term: [] for term in normalized_terms
        }
        focused_bytes = 0
        matching_lines_total = 0
        test_outcome = re.compile(
            r"^test .+ \.\.\. (?:ok|FAILED|ignored)$"
        )
        with path.open("r", encoding="utf-8", errors="replace") as handle:
            for raw_line in handle:
                line = raw_line.rstrip("\r\n")
                if not test_outcome.fullmatch(line):
                    continue
                lowered = line.lower()
                matched = tuple(
                    term for term in normalized_terms if term in lowered
                )
                if not matched:
                    continue
                matching_lines_total += 1
                for term in matched:
                    per_term_total[term] += 1
                    bucket = per_term_lines[term]
                    if len(bucket) >= SPECIALIST_TEST_FOCUS_LINES_PER_TERM:
                        continue
                    if bucket:
                        entry = "\n" + line
                    else:
                        separator = "\n" if any(per_term_lines.values()) else ""
                        entry = f"{separator}[{term}]\n{line}"
                    encoded_size = len(entry.encode("utf-8"))
                    if (
                        focused_bytes + encoded_size
                        > SPECIALIST_TEST_FOCUS_BYTES
                    ):
                        continue
                    bucket.append(line)
                    focused_bytes += encoded_size
        focused_sections = [
            f"[{term}]\n" + "\n".join(per_term_lines[term])
            for term in normalized_terms
            if per_term_lines[term]
        ]
        focused_excerpt = "\n".join(focused_sections)
        per_term_included = {
            term: len(per_term_lines[term]) for term in normalized_terms
        }
        unique_lines = {
            line for lines in per_term_lines.values() for line in lines
        }
        summary["focused_test_inventory"] = {
            "source_sha256": expected_hash,
            "terms": list(normalized_terms),
            "matching_lines_total": matching_lines_total,
            "included_lines": len(unique_lines),
            "included_occurrences": sum(per_term_included.values()),
            "included_bytes": len(focused_excerpt.encode("utf-8")),
            "truncated": any(
                per_term_total[term] > per_term_included[term]
                for term in normalized_terms
            ),
            "per_term_total": per_term_total,
            "per_term_included": per_term_included,
            "excerpt": focused_excerpt,
        }
    return summary


def _review_evidence_summary(
    item: Mapping[str, Any],
    task_dir: Path,
    *,
    channel: str,
    review_kind: str,
) -> Dict[str, Any]:
    evidence = item.get("evidence", item)
    if not isinstance(evidence, Mapping):
        raise legacy.HarnessError("v2 review check evidence is malformed")
    check_id = item.get("id")
    if channel not in {"planned-check", "reviewer-probe"}:
        raise legacy.HarnessError(
            f"invalid review evidence channel: {channel}"
        )
    limit = (
        NPM_LOCK_REVIEW_STREAM_BYTES
        if check_id == "npm-lock"
        else DEFAULT_REVIEW_STREAM_BYTES
    )
    test_focus_terms = (
        SPECIALIST_TEST_FOCUS_TERMS.get(review_kind, ())
        if check_id in {"rust-lib", "protocol-server"}
        else ()
    )
    request_contexts = []
    for context in item.get("request_contexts", []):
        if not isinstance(context, Mapping):
            raise legacy.HarnessError("v2 probe request context is malformed")
        request_contexts.append(
            {
                key: context.get(key)
                for key in (
                    "review_kind",
                    "review_vendor",
                    "review_result_sha256",
                    "review_prompt_sha256",
                    "rationale",
                    "proof_gaps",
                    "context_sha256",
                )
            }
        )
    return {
        "id": check_id,
        "source": channel,
        "command": item.get("command", evidence.get("command")),
        "passed": evidence.get("passed"),
        "outcome": evidence.get("outcome"),
        "exit_code": evidence.get("exit_code"),
        "duration_ms": evidence.get("duration_ms"),
        "resource_wait_ms": evidence.get("resource_wait_ms", 0),
        "log_sha256": evidence.get("log_sha256"),
        "stdout": _bounded_stream_summary(
            task_dir,
            evidence.get("stdout_path"),
            evidence.get("stdout_sha256"),
            f"review-visible {check_id} stdout",
            limit,
            test_focus_terms=test_focus_terms,
        ),
        "stderr": _bounded_stream_summary(
            task_dir,
            evidence.get("stderr_path"),
            evidence.get("stderr_sha256"),
            f"review-visible {check_id} stderr",
            limit,
        ),
        "request_contexts": request_contexts,
    }


def _bounded_source_excerpt(source: bytes, limit: int) -> Tuple[bytes, bool]:
    if len(source) <= limit:
        return source, False
    marker = b"\n... <selected source truncated by byte bound> ...\n"
    if limit <= len(marker):
        return marker[:limit], True
    available = max(0, limit - len(marker))
    head = available // 2
    tail = available - head
    prefix = source[:head].decode("utf-8", "ignore").encode("utf-8")
    excerpt = prefix + marker
    if tail:
        excerpt += source[-tail:].decode("utf-8", "ignore").encode("utf-8")
    return excerpt, True


def specialist_source_context_section(context: Mapping[str, Any]) -> str:
    return (
        SPECIALIST_SOURCE_CONTEXT_LABEL
        + json.dumps(context, sort_keys=True)
        + "\n"
    )


def _rust_raw_string_end(text: str, offset: int) -> Optional[int]:
    for prefix in ("br", "cr", "r"):
        if not text.startswith(prefix, offset):
            continue
        cursor = offset + len(prefix)
        hashes = 0
        while cursor < len(text) and text[cursor] == "#":
            hashes += 1
            cursor += 1
        if cursor >= len(text) or text[cursor] != '"':
            continue
        closing = '"' + ("#" * hashes)
        end = text.find(closing, cursor + 1)
        return len(text) if end < 0 else end + len(closing)
    return None


def _rust_char_literal_end(text: str, offset: int) -> Optional[int]:
    cursor = offset + 1
    if cursor >= len(text) or text[cursor] in {"'", "\n", "\r"}:
        return None
    if text[cursor] == "\\":
        cursor += 1
        if cursor >= len(text):
            return None
        if text[cursor] == "u" and text.startswith("u{", cursor):
            closing = text.find("}", cursor + 2)
            if closing < 0:
                return None
            cursor = closing + 1
        elif text[cursor] == "x":
            cursor += 3
        else:
            cursor += 1
    else:
        cursor += 1
    if cursor < len(text) and text[cursor] == "'":
        return cursor + 1
    return None


def _rust_braced_item_end(
    text: str, start_offset: int
) -> Tuple[Optional[int], str]:
    cursor = start_offset
    depth = 0
    opened = False
    while cursor < len(text):
        if text.startswith("//", cursor):
            newline = text.find("\n", cursor + 2)
            cursor = len(text) if newline < 0 else newline + 1
            continue
        if text.startswith("/*", cursor):
            comment_depth = 1
            cursor += 2
            while cursor < len(text) and comment_depth:
                if text.startswith("/*", cursor):
                    comment_depth += 1
                    cursor += 2
                elif text.startswith("*/", cursor):
                    comment_depth -= 1
                    cursor += 2
                else:
                    cursor += 1
            continue
        raw_end = _rust_raw_string_end(text, cursor)
        if raw_end is not None:
            cursor = raw_end
            continue
        if text[cursor] == '"':
            cursor += 1
            while cursor < len(text):
                if text[cursor] == "\\":
                    cursor += 2
                elif text[cursor] == '"':
                    cursor += 1
                    break
                else:
                    cursor += 1
            continue
        if text[cursor] == "'":
            char_end = _rust_char_literal_end(text, cursor)
            if char_end is not None:
                cursor = char_end
                continue
        if text[cursor] == "{":
            opened = True
            depth += 1
        elif text[cursor] == "}" and opened:
            depth -= 1
            if depth == 0:
                return cursor + 1, "included"
        cursor += 1
    return None, (
        "closing_brace_missing" if opened else "opening_brace_missing"
    )


def _source_context_with_excerpt(
    common: Mapping[str, Any],
    *,
    text: str,
    raw: bytes,
    start_offset: int,
    selected: bytes,
    excerpt_limit: int,
    kind: str,
    base_sha: str,
) -> Dict[str, Any]:
    excerpt, truncated = _bounded_source_excerpt(selected, excerpt_limit)
    return {
        "kind": kind,
        "source_revision": base_sha,
        "max_section_bytes": SPECIALIST_SOURCE_CONTEXT_BYTES,
        "entries": [
            {
                **common,
                "status": "included",
                "line_start": text.count("\n", 0, start_offset) + 1,
                "line_end": (
                    text.count("\n", 0, start_offset)
                    + selected.decode("utf-8").count("\n")
                    + 1
                ),
                "file_sha256": legacy.sha256_bytes(raw),
                "file_bytes": len(raw),
                "selected_sha256": legacy.sha256_bytes(selected),
                "selected_bytes": len(selected),
                "excerpt_sha256": legacy.sha256_bytes(excerpt),
                "included_bytes": len(excerpt),
                "truncated": truncated,
                "excerpt": excerpt.decode("utf-8"),
            }
        ],
    }


def specialist_source_context(
    worktree: Path,
    plan: Mapping[str, Any],
    kind: str,
) -> Dict[str, Any]:
    """Return bounded source proof for unchanged canonical trust seams.

    The exact diff remains authoritative for changed files.  Unchanged seams
    are read from the plan's immutable base commit, so prompt reconstruction
    remains stable after a clean catch-up merge.
    """

    base_sha = str(plan.get("base_sha", ""))
    changed_paths = {
        str(path) for path in plan.get("changed_paths", [])
    }
    entries: List[Dict[str, Any]] = []
    for specification in SPECIALIST_SOURCE_SEAMS.get(kind, ()):
        path = str(specification["path"])
        common = {
            "path": path,
            "symbol": str(specification["symbol"]),
            "source_revision": base_sha,
        }
        if path in changed_paths:
            entries.append(
                {
                    **common,
                    "status": "excluded_changed_path",
                    "reason": "the exact diff is authoritative for this path",
                }
            )
            continue
        completed = subprocess.run(
            ["git", "show", f"{base_sha}:{path}"],
            cwd=str(worktree),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        if completed.returncode != 0:
            entries.append({**common, "status": "missing_at_base"})
            continue
        raw = completed.stdout
        file_sha256 = legacy.sha256_bytes(raw)
        try:
            text = raw.decode("utf-8")
        except UnicodeDecodeError:
            entries.append(
                {
                    **common,
                    "status": "invalid_utf8",
                    "file_sha256": file_sha256,
                }
            )
            continue
        start = re.search(str(specification["start"]), text)
        if start is None:
            entries.append(
                {
                    **common,
                    "status": "start_anchor_missing",
                    "file_sha256": file_sha256,
                }
            )
            continue
        end_offset, end_status = _rust_braced_item_end(
            text, start.start()
        )
        if end_offset is None:
            entries.append(
                {
                    **common,
                    "status": end_status,
                    "file_sha256": file_sha256,
                }
            )
            continue
        selected_text = text[start.start() : end_offset]
        selected = selected_text.encode("utf-8")
        context = _source_context_with_excerpt(
            common,
            text=text,
            raw=raw,
            start_offset=start.start(),
            selected=selected,
            excerpt_limit=len(selected),
            kind=kind,
            base_sha=base_sha,
        )
        section_bytes = len(
            specialist_source_context_section(context).encode("utf-8")
        )
        excerpt_limit = len(selected)
        while section_bytes > SPECIALIST_SOURCE_CONTEXT_BYTES:
            overage = section_bytes - SPECIALIST_SOURCE_CONTEXT_BYTES
            excerpt_limit = max(0, excerpt_limit - overage - 128)
            context = _source_context_with_excerpt(
                common,
                text=text,
                raw=raw,
                start_offset=start.start(),
                selected=selected,
                excerpt_limit=excerpt_limit,
                kind=kind,
                base_sha=base_sha,
            )
            section_bytes = len(
                specialist_source_context_section(context).encode("utf-8")
            )
            if excerpt_limit == 0 and section_bytes > SPECIALIST_SOURCE_CONTEXT_BYTES:
                raise legacy.HarnessError(
                    "canonical risk-seam metadata exceeds its prompt byte bound"
                )
        return context
    return {
        "kind": kind,
        "source_revision": base_sha,
        "max_section_bytes": SPECIALIST_SOURCE_CONTEXT_BYTES,
        "entries": entries,
    }


def combined_review_prompt(
    contract: Mapping[str, Any],
    plan: Mapping[str, Any],
    diff: bytes,
    checks: Sequence[Mapping[str, Any]],
    kind: str,
    worktree: Path,
    task_dir: Path,
    *,
    probes: Sequence[Mapping[str, Any]] = (),
    policy_text: Optional[str] = None,
) -> str:
    if policy_text is not None:
        policy = policy_text
    elif kind == "combined":
        policy = legacy.read_prompt("combined-reviewer")
    else:
        policy = legacy.read_prompt(kind + "-reviewer")
    check_summary = [
        _review_evidence_summary(
            item, task_dir, channel="planned-check", review_kind=kind
        )
        for item in checks
    ] + [
        _review_evidence_summary(
            item, task_dir, channel="reviewer-probe", review_kind=kind
        )
        for item in probes
    ]
    eligible_probes = allowed_probe_ids(plan)
    source_context = specialist_source_context(worktree, plan, kind)
    return (
        f"{policy}\n\n"
        "## Exact v2 task\n"
        f"Task: {contract['task_id']}\n"
        f"Acceptance contract:\n{contract['description']}\n\n"
        f"Changed paths: {json.dumps(plan['changed_paths'])}\n"
        f"Claims: {json.dumps(plan['claims'])}\n"
        f"Actual sensitive risks: {json.dumps(plan['actual_risk_flags'])}\n"
        f"Diff SHA-256: {plan['diff_sha256']}\n"
        f"Plan SHA-256: {plan['plan_sha256']}\n"
        f"Protocol SHA-256: {plan['protocol_sha256']}\n"
        "Check and probe evidence (commands, hashes, bounded output, and any "
        "source proof gaps): "
        f"{json.dumps(check_summary, sort_keys=True)}\n"
        f"{specialist_source_context_section(source_context)}"
        f"Context-eligible probe IDs: {json.dumps(eligible_probes)}\n"
        "Pinned server checkout required: "
        f"{'yes' if plan.get('server_required') else 'no'}\n\n"
        "## Exact diff\n"
        f"{diff.decode('utf-8', 'replace')}\n\n"
        "Return every finding and every missing proof. PASS is forbidden when a "
        "MAJOR/BLOCKER remains. If an empirical proof is missing, use proof_gaps "
        "and optionally a typed probe_requests entry from the context-eligible "
        "IDs above; never ask for or attempt arbitrary shell access. A prior "
        "proof gap may disappear only when the exact shown probe command and "
        "output address its recorded rationale; a green but unrelated command "
        "is not evidence."
    )


def invoke_readonly_review(
    *,
    contract: Mapping[str, Any],
    plan: Mapping[str, Any],
    worktree: Path,
    task_dir: Path,
    attempt_dir: Path,
    diff: bytes,
    checks: Sequence[Mapping[str, Any]],
    review: Mapping[str, str],
    probe_evidence_sha256: str,
    probes: Sequence[Mapping[str, Any]] = (),
    allow_test_adapter: bool = False,
    sleep: Callable[[float], None] = time.sleep,
) -> Dict[str, Any]:
    vendor = review["vendor"]
    if vendor == "fake" and not allow_test_adapter:
        raise legacy.HarnessError("fake v2 reviewer is restricted to selftests")
    kind = review["kind"]
    safe_kind = re.sub(r"[^a-z0-9._-]+", "-", kind.lower())
    config = legacy.load_config()
    timeout = int(config.get("reviewer_timeout_seconds", config["agent_timeout_seconds"]))
    retry_records = attempt_dir / "review-attempts"
    retry_records.mkdir(parents=True, exist_ok=True)
    before = legacy.workspace_fingerprint(worktree)
    prompt = combined_review_prompt(
        contract,
        plan,
        diff,
        checks,
        kind,
        worktree,
        task_dir,
        probes=probes,
    )
    prompt_sha256 = legacy.sha256_bytes(prompt.encode("utf-8"))

    def one(number: int) -> Mapping[str, Any]:
        label = f"review-{safe_kind}-try-{number}"
        log_path = attempt_dir / "logs" / f"{label}-{vendor}.jsonl"
        started = time.monotonic()
        try:
            run = legacy.invoke_model(
                vendor,
                role="reviewer",
                prompt=prompt,
                schema_name="v2-review",
                worktree=worktree,
                task_dir=attempt_dir,
                label=label,
                timeout_seconds=timeout,
                instructions_sha256=plan["protocol_sha256"],
            )
            telemetry = model_telemetry(
                Path(run["log"]), timed_out=bool(run.get("timed_out"))
            )
            record: Dict[str, Any] = {
                "ok": True,
                "transient": False,
                "label": label,
                "vendor": vendor,
                "duration_ms": run.get("duration_ms"),
                "telemetry": telemetry,
                "run": run,
                "created_at": legacy.utc_now(),
            }
        except Exception as exc:  # noqa: BLE001 - classify from runner-owned process evidence
            telemetry = model_telemetry(log_path, timed_out="timeout" in str(exc).lower())
            is_transient = transient_failure(
                str(exc), telemetry, timed_out=telemetry.get("terminal_reason") == "timeout"
            )
            record = {
                "ok": False,
                "transient": is_transient,
                "label": label,
                "vendor": vendor,
                "duration_ms": int((time.monotonic() - started) * 1000),
                "telemetry": telemetry,
                "retry_after_seconds": telemetry.get("retry_after_seconds"),
                "error": f"{type(exc).__name__}: {exc}",
                "created_at": legacy.utc_now(),
            }
        legacy.atomic_write_json(retry_records / f"{label}.json", record)
        return record

    try:
        successful, attempts = retry_call(
            one,
            sleep=sleep,
            max_retry_delay_seconds=float(
                config.get("review_retry_max_delay_seconds", 60)
            ),
        )
    except ReviewPaused as exc:
        exc.attempts = [
            {
                key: value
                for key, value in item.items()
                if key != "run"
            }
            for item in exc.attempts
        ]
        raise
    if legacy.workspace_fingerprint(worktree) != before:
        raise legacy.HarnessError(
            f"{kind} review changed the worktree; reviewer sandbox must remain read-only"
        )
    run = dict(successful["run"])
    if run.get("prompt_sha256") != prompt_sha256:
        raise legacy.HarnessError(
            f"{kind} review invocation prompt hash differs from runner input"
        )
    result = run["result"]
    legacy.validate_schema(
        result, legacy.load_schema("v2-review"), label=f"{kind} v2 review"
    )
    # invoke_model creates every result/invocation/log with a fresh UUID and
    # exclusive-create semantics.  The per-kind review checkpoint may advance
    # on resume, but these source artifacts remain immutable for probe binding.
    record = {
        "schema_version": 2,
        **evidence_binding(plan),
        "kind": kind,
        "vendor": vendor,
        "label": successful["label"],
        "result": result,
        "result_path": run["result_path"],
        "result_sha256": run["artifact_sha256"],
        "invocation_path": run["invocation_path"],
        "invocation_sha256": run["invocation_sha256"],
        "log_path": run["log"],
        "log_sha256": run["log_sha256"],
        "session_id": run["session_id"],
        "model": run["model"],
        "cli_version": run["cli_version"],
        "duration_ms": run.get("duration_ms"),
        "execution_id": run.get("execution_id"),
        "guardian_path": run.get("guardian_path"),
        "guardian_sha256": run.get("guardian_sha256"),
        "leader_exited_with_live_group": bool(
            run.get("leader_exited_with_live_group")
        ),
        "termination_reason": run.get("termination_reason"),
        "telemetry": successful.get("telemetry", {}),
        "probe_evidence_sha256": probe_evidence_sha256,
        "prompt_sha256": run["prompt_sha256"],
        "attempts": [
            {
                key: value
                for key, value in item.items()
                if key != "run"
            }
            for item in attempts
        ],
        "created_at": legacy.utc_now(),
    }
    return record


def check_record(
    check: Mapping[str, Any], plan: Mapping[str, Any], evidence: Mapping[str, Any]
) -> Dict[str, Any]:
    return {
        "schema_version": 2,
        **evidence_binding(plan),
        "id": check["id"],
        "command": check["command"],
        "evidence": dict(evidence),
        "created_at": legacy.utc_now(),
    }


def canonical_probe_request_contexts(
    contexts: Sequence[Mapping[str, Any]],
) -> List[Dict[str, Any]]:
    """Deduplicate and order bound probe requests in one canonical way."""

    unique = {
        str(context.get("context_sha256")): copy.deepcopy(dict(context))
        for context in contexts
    }
    return sorted(
        unique.values(),
        key=lambda item: (
            str(item.get("review_kind")),
            str(item.get("review_vendor")),
            str(item.get("context_sha256")),
        ),
    )


def probe_request_contexts(
    reviews: Sequence[Mapping[str, Any]], probe_id: str
) -> List[Dict[str, Any]]:
    """Bind a probe to the exact review claim that requested it."""

    contexts: Dict[str, Dict[str, Any]] = {}
    for review in reviews:
        result = review.get("result", {})
        if not isinstance(result, Mapping):
            raise legacy.HarnessError("v2 source review result is malformed")
        proof_gaps = copy.deepcopy(list(result.get("proof_gaps", [])))
        for request in result.get("probe_requests", []):
            if not isinstance(request, Mapping) or request.get("probe_id") != probe_id:
                continue
            context: Dict[str, Any] = {
                "probe_id": probe_id,
                "review_kind": review.get("kind"),
                "review_vendor": review.get("vendor"),
                "review_result_path": review.get("result_path"),
                "review_result_sha256": review.get("result_sha256"),
                "review_prompt_sha256": review.get("prompt_sha256"),
                "rationale": request.get("rationale"),
                "proof_gaps": proof_gaps,
                "source_review": copy.deepcopy(dict(review)),
                "context_sha256": "",
            }
            context["context_sha256"] = document_hash(
                context, "context_sha256"
            )
            contexts[context["context_sha256"]] = context
    return canonical_probe_request_contexts(list(contexts.values()))


def aggregate_verdict(
    checks: Sequence[Mapping[str, Any]], reviews: Sequence[Mapping[str, Any]]
) -> Tuple[str, str]:
    if len(checks) == 0 and len(reviews) == 0:
        return "NEEDS_EVIDENCE", "no check or review evidence exists"
    for check in checks:
        evidence = check.get("evidence", {})
        if evidence.get("outcome") == "BLOCKED" or evidence.get("timed_out"):
            return "PAUSED_RETRYABLE", f"check {check.get('id')} is retryable"
        if not evidence.get("passed"):
            return "NEEDS_FIX", f"check {check.get('id')} failed"
    review_states = [review_result_state(review.get("result", {})) for review in reviews]
    if "NEEDS_FIX" in review_states:
        return "NEEDS_FIX", "a review has unresolved FAIL/MAJOR/BLOCKER findings"
    if "NEEDS_EVIDENCE" in review_states:
        return "NEEDS_EVIDENCE", "a review has unresolved proof gaps or missing evidence"
    return "PASSED", "all planned checks and reviews passed"


def aggregate_review_outcomes(
    reviews: Sequence[Mapping[str, Any]],
) -> Dict[str, List[Dict[str, Any]]]:
    """Derive the receipt-level review summary from the bound review records."""

    return {
        "findings": [
            {"review": review["kind"], **finding}
            for review in reviews
            for finding in review.get("result", {}).get("findings", [])
        ],
        "proof_gaps": [
            {"review": review["kind"], **gap}
            for review in reviews
            for gap in review.get("result", {}).get("proof_gaps", [])
        ],
        "probe_requests": [
            {"review": review["kind"], **probe}
            for review in reviews
            for probe in review.get("result", {}).get("probe_requests", [])
        ],
    }


def build_evidence(
    contract: Mapping[str, Any],
    plan: Mapping[str, Any],
    worktree: Path,
    checks: Sequence[Mapping[str, Any]],
    probes: Sequence[Mapping[str, Any]],
    reviews: Sequence[Mapping[str, Any]],
) -> Dict[str, Any]:
    review_outcomes = aggregate_review_outcomes(reviews)
    verdict, reason = aggregate_verdict([*checks, *probes], reviews)
    evidence: Dict[str, Any] = {
        "schema_version": 2,
        "task_id": contract["task_id"],
        "contract_sha256": contract["contract_sha256"],
        **evidence_binding(plan),
        "parent_sha": legacy.git(worktree, "rev-parse", "HEAD"),
        "tree_sha": plan["tree_sha"],
        "changed_paths": list(plan["changed_paths"]),
        "actual_risk_flags": list(plan["actual_risk_flags"]),
        "claims": list(plan["claims"]),
        "checks": [dict(item) for item in checks],
        "probes": [dict(item) for item in probes],
        "reviews": [dict(item) for item in reviews],
        **review_outcomes,
        "degraded_provenance": list(contract.get("degraded_provenance", [])),
        "telemetry": {
            "resource_wait_ms": sum(
                int(record.get("evidence", {}).get("resource_wait_ms", 0) or 0)
                for record in [*checks, *probes]
            ),
            "review_attempts": sum(
                len(review.get("attempts", [])) for review in reviews
            ),
            "review_transient_failures": sum(
                1
                for review in reviews
                for attempt in review.get("attempts", [])
                if attempt.get("transient")
            ),
            "review_terminal_reasons": sorted(
                {
                    str(attempt.get("telemetry", {}).get("terminal_reason"))
                    for review in reviews
                    for attempt in review.get("attempts", [])
                    if attempt.get("telemetry", {}).get("terminal_reason")
                }
            ),
        },
        "verdict": verdict,
        "reason": reason,
        "created_at": legacy.utc_now(),
        "evidence_sha256": "",
    }
    evidence["evidence_sha256"] = document_hash(evidence, "evidence_sha256")
    legacy.validate_schema(
        evidence, legacy.load_schema("v2-evidence"), label="v2 evidence"
    )
    return evidence


def _artifact_inside(task_dir: Path, raw: Any, expected_hash: Any, label: str) -> Path:
    if not isinstance(raw, str) or not raw:
        raise legacy.HarnessError(f"{label} path is missing")
    path = Path(raw)
    try:
        resolved = path.resolve(strict=True)
        resolved.relative_to(task_dir.resolve())
    except (FileNotFoundError, OSError, ValueError) as exc:
        raise legacy.HarnessError(f"{label} is missing or outside the v2 task store") from exc
    metadata = path.lstat()
    if not path.is_file() or path.is_symlink() or metadata.st_nlink != 1:
        raise legacy.HarnessError(f"{label} is not a single-link regular evidence file")
    if legacy.sha256_file(path) != expected_hash:
        raise legacy.HarnessError(f"{label} hash changed")
    return path


def validate_check_checkpoint(
    record: Mapping[str, Any],
    declared: Mapping[str, Any],
    plan: Mapping[str, Any],
    task_dir: Path,
) -> None:
    if not binding_matches(record, plan):
        raise legacy.HarnessError("v2 check checkpoint binding is stale")
    if record.get("id") != declared.get("id"):
        raise legacy.HarnessError("v2 check checkpoint id changed")
    if record.get("command") != declared.get("command"):
        raise legacy.HarnessError("v2 check checkpoint command changed")
    result = record.get("evidence")
    if not isinstance(result, Mapping):
        raise legacy.HarnessError("v2 check checkpoint evidence is malformed")
    if result.get("id") != declared.get("id"):
        raise legacy.HarnessError("v2 check result id changed")
    if result.get("command") != declared.get("command"):
        raise legacy.HarnessError("v2 check result command changed")
    expected_bound_environment = (
        {"MURMUR_HARNESS_BASE_SHA": str(plan["base_sha"])}
        if declared.get("id") == "npm-lock"
        else {}
    )
    if (
        result.get("bound_environment", {})
        != expected_bound_environment
    ):
        raise legacy.HarnessError(
            "v2 check runner-bound environment changed"
        )
    log_path = _artifact_inside(
        task_dir,
        result.get("log_path"),
        result.get("log_sha256"),
        f"check {declared.get('id')} log",
    )
    expected_stdout = log_path.with_suffix(".stdout.log")
    expected_stderr = log_path.with_suffix(".stderr.log")
    if result.get("stdout_path") != str(expected_stdout):
        raise legacy.HarnessError("v2 check stdout path is not bound to its log")
    if result.get("stderr_path") != str(expected_stderr):
        raise legacy.HarnessError("v2 check stderr path is not bound to its log")
    _artifact_inside(
        task_dir,
        str(expected_stdout),
        result.get("stdout_sha256"),
        f"check {declared.get('id')} stdout",
    )
    _artifact_inside(
        task_dir,
        str(expected_stderr),
        result.get("stderr_sha256"),
        f"check {declared.get('id')} stderr",
    )
    _artifact_inside(
        task_dir,
        result.get("sandbox_profile_path"),
        result.get("sandbox_profile_sha256"),
        f"check {declared.get('id')} sandbox",
    )
    _artifact_inside(
        task_dir,
        result.get("guardian_path"),
        result.get("guardian_sha256"),
        f"check {declared.get('id')} guardian",
    )
    if result.get("leader_exited_with_live_group"):
        raise legacy.HarnessError(
            f"v2 check {declared.get('id')} left a live process group"
        )


def validate_probe_checkpoint(
    record: Mapping[str, Any],
    declared: Mapping[str, Any],
    plan: Mapping[str, Any],
    task_dir: Path,
    *,
    allow_test_adapter: bool = False,
    review_schema: Optional[Mapping[str, Any]] = None,
) -> None:
    validate_check_checkpoint(record, declared, plan, task_dir)
    probe_id = record.get("id")
    if probe_id not in allowed_probe_ids(plan):
        raise legacy.HarnessError(
            f"v2 probe {probe_id} is outside the exact plan"
        )
    if record.get("source") != "reviewer-probe":
        raise legacy.HarnessError("v2 probe has no reviewer-probe provenance")
    execution_number = record.get("execution_number", 1)
    if (
        isinstance(execution_number, bool)
        or not isinstance(execution_number, int)
        or execution_number < 1
        or execution_number > MAX_PROBE_EXECUTIONS_PER_ID
    ):
        raise legacy.HarnessError(
            "v2 probe execution number is malformed or exceeds its bound"
        )
    contexts = record.get("request_contexts")
    if not isinstance(contexts, list) or not contexts:
        raise legacy.HarnessError("v2 probe request provenance is missing")
    required_keys = {
        "probe_id",
        "review_kind",
        "review_vendor",
        "review_result_path",
        "review_result_sha256",
        "review_prompt_sha256",
        "rationale",
        "proof_gaps",
        "source_review",
        "context_sha256",
    }
    expected_reviews = {
        (item.get("kind"), item.get("vendor"))
        for item in plan.get("reviews", [])
        if isinstance(item, Mapping)
    }
    seen: set[str] = set()
    for context in contexts:
        if not isinstance(context, Mapping) or set(context) != required_keys:
            raise legacy.HarnessError("v2 probe request context is malformed")
        if context.get("probe_id") != probe_id:
            raise legacy.HarnessError("v2 probe request context id changed")
        if (
            context.get("review_kind"),
            context.get("review_vendor"),
        ) not in expected_reviews:
            raise legacy.HarnessError(
                "v2 probe request context review is outside the plan"
            )
        context_hash = context.get("context_sha256")
        if (
            not isinstance(context_hash, str)
            or context_hash in seen
            or context_hash != document_hash(context, "context_sha256")
        ):
            raise legacy.HarnessError(
                "v2 probe request context hash is stale or duplicated"
            )
        seen.add(context_hash)
        if not isinstance(context.get("rationale"), str):
            raise legacy.HarnessError("v2 probe request rationale is malformed")
        proof_gaps = context.get("proof_gaps")
        if not isinstance(proof_gaps, list):
            raise legacy.HarnessError("v2 probe source proof gaps are malformed")
        source_review = context.get("source_review")
        if not isinstance(source_review, Mapping):
            raise legacy.HarnessError(
                "v2 probe source review checkpoint is malformed"
            )
        source_fields = {
            "review_kind": "kind",
            "review_vendor": "vendor",
            "review_result_path": "result_path",
            "review_result_sha256": "result_sha256",
            "review_prompt_sha256": "prompt_sha256",
        }
        for context_key, review_key in source_fields.items():
            if context.get(context_key) != source_review.get(review_key):
                raise legacy.HarnessError(
                    "v2 probe source review checkpoint metadata changed"
                )
        source_result_summary = source_review.get("result")
        if not isinstance(source_result_summary, Mapping):
            raise legacy.HarnessError(
                "v2 probe source review result summary is malformed"
            )
        if source_result_summary.get("proof_gaps") != proof_gaps:
            raise legacy.HarnessError(
                "v2 probe source proof gaps differ from its checkpoint"
            )
        expected_request = {
            "probe_id": probe_id,
            "rationale": context.get("rationale"),
        }
        if expected_request not in source_result_summary.get(
            "probe_requests", []
        ):
            raise legacy.HarnessError(
                "v2 probe request differs from its source review checkpoint"
            )
        source_declarations = [
            item
            for item in plan.get("reviews", [])
            if isinstance(item, Mapping)
            and item.get("kind") == context.get("review_kind")
            and item.get("vendor") == context.get("review_vendor")
        ]
        if len(source_declarations) != 1:
            raise legacy.HarnessError(
                "v2 probe source review is not uniquely declared in the plan"
            )
        prompt_hash = context.get("review_prompt_sha256")
        if not isinstance(prompt_hash, str) or not re.fullmatch(
            r"[0-9a-f]{64}", prompt_hash
        ):
            raise legacy.HarnessError(
                "v2 probe source review prompt hash is malformed"
            )
        validate_review_checkpoint(
            source_review,
            source_declarations[0],
            plan,
            task_dir,
            expected_prompt_sha256=prompt_hash,
            allow_test_adapter=allow_test_adapter,
            review_schema=review_schema,
        )
        source_path = _artifact_inside(
            task_dir,
            context.get("review_result_path"),
            context.get("review_result_sha256"),
            f"probe {probe_id} source review result",
        )
        source_result = legacy.load_json(source_path)
        legacy.validate_schema(
            source_result,
            (
                dict(review_schema)
                if review_schema is not None
                else legacy.load_schema("v2-review")
            ),
            label=f"probe {probe_id} source review",
        )
        if source_result.get("proof_gaps") != proof_gaps:
            raise legacy.HarnessError(
                "v2 probe source proof gaps differ from the review artifact"
            )
        if expected_request not in source_result.get("probe_requests", []):
            raise legacy.HarnessError(
                "v2 probe request differs from the source review artifact"
            )
    if list(contexts) != canonical_probe_request_contexts(contexts):
        raise legacy.HarnessError("v2 probe request contexts are not canonical")


def validate_review_checkpoint(
    record: Mapping[str, Any],
    declared: Mapping[str, Any],
    plan: Mapping[str, Any],
    task_dir: Path,
    *,
    expected_prompt_sha256: str,
    allow_test_adapter: bool,
    review_schema: Optional[Mapping[str, Any]] = None,
) -> None:
    if not binding_matches(record, plan):
        raise legacy.HarnessError("v2 review checkpoint binding is stale")
    if record.get("kind") != declared.get("kind"):
        raise legacy.HarnessError("v2 review checkpoint kind changed")
    if record.get("vendor") != declared.get("vendor"):
        raise legacy.HarnessError("v2 review checkpoint vendor changed")
    if record.get("prompt_sha256") != expected_prompt_sha256:
        raise legacy.HarnessError("v2 review checkpoint prompt hash changed")
    if record.get("vendor") == "fake" and not allow_test_adapter:
        raise legacy.HarnessError("fake v2 review checkpoint is forbidden")
    label = record.get("label")
    vendor = record.get("vendor")
    if not isinstance(label, str) or not isinstance(vendor, str):
        raise legacy.HarnessError("v2 review checkpoint label/vendor is malformed")
    safe_kind = re.sub(
        r"[^a-z0-9._-]+", "-", str(declared.get("kind", "")).lower()
    )
    if re.fullmatch(
        rf"review-{re.escape(safe_kind)}-try-[12]", label
    ) is None:
        raise legacy.HarnessError(
            "v2 review checkpoint label does not match review kind"
        )
    raw_invocation_path = Path(str(record.get("invocation_path", "")))
    run_dir = raw_invocation_path.parent.parent
    expected_result = run_dir / "results" / f"{label}-{vendor}.json"
    expected_invocation = (
        run_dir / "results" / f"{label}-{vendor}-invocation.json"
    )
    expected_log = run_dir / "logs" / f"{label}-{vendor}.jsonl"

    result_path = legacy.evidence_file(
        task_dir,
        record.get("result_path"),
        expected_result,
        f"review {declared.get('kind')} result",
    )
    if legacy.sha256_file(result_path) != record.get("result_sha256"):
        raise legacy.HarnessError("v2 review checkpoint result hash changed")
    result = legacy.load_json(result_path)
    legacy.validate_schema(
        result,
        (
            dict(review_schema)
            if review_schema is not None
            else legacy.load_schema("v2-review")
        ),
        label=f"review {declared.get('kind')}",
    )
    if result != record.get("result"):
        raise legacy.HarnessError("v2 review checkpoint result summary changed")
    invocation_path = legacy.evidence_file(
        task_dir,
        record.get("invocation_path"),
        expected_invocation,
        f"review {declared.get('kind')} invocation",
    )
    if legacy.sha256_file(invocation_path) != record.get("invocation_sha256"):
        raise legacy.HarnessError("v2 review checkpoint invocation hash changed")
    invocation = legacy.load_json(invocation_path)
    if invocation.get("prompt_sha256") != expected_prompt_sha256:
        raise legacy.HarnessError(
            "v2 review invocation prompt hash changed"
        )
    log_path = legacy.evidence_file(
        task_dir,
        record.get("log_path"),
        expected_log,
        f"review {declared.get('kind')} log",
    )
    if legacy.sha256_file(log_path) != record.get("log_sha256"):
        raise legacy.HarnessError("v2 review checkpoint log hash changed")
    execution_ids = {
        legacy._artifact_execution_id(result_path, expected_result),
        legacy._artifact_execution_id(invocation_path, expected_invocation),
        legacy._artifact_execution_id(log_path, expected_log),
    }
    if (
        len(execution_ids) != 1
        or None in execution_ids
        or record.get("execution_id") not in execution_ids
    ):
        raise legacy.HarnessError(
            "v2 review artifacts do not share one execution id"
        )
    if record.get("vendor") != "fake":
        _artifact_inside(
            task_dir,
            record.get("guardian_path"),
            record.get("guardian_sha256"),
            f"review {declared.get('kind')} guardian",
        )
        if record.get("leader_exited_with_live_group"):
            raise legacy.HarnessError(
                f"v2 review {declared.get('kind')} left a live process group"
            )


def verify_v2_evidence(
    contract: Mapping[str, Any],
    task_dir: Path,
    *,
    allow_test_adapter: bool = False,
    allow_committed_head: bool = False,
    attested_commit_sha: Optional[str] = None,
) -> Dict[str, Any]:
    """Fail closed unless current bytes exactly match a complete v2 PASS."""

    worktree = Path(str(contract["worktree_path"]))
    attested_schemas: Dict[str, Dict[str, Any]] = {}
    if attested_commit_sha is not None:
        if not legacy.SHA1_RE.fullmatch(attested_commit_sha):
            raise legacy.HarnessError("v2 attested commit is malformed")
        attested_schemas = {
            name: attested_schema(worktree, attested_commit_sha, name)
            for name in ("v2-task", "v2-plan", "v2-review", "v2-evidence")
        }
    validate_hashed_document(
        contract,
        "v2-task",
        "contract_sha256",
        "v2 task",
        schema=attested_schemas.get("v2-task"),
    )
    state = load_v2_state(task_dir)
    if state.get("status") not in {"PASSED", "COMMITTED"}:
        raise legacy.HarnessError(
            f"v2 receipt requires PASSED/COMMITTED state; found {state.get('status')}"
        )
    if not worktree.is_dir() or worktree.is_symlink():
        raise legacy.HarnessError("v2 task worktree is missing or unsafe")
    if Path(legacy.git(worktree, "rev-parse", "--show-toplevel")).resolve() != worktree.resolve():
        raise legacy.HarnessError("v2 worktree path is not its Git root")
    if legacy.git(worktree, "branch", "--show-current") != contract["branch"]:
        raise legacy.HarnessError("v2 task branch changed")
    current_head = legacy.git(worktree, "rev-parse", "HEAD")
    if attested_commit_sha is not None:
        parents = legacy.git(
            worktree, "show", "-s", "--format=%P", attested_commit_sha
        ).split()
        if parents != [contract["base_sha"]]:
            raise legacy.HarnessError(
                "v2 attested commit is not the exact child of task base"
            )
        if legacy.git_bytes(worktree, "status", "--porcelain").strip():
            raise legacy.HarnessError(
                "v2 committed evidence requires a clean worktree/index"
            )
        evidence_parent = contract["base_sha"]
        encoded_paths = legacy.git_bytes(
            worktree,
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            evidence_parent,
            attested_commit_sha,
            "--",
        )
        paths = sorted(
            item.decode("utf-8", "surrogateescape")
            for item in encoded_paths.split(b"\x00")
            if item
        )
        diff = legacy.git_bytes(
            worktree,
            "diff",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-renames",
            evidence_parent,
            attested_commit_sha,
            "--",
        )
        tree_sha = legacy.git(
            worktree, "rev-parse", f"{attested_commit_sha}^{{tree}}"
        )
        violations = [
            path
            for path in paths
            if not legacy.path_is_owned(path, contract["owned_paths"])
        ]
        if violations:
            raise legacy.HarnessError(
                "out-of-scope paths in v2 attested commit: "
                + ", ".join(violations)
            )
    elif current_head == contract["base_sha"]:
        paths, diff, tree_sha = snapshot_scoped_diff(worktree, contract, task_dir)
        evidence_parent = current_head
    elif allow_committed_head:
        evidence_parent = legacy.git(worktree, "rev-parse", "HEAD^")
        if evidence_parent != contract["base_sha"]:
            raise legacy.HarnessError(
                "v2 recovery commit is not the single exact child of task base"
            )
        if legacy.git_bytes(worktree, "status", "--porcelain").strip():
            raise legacy.HarnessError(
                "v2 recovery commit worktree/index is not clean"
            )
        encoded_paths = legacy.git_bytes(
            worktree,
            "diff",
            "--name-only",
            "-z",
            "--no-renames",
            evidence_parent,
            current_head,
            "--",
        )
        paths = sorted(
            item.decode("utf-8", "surrogateescape")
            for item in encoded_paths.split(b"\x00")
            if item
        )
        diff = legacy.git_bytes(
            worktree,
            "diff",
            "--binary",
            "--full-index",
            "--no-ext-diff",
            "--no-renames",
            evidence_parent,
            current_head,
            "--",
        )
        tree_sha = legacy.git(worktree, "rev-parse", "HEAD^{tree}")
        violations = [
            path
            for path in paths
            if not legacy.path_is_owned(path, contract["owned_paths"])
        ]
        if violations:
            raise legacy.HarnessError(
                "out-of-scope paths in v2 recovery commit: "
                + ", ".join(violations)
            )
    else:
        raise legacy.HarnessError(
            "v2 task HEAD changed after PASS without an active commit recovery"
        )

    plan_path = Path(str(state.get("plan_path", "")))
    try:
        plan_path.resolve(strict=True).relative_to(task_dir.resolve())
    except (FileNotFoundError, OSError, ValueError) as exc:
        raise legacy.HarnessError("v2 plan path escapes the task store") from exc
    plan = legacy.load_json(plan_path)
    validate_hashed_document(
        plan,
        "v2-plan",
        "plan_sha256",
        "v2 plan",
        schema=attested_schemas.get("v2-plan"),
    )
    if plan.get("changed_paths") != paths:
        raise legacy.HarnessError("v2 plan changed paths are stale")
    if plan.get("diff_sha256") != legacy.sha256_bytes(diff):
        raise legacy.HarnessError("v2 plan diff is stale")
    if plan.get("tree_sha") != tree_sha:
        raise legacy.HarnessError("v2 plan tree is stale")
    protocol_path = plan_path.parent / "protocol.json"
    try:
        protocol_path.resolve(strict=True).relative_to(task_dir.resolve())
    except (FileNotFoundError, OSError, ValueError) as exc:
        raise legacy.HarnessError(
            "v2 protocol bundle path escapes the task store"
        ) from exc
    bundle = legacy.load_json(protocol_path)
    if (
        bundle.get("schema_version") != 2
        or bundle.get("protocol_sha256")
        != canonical_hash(
            {
                key: value
                for key, value in bundle.items()
                if key != "protocol_sha256"
            }
        )
        or not isinstance(bundle.get("files"), list)
    ):
        raise legacy.HarnessError("v2 recorded protocol bundle is malformed")
    if attested_commit_sha is None:
        if bundle != executable_protocol_bundle(worktree):
            raise legacy.HarnessError(
                "v2 recorded protocol bundle differs from executable bytes"
            )
    else:
        seen_protocol_paths: set[str] = set()
        for item in bundle["files"]:
            if (
                not isinstance(item, Mapping)
                or not isinstance(item.get("path"), str)
                or not isinstance(item.get("sha256"), str)
                or item["path"] in seen_protocol_paths
            ):
                raise legacy.HarnessError(
                    "v2 recorded protocol file entry is malformed"
                )
            seen_protocol_paths.add(item["path"])
            committed_bytes = git_file_at_commit(
                worktree, attested_commit_sha, item["path"]
            )
            if legacy.sha256_bytes(committed_bytes) != item["sha256"]:
                raise legacy.HarnessError(
                    f"v2 attested protocol byte changed: {item['path']}"
                )
    for key, value in (
        ("task_id", contract["task_id"]),
        ("contract_sha256", contract["contract_sha256"]),
        ("base_sha", contract["base_sha"]),
        ("claims", list(contract.get("claims", []))),
        (
            "server_required",
            any(
                check["id"] in {"rust-lib", "protocol-server"}
                for check in plan.get("checks", [])
            ),
        ),
        ("created_at", contract["created_at"]),
    ):
        if plan.get(key) != value:
            raise legacy.HarnessError(f"v2 plan {key} differs from its contract")
    if plan.get("protocol_sha256") != bundle["protocol_sha256"]:
        raise legacy.HarnessError("v2 plan protocol bundle is stale")
    if attested_commit_sha is None:
        profile_config = legacy.load_config()
    else:
        profile_config = attested_json_object(
            worktree,
            attested_commit_sha,
            ".agents/harness/config.json",
            "harness config",
        )
    expected_checks, expected_reviews, expected_risks = derive_profile(
        paths,
        list(contract.get("claims", [])),
        profile_config,
        reviewer=str(contract["reviewer"]),
        allow_same_vendor_high_risk=bool(
            contract.get("allow_same_vendor_high_risk", False)
        ),
    )
    if plan.get("checks") != expected_checks:
        raise legacy.HarnessError("v2 plan check profile is stale")
    if plan.get("reviews") != expected_reviews:
        raise legacy.HarnessError("v2 plan review profile is stale")
    if plan.get("actual_risk_flags") != expected_risks:
        raise legacy.HarnessError("v2 plan sensitive-risk profile is stale")

    evidence_path = Path(str(state.get("evidence_path", "")))
    try:
        evidence_path.resolve(strict=True).relative_to(task_dir.resolve())
    except (FileNotFoundError, OSError, ValueError) as exc:
        raise legacy.HarnessError("v2 evidence path escapes the task store") from exc
    evidence = legacy.load_json(evidence_path)
    validate_hashed_document(
        evidence,
        "v2-evidence",
        "evidence_sha256",
        "v2 evidence",
        schema=attested_schemas.get("v2-evidence"),
    )
    for key, value in (
        ("task_id", contract["task_id"]),
        ("contract_sha256", contract["contract_sha256"]),
        ("diff_sha256", plan["diff_sha256"]),
        ("plan_sha256", plan["plan_sha256"]),
        ("protocol_sha256", plan["protocol_sha256"]),
        ("parent_sha", evidence_parent),
        ("tree_sha", tree_sha),
        ("changed_paths", paths),
        ("actual_risk_flags", expected_risks),
        ("claims", list(plan["claims"])),
        (
            "degraded_provenance",
            list(contract.get("degraded_provenance", [])),
        ),
    ):
        if evidence.get(key) != value:
            raise legacy.HarnessError(f"v2 evidence {key} is stale")
    if evidence.get("verdict") != "PASSED":
        raise legacy.HarnessError("v2 evidence verdict is not PASSED")

    check_records = evidence.get("checks", [])
    if [item.get("id") for item in check_records] != [
        item["id"] for item in expected_checks
    ]:
        raise legacy.HarnessError("v2 evidence check set/order differs from plan")
    for record, declared in zip(check_records, expected_checks):
        validate_check_checkpoint(record, declared, plan, task_dir)
        result = record.get("evidence", {})
        if not result.get("passed") or result.get("exit_code") != 0:
            raise legacy.HarnessError(f"v2 check {declared['id']} is not green")
        _artifact_inside(
            task_dir, result.get("log_path"), result.get("log_sha256"), f"check {declared['id']} log"
        )
        log_path = Path(str(result.get("log_path")))
        _artifact_inside(
            task_dir,
            str(log_path.with_suffix(".stdout.log")),
            result.get("stdout_sha256"),
            f"check {declared['id']} stdout",
        )
        _artifact_inside(
            task_dir,
            str(log_path.with_suffix(".stderr.log")),
            result.get("stderr_sha256"),
            f"check {declared['id']} stderr",
        )
        _artifact_inside(
            task_dir,
            result.get("sandbox_profile_path"),
            result.get("sandbox_profile_sha256"),
            f"check {declared['id']} sandbox",
        )

    probe_records = evidence.get("probes", [])
    probe_ids: set = set()
    eligible_probe_ids = set(allowed_probe_ids(plan))
    for record in probe_records:
        probe_id = record.get("id")
        if probe_id not in eligible_probe_ids or probe_id in probe_ids:
            raise legacy.HarnessError(
                "v2 probe evidence has a duplicate/id outside the exact plan"
            )
        probe_ids.add(probe_id)
        if not binding_matches(record, plan):
            raise legacy.HarnessError(f"v2 probe {probe_id} binding is stale")
        declared = canonical_check(str(probe_id), profile_config)
        validate_probe_checkpoint(
            record,
            declared,
            plan,
            task_dir,
            allow_test_adapter=allow_test_adapter,
            review_schema=attested_schemas.get("v2-review"),
        )
        result = record.get("evidence", {})
        if not result.get("passed") or result.get("exit_code") != 0:
            raise legacy.HarnessError(f"v2 probe {probe_id} is not green")
        log_path = _artifact_inside(
            task_dir,
            result.get("log_path"),
            result.get("log_sha256"),
            f"probe {probe_id} log",
        )
        _artifact_inside(
            task_dir,
            str(log_path.with_suffix(".stdout.log")),
            result.get("stdout_sha256"),
            f"probe {probe_id} stdout",
        )
        _artifact_inside(
            task_dir,
            str(log_path.with_suffix(".stderr.log")),
            result.get("stderr_sha256"),
            f"probe {probe_id} stderr",
        )
        _artifact_inside(
            task_dir,
            result.get("sandbox_profile_path"),
            result.get("sandbox_profile_sha256"),
            f"probe {probe_id} sandbox",
        )
    expected_probe_hash = probe_evidence_hash(probe_records)

    review_records = evidence.get("reviews", [])
    if [(item.get("kind"), item.get("vendor")) for item in review_records] != [
        (item["kind"], item["vendor"]) for item in expected_reviews
    ]:
        raise legacy.HarnessError("v2 evidence review set/order differs from plan")
    sessions: set = set()
    for record, declared in zip(review_records, expected_reviews):
        if not binding_matches(record, plan):
            raise legacy.HarnessError(f"v2 review {declared['kind']} binding is stale")
        if record.get("vendor") != declared["vendor"]:
            raise legacy.HarnessError(f"v2 review {declared['kind']} vendor changed")
        if record.get("probe_evidence_sha256") != expected_probe_hash:
            raise legacy.HarnessError(
                f"v2 review {declared['kind']} did not see the recorded probe evidence"
            )
        if attested_commit_sha is None:
            policy_text = None
        else:
            policy_name = (
                "combined-reviewer"
                if declared["kind"] == "combined"
                else str(declared["kind"]) + "-reviewer"
            )
            try:
                policy_text = git_file_at_commit(
                    worktree,
                    attested_commit_sha,
                    f".agents/harness/prompts/{policy_name}.md",
                ).decode("utf-8")
            except UnicodeDecodeError as exc:
                raise legacy.HarnessError(
                    f"v2 attested review policy is not UTF-8: {policy_name}"
                ) from exc
        expected_prompt = combined_review_prompt(
            contract,
            plan,
            diff,
            check_records,
            str(declared["kind"]),
            worktree,
            task_dir,
            probes=probe_records,
            policy_text=policy_text,
        )
        validate_review_checkpoint(
            record,
            declared,
            plan,
            task_dir,
            expected_prompt_sha256=legacy.sha256_bytes(
                expected_prompt.encode("utf-8")
            ),
            allow_test_adapter=allow_test_adapter,
            review_schema=attested_schemas.get("v2-review"),
        )
        if record.get("vendor") == "fake" and not allow_test_adapter:
            raise legacy.HarnessError("fake v2 evidence is forbidden outside selftests")
        session = record.get("session_id")
        if not isinstance(session, str) or not session or session in sessions:
            raise legacy.HarnessError(
                f"v2 review {declared['kind']} has missing/reused session provenance"
            )
        sessions.add(session)
        result_path = _artifact_inside(
            task_dir,
            record.get("result_path"),
            record.get("result_sha256"),
            f"review {declared['kind']} result",
        )
        result = legacy.load_json(result_path)
        legacy.validate_schema(
            result,
            (
                attested_schemas["v2-review"]
                if attested_commit_sha is not None
                else legacy.load_schema("v2-review")
            ),
            label=f"review {declared['kind']}",
        )
        if result != record.get("result"):
            raise legacy.HarnessError(
                f"v2 review {declared['kind']} result summary changed"
            )
        result_state = review_result_state(result)
        if result_state != "PASSED":
            if any(
                finding.get("severity") in SEVERE_FINDINGS
                for finding in result.get("findings", [])
            ):
                raise legacy.HarnessError(
                    "v2 PASS contains unresolved MAJOR/BLOCKER findings"
                )
            if result.get("proof_gaps") or result.get("probe_requests"):
                raise legacy.HarnessError(
                    "v2 PASS contains unresolved proof gaps"
                )
            raise legacy.HarnessError(
                f"v2 review {declared['kind']} is not an admissible PASS"
            )
        invocation_path = _artifact_inside(
            task_dir,
            record.get("invocation_path"),
            record.get("invocation_sha256"),
            f"review {declared['kind']} invocation",
        )
        log_path = _artifact_inside(
            task_dir,
            record.get("log_path"),
            record.get("log_sha256"),
            f"review {declared['kind']} log",
        )
        label = record.get("label")
        safe_kind = re.sub(r"[^a-z0-9._-]+", "-", declared["kind"].lower())
        if not isinstance(label, str) or re.fullmatch(
            rf"review-{re.escape(safe_kind)}-try-[12]", label
        ) is None:
            raise legacy.HarnessError(
                f"v2 review {declared['kind']} invocation label is malformed"
            )
        invocation_time = legacy.verify_model_invocation(
            task_dir,
            vendor=str(record["vendor"]),
            role="reviewer",
            label=label,
            session_id=str(session),
            model=str(record.get("model", "")),
            cli_version=str(record.get("cli_version", "")),
            invocation_path_raw=str(invocation_path),
            invocation_sha256=record.get("invocation_sha256"),
            expected_path=(
                invocation_path.parent
                / f"{label}-{record['vendor']}-invocation.json"
            ),
            instructions_sha256=plan["protocol_sha256"],
            prompt_sha256=str(record["prompt_sha256"]),
            require_cwd_binding=True,
        )
        review_time = legacy.parse_timestamp(
            record.get("created_at"), f"v2 review {declared['kind']}.created_at"
        )
        if invocation_time > review_time:
            raise legacy.HarnessError(
                f"v2 review {declared['kind']} predates its invocation"
            )
        if record["vendor"] != "fake":
            metadata = legacy.extract_model_metadata(
                log_path, str(record["vendor"]), str(session)
            )
            if metadata["session_id"] != session:
                raise legacy.HarnessError(
                    f"v2 review {declared['kind']} session differs from real model log"
                )
            if metadata["model"] != record.get("model"):
                raise legacy.HarnessError(
                    f"v2 review {declared['kind']} model differs from real model log"
                )
    expected_review_outcomes = aggregate_review_outcomes(review_records)
    for field, expected in expected_review_outcomes.items():
        if evidence.get(field) != expected:
            raise legacy.HarnessError(
                f"v2 evidence {field} differs from its bound review records"
            )
    if evidence.get("findings") and any(
        item.get("severity") in SEVERE_FINDINGS for item in evidence["findings"]
    ):
        raise legacy.HarnessError("v2 PASS contains unresolved MAJOR/BLOCKER findings")
    if evidence.get("proof_gaps") or evidence.get("probe_requests"):
        raise legacy.HarnessError("v2 PASS contains unresolved proof gaps")
    return evidence
