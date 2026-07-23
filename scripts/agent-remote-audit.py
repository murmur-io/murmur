#!/usr/bin/env python3
"""Read-only audit of the remote enforcement boundary for the agent harness."""

from __future__ import annotations

import argparse
import copy
import json
import subprocess
import sys
from pathlib import Path


def gh_json(endpoint: str) -> dict:
    result = subprocess.run(
        ["gh", "api", endpoint], text=True, capture_output=True, check=False, timeout=30
    )
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or f"gh api {endpoint} failed")
    value = json.loads(result.stdout)
    if not isinstance(value, dict):
        raise RuntimeError(f"unexpected response for {endpoint}")
    return value


def evaluate(policy: dict, protection: dict, repository: dict) -> list[dict]:
    findings = []
    required = protection.get("required_status_checks") or {}
    expected_context = policy["required_status_check"]
    expected_app_id = policy.get("required_status_check_app_id")
    checks = [item for item in required.get("checks") or [] if isinstance(item, dict)]
    app_bound = any(
        item.get("context") == expected_context
        and (expected_app_id is None or item.get("app_id") == expected_app_id)
        for item in checks
    )
    findings.append(
        {
            "id": "required-status-check",
            "status": "PASS" if app_bound else "FAIL",
            "detail": {"context": expected_context, "app_id": expected_app_id},
        }
    )
    strict = bool(required.get("strict"))
    findings.append(
        {
            "id": "strict-status-checks",
            "status": "PASS" if strict == bool(policy.get("require_strict_status_checks")) else "FAIL",
            "detail": strict,
        }
    )
    reviews = protection.get("required_pull_request_reviews") or {}
    approvals = reviews.get("required_approving_review_count", 0)
    if not isinstance(approvals, int) or isinstance(approvals, bool):
        approvals = 0
    minimum = int(policy.get("minimum_approvals", 0))
    findings.append(
        {
            "id": "required-approvals",
            "status": "PASS" if approvals >= minimum else "FAIL",
            "detail": {"actual": approvals, "minimum": minimum},
        }
    )
    admin = bool((protection.get("enforce_admins") or {}).get("enabled"))
    findings.append(
        {
            "id": "admin-enforcement",
            "status": "PASS" if admin == bool(policy.get("require_admin_enforcement")) else "FAIL",
            "detail": admin,
        }
    )
    resolution = bool((protection.get("required_conversation_resolution") or {}).get("enabled"))
    findings.append(
        {
            "id": "conversation-resolution",
            "status": "PASS" if resolution == bool(policy.get("require_conversation_resolution")) else "FAIL",
            "detail": resolution,
        }
    )
    force_pushes = bool((protection.get("allow_force_pushes") or {}).get("enabled"))
    findings.append(
        {
            "id": "force-push-disabled",
            "status": "PASS" if not force_pushes else "FAIL",
            "detail": force_pushes,
        }
    )
    deletions = bool((protection.get("allow_deletions") or {}).get("enabled"))
    findings.append(
        {
            "id": "branch-deletion-disabled",
            "status": "PASS" if not deletions else "FAIL",
            "detail": deletions,
        }
    )
    security = repository.get("security_and_analysis") or {}
    scanning = (security.get("secret_scanning") or {}).get("status") == "enabled"
    push = (security.get("secret_scanning_push_protection") or {}).get("status") == "enabled"
    findings.append(
        {
            "id": "secret-scanning",
            "status": "PASS" if scanning == bool(policy.get("require_secret_scanning")) else "FAIL",
            "detail": scanning,
        }
    )
    findings.append(
        {
            "id": "secret-push-protection",
            "status": "PASS" if push == bool(policy.get("require_secret_push_protection")) else "FAIL",
            "detail": push,
        }
    )
    return findings


def selftest() -> int:
    policy = {
        "required_status_check": "gate",
        "required_status_check_app_id": 15368,
        "require_strict_status_checks": True,
        "minimum_approvals": 1,
        "require_admin_enforcement": True,
        "require_conversation_resolution": True,
        "require_secret_scanning": True,
        "require_secret_push_protection": True,
    }
    protection = {
        "required_status_checks": {
            "strict": True,
            "checks": [{"context": "gate", "app_id": 15368}],
        },
        "required_pull_request_reviews": {"required_approving_review_count": 1},
        "enforce_admins": {"enabled": True},
        "required_conversation_resolution": {"enabled": True},
        "allow_force_pushes": {"enabled": False},
        "allow_deletions": {"enabled": False},
    }
    repository = {
        "security_and_analysis": {
            "secret_scanning": {"status": "enabled"},
            "secret_scanning_push_protection": {"status": "enabled"},
        }
    }
    failures = []
    if any(item["status"] != "PASS" for item in evaluate(policy, protection, repository)):
        failures.append("secure fixture did not pass")
    for label, mutate in (
        (
            "zero approvals",
            lambda value: value["required_pull_request_reviews"].update(
                required_approving_review_count=0
            ),
        ),
        (
            "wrong check app",
            lambda value: value["required_status_checks"]["checks"][0].update(app_id=1),
        ),
        (
            "force push enabled",
            lambda value: value["allow_force_pushes"].update(enabled=True),
        ),
    ):
        mutated = copy.deepcopy(protection)
        mutate(mutated)
        if all(item["status"] == "PASS" for item in evaluate(policy, mutated, repository)):
            failures.append(f"policy evaluator accepted {label}")
    if failures:
        print("remote policy evaluator selftest: FAIL", file=sys.stderr)
        for failure in failures:
            print(f"- {failure}", file=sys.stderr)
        return 1
    print("remote policy evaluator selftest: PASS")
    return 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--selftest", action="store_true")
    args = parser.parse_args()
    if args.selftest:
        return selftest()
    root = Path(subprocess.check_output(["git", "rev-parse", "--show-toplevel"], text=True).strip())
    policy = json.loads((root / ".agents/harness/remote-policy.json").read_text())
    repo = policy["repository"]
    branch = policy["branch"]
    findings = []
    try:
        protection = gh_json(f"repos/{repo}/branches/{branch}/protection")
        repository = gh_json(f"repos/{repo}")
    except (FileNotFoundError, RuntimeError, json.JSONDecodeError, subprocess.TimeoutExpired) as exc:
        findings.append({"id": "remote-unavailable", "status": "UNKNOWN", "detail": str(exc)})
    else:
        findings.extend(evaluate(policy, protection, repository))

    verdict = (
        "UNKNOWN"
        if findings and all(item["status"] == "UNKNOWN" for item in findings)
        else ("PASS" if findings and all(item["status"] == "PASS" for item in findings) else "FAIL")
    )
    result = {"schema_version": 1, "repository": repo, "branch": branch, "verdict": verdict, "findings": findings}
    if args.json:
        print(json.dumps(result, sort_keys=True))
    else:
        print(f"remote harness boundary: {verdict}")
        for item in findings:
            print(f"  {item['status']:7} {item['id']}: {item['detail']}")
        if verdict != "PASS":
            print("Remote settings are not mutated by this command; changing them requires explicit operator approval.")
    return 0 if verdict == "PASS" else 2


if __name__ == "__main__":
    sys.exit(main())
