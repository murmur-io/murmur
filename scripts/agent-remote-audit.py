#!/usr/bin/env python3
"""Read-only audit of Murmur's effective GitHub merge-enforcement boundary."""

from __future__ import annotations

import argparse
import copy
import json
import subprocess
import sys
from pathlib import Path
from typing import Any


API_HEADERS = [
    "-H",
    "Accept: application/vnd.github+json",
    "-H",
    "X-GitHub-Api-Version: 2022-11-28",
]


def gh_json(endpoint: str) -> Any:
    result = subprocess.run(
        ["gh", "api", *API_HEADERS, endpoint],
        text=True,
        capture_output=True,
        check=False,
        timeout=30,
    )
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or f"gh api {endpoint} failed")
    return json.loads(result.stdout)


def finding(finding_id: str, passed: bool, detail: Any) -> dict[str, Any]:
    return {"id": finding_id, "status": "PASS" if passed else "FAIL", "detail": detail}


def rule_from(
    rules: list[dict[str, Any]], rule_type: str, ruleset_id: int | None
) -> dict[str, Any] | None:
    return next(
        (
            rule
            for rule in rules
            if rule.get("type") == rule_type
            and (ruleset_id is None or rule.get("ruleset_id") == ruleset_id)
        ),
        None,
    )


def evaluate(
    policy: dict[str, Any],
    rulesets: list[dict[str, Any]],
    effective_rules: list[dict[str, Any]],
    repository: dict[str, Any],
    *,
    privileged: bool = True,
) -> list[dict[str, Any]]:
    findings: list[dict[str, Any]] = []
    monitoring = policy["privileged_monitoring"]
    repo = policy["repository"]
    expected_name = policy["ruleset_name"]
    candidates = [
        value
        for value in rulesets
        if value.get("name") == expected_name
        and value.get("source") == repo
        and value.get("source_type") == "Repository"
        and value.get("target") == "branch"
    ]
    ruleset = candidates[0] if len(candidates) == 1 else None
    active = ruleset is not None and ruleset.get("enforcement") == "active"
    findings.append(
        finding(
            "active-repository-ruleset",
            active,
            {
                "name": expected_name,
                "matches": len(candidates),
                "enforcement": ruleset.get("enforcement") if ruleset else None,
            },
        )
    )

    ruleset_id = ruleset.get("id") if ruleset else None
    effective_from_ruleset = [
        rule for rule in effective_rules if rule.get("ruleset_id") == ruleset_id
    ]
    findings.append(
        finding(
            "ruleset-applies-to-branch",
            active and bool(effective_from_ruleset),
            {
                "branch": policy["branch"],
                "ruleset_id": ruleset_id,
                "effective_rule_count": len(effective_from_ruleset),
            },
        )
    )

    bypasses = ruleset.get("bypass_actors") if ruleset else None
    expected_no_bypass = monitoring["no_ruleset_bypass"]
    findings.append(
        (
            finding(
                "no-ruleset-bypass",
                expected_no_bypass is True
                and isinstance(bypasses, list)
                and not bypasses,
                {
                    "bypass_actors_visible": isinstance(bypasses, list),
                    "bypass_actor_count": len(bypasses) if isinstance(bypasses, list) else None,
                    "required": expected_no_bypass,
                },
            )
            if privileged
            else {
                "id": "no-ruleset-bypass",
                "status": "MONITOR_ONLY",
                "detail": monitoring["scope"],
            }
        )
    )

    status_rule = rule_from(effective_rules, "required_status_checks", ruleset_id)
    status_params = (status_rule or {}).get("parameters") or {}
    expected_context = policy["required_status_check"]
    expected_app_id = policy.get("required_status_check_app_id")
    status_checks = status_params.get("required_status_checks") or []
    exact_check = any(
        isinstance(check, dict)
        and check.get("context") == expected_context
        and (
            expected_app_id is None
            or check.get("integration_id") == expected_app_id
        )
        for check in status_checks
    )
    findings.append(
        finding(
            "required-status-check",
            exact_check,
            {"context": expected_context, "integration_id": expected_app_id},
        )
    )
    strict = status_params.get("strict_required_status_checks_policy")
    findings.append(
        finding(
            "strict-status-checks",
            strict is policy["require_strict_status_checks"],
            {"actual": strict, "required": policy["require_strict_status_checks"]},
        )
    )

    pull_request_rule = rule_from(effective_rules, "pull_request", ruleset_id)
    review_params = (pull_request_rule or {}).get("parameters") or {}
    approvals = review_params.get("required_approving_review_count")
    expected_approvals = policy["required_approvals"]
    findings.append(
        finding(
            "required-approvals",
            approvals == expected_approvals,
            {
                "actual": approvals,
                "required": expected_approvals,
                "rationale": policy["approval_policy_rationale"],
            },
        )
    )
    resolution = review_params.get("required_review_thread_resolution")
    findings.append(
        finding(
            "conversation-resolution",
            resolution is policy["require_conversation_resolution"],
            {
                "actual": resolution,
                "required": policy["require_conversation_resolution"],
            },
        )
    )

    findings.append(
        finding(
            "force-push-disabled",
            rule_from(effective_rules, "non_fast_forward", ruleset_id) is not None,
            {"rule": "non_fast_forward"},
        )
    )
    findings.append(
        finding(
            "branch-deletion-disabled",
            rule_from(effective_rules, "deletion", ruleset_id) is not None,
            {"rule": "deletion"},
        )
    )

    security = repository.get("security_and_analysis") or {}
    scanning = (security.get("secret_scanning") or {}).get("status") == "enabled"
    push = (
        security.get("secret_scanning_push_protection") or {}
    ).get("status") == "enabled"
    if privileged:
        findings.append(
            finding(
                "secret-scanning",
                scanning is monitoring["secret_scanning"],
                scanning,
            )
        )
        findings.append(
            finding(
                "secret-push-protection",
                push is monitoring["secret_push_protection"],
                push,
            )
        )
    else:
        findings.extend(
            [
                {
                    "id": "secret-scanning",
                    "status": "MONITOR_ONLY",
                    "detail": monitoring["scope"],
                },
                {
                    "id": "secret-push-protection",
                    "status": "MONITOR_ONLY",
                    "detail": monitoring["scope"],
                },
            ]
        )
    return findings


def secure_fixture() -> tuple[
    dict[str, Any], list[dict[str, Any]], list[dict[str, Any]], dict[str, Any]
]:
    policy = {
        "repository": "murmur-io/murmur",
        "branch": "murmur",
        "ruleset_name": "Protect",
        "required_status_check": "gate (full ci.sh — release parity)",
        "required_status_check_app_id": 15368,
        "require_strict_status_checks": True,
        "required_approvals": 0,
        "approval_policy_rationale": "single operator; a self-approval requirement would deadlock",
        "require_conversation_resolution": True,
        "privileged_monitoring": {
            "scope": "trusted schedule/manual monitoring; not claimed by PR status",
            "no_ruleset_bypass": True,
            "secret_scanning": True,
            "secret_push_protection": True,
        },
    }
    rulesets = [
        {
            "id": 42,
            "name": "Protect",
            "source": "murmur-io/murmur",
            "source_type": "Repository",
            "target": "branch",
            "enforcement": "active",
            "bypass_actors": [],
        }
    ]
    common = {
        "ruleset_id": 42,
        "ruleset_source": "murmur-io/murmur",
        "ruleset_source_type": "Repository",
    }
    effective_rules = [
        {
            **common,
            "type": "required_status_checks",
            "parameters": {
                "strict_required_status_checks_policy": True,
                "required_status_checks": [
                    {
                        "context": "gate (full ci.sh — release parity)",
                        "integration_id": 15368,
                    }
                ],
            },
        },
        {
            **common,
            "type": "pull_request",
            "parameters": {
                "required_approving_review_count": 0,
                "required_review_thread_resolution": True,
            },
        },
        {**common, "type": "non_fast_forward"},
        {**common, "type": "deletion"},
    ]
    repository = {
        "security_and_analysis": {
            "secret_scanning": {"status": "enabled"},
            "secret_scanning_push_protection": {"status": "enabled"},
        }
    }
    return policy, rulesets, effective_rules, repository


def selftest() -> int:
    policy, rulesets, effective_rules, repository = secure_fixture()
    failures = []
    if any(
        item["status"] != "PASS"
        for item in evaluate(policy, rulesets, effective_rules, repository)
    ):
        failures.append("secure fixture did not pass")
    public_findings = evaluate(
        policy,
        rulesets,
        effective_rules,
        {},
        privileged=False,
    )
    public_monitor_only = {
        item["id"] for item in public_findings if item["status"] == "MONITOR_ONLY"
    }
    if (
        any(item["status"] in {"FAIL", "UNKNOWN"} for item in public_findings)
        or public_monitor_only
        != {
        "no-ruleset-bypass",
        "secret-scanning",
        "secret-push-protection",
        }
    ):
        failures.append(
            "public fixture did not preserve exact PASS/MONITOR_ONLY coverage"
        )

    mutations = (
        (
            "ruleset does not apply",
            lambda p, rs, er, repo: er.clear(),
        ),
        (
            "ruleset bypass",
            lambda p, rs, er, repo: rs[0]["bypass_actors"].append(
                {"actor_type": "RepositoryRole", "actor_id": 5, "bypass_mode": "always"}
            ),
        ),
        (
            "wrong check context",
            lambda p, rs, er, repo: er[0]["parameters"]["required_status_checks"][
                0
            ].update(context="gate"),
        ),
        (
            "wrong check integration",
            lambda p, rs, er, repo: er[0]["parameters"]["required_status_checks"][
                0
            ].update(integration_id=1),
        ),
        (
            "non-strict status",
            lambda p, rs, er, repo: er[0]["parameters"].update(
                strict_required_status_checks_policy=False
            ),
        ),
        (
            "dishonest approval requirement",
            lambda p, rs, er, repo: er[1]["parameters"].update(
                required_approving_review_count=1
            ),
        ),
        (
            "unresolved conversations allowed",
            lambda p, rs, er, repo: er[1]["parameters"].update(
                required_review_thread_resolution=False
            ),
        ),
        (
            "force push allowed",
            lambda p, rs, er, repo: er.__setitem__(
                slice(None), [rule for rule in er if rule["type"] != "non_fast_forward"]
            ),
        ),
        (
            "branch deletion allowed",
            lambda p, rs, er, repo: er.__setitem__(
                slice(None), [rule for rule in er if rule["type"] != "deletion"]
            ),
        ),
        (
            "secret scanning disabled",
            lambda p, rs, er, repo: repo["security_and_analysis"][
                "secret_scanning"
            ].update(status="disabled"),
        ),
        (
            "push protection disabled",
            lambda p, rs, er, repo: repo["security_and_analysis"][
                "secret_scanning_push_protection"
            ].update(status="disabled"),
        ),
    )
    for label, mutate in mutations:
        changed = copy.deepcopy((policy, rulesets, effective_rules, repository))
        mutate(*changed)
        if all(item["status"] == "PASS" for item in evaluate(*changed)):
            failures.append(f"policy evaluator accepted {label}")

    if failures:
        print("remote policy evaluator selftest: FAIL", file=sys.stderr)
        for failure in failures:
            print(f"- {failure}", file=sys.stderr)
        return 1
    print("remote policy evaluator selftest: PASS")
    return 0


def load_live(policy: dict[str, Any]) -> tuple[
    list[dict[str, Any]], list[dict[str, Any]], dict[str, Any]
]:
    repo = policy["repository"]
    branch = policy["branch"]
    summaries = gh_json(f"repos/{repo}/rulesets?includes_parents=true&targets=branch")
    if not isinstance(summaries, list):
        raise RuntimeError("unexpected response for repository rulesets")
    details = []
    for summary in summaries:
        if not isinstance(summary, dict) or not isinstance(summary.get("id"), int):
            raise RuntimeError("repository ruleset response contains an invalid item")
        detail = gh_json(f"repos/{repo}/rulesets/{summary['id']}?includes_parents=true")
        if not isinstance(detail, dict):
            raise RuntimeError(f"unexpected response for ruleset {summary['id']}")
        details.append(detail)
    effective = gh_json(f"repos/{repo}/rules/branches/{branch}?per_page=100")
    if not isinstance(effective, list):
        raise RuntimeError("unexpected response for effective branch rules")
    repository = gh_json(f"repos/{repo}")
    if not isinstance(repository, dict):
        raise RuntimeError("unexpected response for repository")
    return details, effective, repository


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--selftest", action="store_true")
    parser.add_argument(
        "--public",
        action="store_true",
        help=(
            "verify only the live merge-time surface available without a privileged "
            "credential; label separately monitored admin controls MONITOR_ONLY"
        ),
    )
    args = parser.parse_args()
    if args.selftest:
        return selftest()

    root = Path(
        subprocess.check_output(
            ["git", "rev-parse", "--show-toplevel"], text=True
        ).strip()
    )
    policy = json.loads((root / ".agents/h/remote-policy.json").read_text())
    repo = policy["repository"]
    branch = policy["branch"]
    try:
        rulesets, effective_rules, repository = load_live(policy)
        findings = evaluate(
            policy,
            rulesets,
            effective_rules,
            repository,
            privileged=not args.public,
        )
    except (
        FileNotFoundError,
        RuntimeError,
        json.JSONDecodeError,
        subprocess.TimeoutExpired,
    ) as exc:
        findings = [
            {"id": "remote-unavailable", "status": "UNKNOWN", "detail": str(exc)}
        ]

    failures = [item for item in findings if item["status"] == "FAIL"]
    unknowns = [item for item in findings if item["status"] == "UNKNOWN"]
    monitor_only = [
        item for item in findings if item["status"] == "MONITOR_ONLY"
    ]
    verdict = (
        "UNKNOWN"
        if findings and len(unknowns) == len(findings)
        else (
            "FAIL"
            if failures
            else (
                "UNKNOWN"
                if unknowns
                else ("PASS_MERGE_SCOPE" if args.public and monitor_only else "PASS")
            )
        )
    )
    result = {
        "schema_version": 3,
        "repository": repo,
        "branch": branch,
        "verdict": verdict,
        "findings": findings,
    }
    if args.json:
        print(json.dumps(result, sort_keys=True))
    else:
        print(f"remote harness boundary: {verdict}")
        for item in findings:
            print(f"  {item['status']:7} {item['id']}: {item['detail']}")
        if verdict not in {"PASS", "PASS_MERGE_SCOPE"}:
            print(
                "Remote settings are not mutated by this command; changing them "
                "requires explicit operator approval."
            )
    return 0 if verdict in {"PASS", "PASS_MERGE_SCOPE"} else 2


if __name__ == "__main__":
    sys.exit(main())
