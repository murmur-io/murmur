#!/usr/bin/env python3
"""Dependency-free audit of Murmur's Claude/Codex development control plane."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path, PurePosixPath
from typing import Any, Dict, Iterable, List, Mapping, Optional, Sequence, Tuple


ROOT = Path(__file__).resolve().parents[2]
HOOK_NAMES = ("block-bash", "secret-scan", "finish-guard")
WRAPPER_NAMES = (*HOOK_NAMES, "selftest")
REQUIRED_RULES = ("rust-tauri.md", "angular-zoneless.md", "lock-model.md", "agentic-workflow.md")
ADVERSARIAL_PROMPT_MARKER = "control-plane-audit: shipped-bug-classes"
ADVERSARIAL_BUG_MARKERS = (
    "SEALED_CONTENT_LEAK",
    "FFI_LAUNCH_ABORT",
    "ANGULAR_NG0600",
    "ANGULAR_IMPORT_CYCLE_ɵcmp",
    "SEAL_ROUND_TRIP_LOSS",
    "EGRESS_WITHOUT_CONSENT",
    "PROCESS_OWNERSHIP_KILL",
)
REQUIRED_AGENTS = (
    "rust-tauri-dev",
    "angular-zoneless-dev",
    "adversarial-verifier",
    "lock-security-reviewer",
    "release-engineer",
    "ci-cd-engineer",
    "murmur-researcher",
)
REQUIRED_DENIES = {
    "Read(~/.ssh/**)",
    "Read(~/.aws/**)",
    "Read(~/.gnupg/**)",
    "Read(**/*.pem)",
    "Read(**/*.p12)",
    "Read(**/id_rsa)",
    "Read(**/id_ed25519)",
    "Read(~/.cargo/credentials*)",
    "Read(~/Library/Application Support/MeetNotes/**)",
}
LOCAL_SETTINGS_RELATIVE = (".claude", "settings.local.json")
LOCAL_POLICY_ACK_KEY = "_ack_policy_overrides"
# Tracked `sandbox` scalar -> the local value that REVERSES the tracked posture.
# Direction cannot be inferred from the value: `autoAllowBashIfSandboxed`
# true -> false only ADDS permission prompts, so it has no reversal at all and
# carries None.  `_local_settings` fails when the tracked document declares a
# sandbox scalar missing from this table, so a future hardening in
# `.claude/settings.json` cannot land silently unmodelled.
SANDBOX_SCALAR_REVERSALS: Dict[str, Optional[bool]] = {
    "allowUnsandboxedCommands": True,
    "autoAllowBashIfSandboxed": None,
    "enabled": False,
    "failIfUnavailable": False,
}
# Least -> most permissive.  A local `permissions.defaultMode` ranked at or above
# the tracked one (absent means Claude Code's own `default`) suppresses gating the
# repository declared; an unrecognized mode cannot be proven safe, so it ranks last.
PERMISSION_MODES: Tuple[str, ...] = (
    "plan",
    "default",
    "acceptEdits",
    "bypassPermissions",
)
# Shortest literal prefix a tracked deny must expose before it may be matched by
# containment instead of by exact string.
MIN_DENY_LITERAL = 4
LOCAL_POLICY_KEYS: Tuple[str, ...] = tuple(
    sorted(
        {
            "env",
            "permissions.allow",
            "permissions.defaultMode",
            "sandbox.filesystem.allowWrite",
            "sandbox.filesystem.denyWrite",
        }
        | {
            f"sandbox.{name}"
            for name, reversal in SANDBOX_SCALAR_REVERSALS.items()
            if reversal is not None
        }
    )
)
CODEX_REQUIRED_DENY_PATHS = {
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
}


def _sha256(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def _canonical_json(value: Any) -> bytes:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")


_HEREDOC_START = re.compile(
    r"<<(?P<tabs>-)?\s*(?:'(?P<single>[^']+)'|\"(?P<double>[^\"]+)\"|(?P<bare>[A-Za-z_][A-Za-z0-9_]*))"
)


def _blank_shell_arithmetic(source: str) -> str:
    """Blank arithmetic expansions/commands before looking for heredocs."""

    chars = list(source)
    single = False
    double = False
    escaped = False
    index = 0
    while index < len(source):
        char = source[index]
        if escaped:
            escaped = False
            index += 1
            continue
        if char == "\\" and not single:
            escaped = True
            index += 1
            continue
        if char == "'" and not double:
            single = not single
            index += 1
            continue
        if char == '"' and not single:
            double = not double
            index += 1
            continue

        expansion = not single and source.startswith("$((", index)
        command = not single and not double and source.startswith("((", index)
        if not expansion and not command:
            index += 1
            continue

        start = index
        cursor = index + (3 if expansion else 2)
        depth = 2
        arithmetic_escaped = False
        while cursor < len(source) and depth:
            arithmetic_char = source[cursor]
            if arithmetic_escaped:
                arithmetic_escaped = False
            elif arithmetic_char == "\\":
                arithmetic_escaped = True
            elif arithmetic_char == "(":
                depth += 1
            elif arithmetic_char == ")":
                depth -= 1
            cursor += 1
        if depth:
            index += 1
            continue
        chars[start:cursor] = " " * (cursor - start)
        index = cursor
    return "".join(chars)


def _strip_shell_comment(line: str) -> str:
    """Remove a real shell comment while preserving quoted hash characters."""

    single = False
    double = False
    escaped = False
    for index, char in enumerate(line):
        if escaped:
            escaped = False
            continue
        if char == "\\" and not single:
            escaped = True
            continue
        if char == "'" and not double:
            single = not single
            continue
        if char == '"' and not single:
            double = not double
            continue
        if (
            char == "#"
            and not single
            and not double
            and (index == 0 or line[index - 1].isspace())
        ):
            return line[:index]
    return line


def _shell_executable_statements(source: str) -> List[Tuple[int, str]]:
    """Return executable shell lines, excluding comments and heredoc bodies."""

    statements: List[Tuple[int, str]] = []
    heredocs: List[Tuple[str, bool]] = []
    offset = 0
    for line in source.splitlines(keepends=True):
        if heredocs:
            delimiter, strip_tabs = heredocs[0]
            candidate = line.rstrip("\r\n")
            if strip_tabs:
                candidate = candidate.lstrip("\t")
            if candidate == delimiter:
                heredocs.pop(0)
            offset += len(line)
            continue

        code = _strip_shell_comment(line).strip()
        if code:
            statements.append((offset, code))
            for match in _HEREDOC_START.finditer(_blank_shell_arithmetic(code)):
                delimiter = (
                    match.group("single")
                    or match.group("double")
                    or match.group("bare")
                )
                heredocs.append((delimiter, bool(match.group("tabs"))))
        offset += len(line)
    return statements


@dataclass
class Audit:
    errors: List[str] = field(default_factory=list)
    warnings: List[str] = field(default_factory=list)
    checks: List[str] = field(default_factory=list)
    fingerprints: Dict[str, str] = field(default_factory=dict)

    def require(self, condition: bool, success: str, failure: str) -> None:
        if condition:
            self.checks.append(success)
        else:
            self.errors.append(failure)

    def error(self, message: str) -> None:
        self.errors.append(message)

    def warn(self, message: str) -> None:
        self.warnings.append(message)


def _load_json(path: Path, audit: Audit) -> Optional[Any]:
    try:
        with path.open("r", encoding="utf-8") as handle:
            document = json.load(handle)
    except FileNotFoundError:
        audit.error(f"missing JSON file: {path.relative_to(ROOT)}")
        return None
    except (OSError, json.JSONDecodeError) as exc:
        audit.error(f"invalid JSON in {path.relative_to(ROOT)}: {exc}")
        return None
    audit.checks.append(f"JSON parses: {path.relative_to(ROOT)}")
    return document


def _json_audit(audit: Audit) -> Dict[str, Any]:
    paths = sorted((ROOT / ".agents" / "harness").rglob("*.json"))
    paths.extend([ROOT / ".codex" / "hooks.json", ROOT / ".claude" / "settings.json"])
    documents: Dict[str, Any] = {}
    for path in paths:
        document = _load_json(path, audit)
        if document is not None:
            documents[path.relative_to(ROOT).as_posix()] = document

    config = documents.get(".agents/harness/config.json")
    audit.require(
        isinstance(config, dict) and config.get("schema_version") == 2,
        "harness config schema_version=2",
        "harness config must be an object with schema_version=2",
    )
    audit.require(
        isinstance(config, dict) and config.get("default_reviewer") == "codex",
        "harness defaults to the Codex reviewer",
        "harness default_reviewer must be codex; Claude remains explicit opt-in",
    )
    audit.require(
        isinstance(config, dict)
        and isinstance(config.get("reviewer_timeout_seconds"), int)
        and 1 <= config["reviewer_timeout_seconds"] <= 86_400,
        "harness has a bounded reviewer deadline",
        "harness reviewer_timeout_seconds must be between 1 and 86400",
    )
    authority = config.get("review_authority") if isinstance(config, dict) else None
    risk_reviews = config.get("risk_reviews") if isinstance(config, dict) else None
    # Demoting a reviewer is a measured decision, and the first one shipped was
    # measured against the wrong threshold: it ranked reviewers by BLOCKER count
    # while the gate that actually forbids a PASS reads
    # `verifier.SEVERE_FINDINGS` (MAJOR + BLOCKER). Re-counted on that metric
    # over the same 232-review corpus, the `combined` generalist had the HIGHEST
    # density of PASS-forbidding findings, not the lowest: 116 severe findings
    # over 105 reviews (1.10 each) against 30/74 (0.41) for egress-security and
    # 7/53 (0.13) for lock-security. So every planned review kind ships blocking,
    # and a future demotion has to change this pin deliberately rather than
    # inherit one by editing a single config value.
    audit.require(
        isinstance(authority, dict)
        and isinstance(risk_reviews, dict)
        and all(
            authority.get(kind) == "blocking"
            for kind in (
                "combined",
                "lock-security",
                "egress-security",
                "protocol-security",
            )
        )
        and all(
            authority.get(kind) == "blocking" for kind in risk_reviews.values()
        ),
        "every planned review kind retains blocking review authority",
        "review_authority must keep combined and every risk_reviews specialist blocking",
    )
    audit.require(
        isinstance(authority, dict)
        and set(authority).issubset(
            {"combined", "lock-security", "egress-security", "protocol-security"}
        )
        and set(authority.values()).issubset({"blocking", "advisory"}),
        "review authority names only real review kinds",
        "review_authority keys must be planned review kinds with blocking/advisory values",
    )
    canonical = config.get("canonical_checks", {}) if isinstance(config, dict) else {}
    risk_evidence = config.get("risk_required_evidence", {}) if isinstance(config, dict) else {}
    required_check_ids = {
        check_id
        for values in risk_evidence.values()
        if isinstance(values, list)
        for check_id in values
        if isinstance(check_id, str)
    } if isinstance(risk_evidence, dict) else set()
    audit.require(
        isinstance(canonical, dict)
        and required_check_ids
        and required_check_ids.issubset(set(canonical)),
        "every risk evidence id has a canonical command",
        "risk evidence ids must resolve to runner-owned canonical_checks",
    )
    performance_profiles = risk_evidence.get("performance", []) if isinstance(risk_evidence, dict) else []
    audit.require(
        "perf-contracts" in performance_profiles
        and (ROOT / ".agents" / "harness" / "checks" / "perf-contracts.sh").is_file(),
        "hot paths require deterministic performance contracts",
        "performance risk must require .agents/harness/checks/perf-contracts.sh",
    )
    sherpa = (
        config.get("shared_artifacts", {}).get("sherpa_onnx", {})
        if isinstance(config, dict)
        else {}
    )
    sherpa_archives = sherpa.get("archives", {}) if isinstance(sherpa, dict) else {}
    audit.require(
        isinstance(sherpa, dict)
        and sherpa.get("directory") == "target/sherpa-onnx-prebuilt"
        and isinstance(sherpa_archives, dict)
        and set(sherpa_archives) == {"arm64", "x86_64"}
        and all(
            isinstance(value, dict)
            and isinstance(value.get("filename"), str)
            and value["filename"].endswith(".tar.bz2")
            and isinstance(value.get("sha256"), str)
            and re.fullmatch(r"[0-9a-f]{64}", value["sha256"])
            for value in sherpa_archives.values()
        ),
        "sherpa-onnx offline archives are versioned and checksum-pinned",
        "shared_artifacts.sherpa_onnx must pin filename + SHA-256 for arm64 and x86_64",
    )
    identity = config.get("commit_identity", {}) if isinstance(config, dict) else {}
    audit.require(
        isinstance(identity, dict)
        and identity.get("name") == "QueaT"
        and identity.get("email") == "kgm004a@gmail.com",
        "harness commit identity is QueaT",
        "harness commit_identity must remain QueaT <kgm004a@gmail.com>",
    )
    schema_paths = sorted((ROOT / ".agents" / "harness" / "schemas").glob("*.schema.json"))
    for path in schema_paths:
        document = documents.get(path.relative_to(ROOT).as_posix())
        audit.require(
            isinstance(document, dict) and document.get("type") == "object" and isinstance(document.get("required"), list),
            f"schema has object contract: {path.name}",
            f"schema must declare object type and required fields: {path.relative_to(ROOT)}",
        )
    return documents


def _bash_syntax(audit: Audit) -> None:
    paths = sorted((ROOT / ".codex" / "hooks").glob("*.sh"))
    paths.extend(sorted((ROOT / ".claude" / "hooks").glob("*.sh")))
    launcher = ROOT / "scripts" / "agent-config-audit"
    if launcher.is_file():
        paths.append(launcher)
    paths.extend(sorted((ROOT / ".agents" / "harness" / "checks").glob("*.sh")))
    for path in paths:
        completed = subprocess.run(
            ["bash", "-n", str(path)], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, check=False
        )
        if completed.returncode == 0:
            audit.checks.append(f"shell syntax: {path.relative_to(ROOT)}")
        else:
            audit.error(f"shell syntax failed for {path.relative_to(ROOT)}: {completed.stderr.strip()}")


_TOP_LEVEL_ASSIGNMENT = re.compile(r"^([A-Za-z_][A-Za-z0-9_.-]*)\s*=\s*(.*)$")


def _basic_toml(path: Path, audit: Audit) -> None:
    """Validate the deliberately small top-level subset used by Codex agents.

    Python 3.9 ships without tomllib on the oldest supported Murmur Mac.  The
    adapter files use three string keys only, so a strict structural parser is
    safer than silently skipping validation or adding a dependency.
    """

    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except (OSError, UnicodeError) as exc:
        audit.error(f"cannot read TOML {path.relative_to(ROOT)}: {exc}")
        return
    values: Dict[str, str] = {}
    multiline_key: Optional[str] = None
    multiline: List[str] = []
    for number, raw in enumerate(lines, 1):
        stripped = raw.strip()
        if multiline_key is not None:
            if '"""' in raw:
                before, _, trailing = raw.partition('"""')
                multiline.append(before)
                if trailing.strip() and not trailing.lstrip().startswith("#"):
                    audit.error(f"unexpected TOML text after multiline string: {path.relative_to(ROOT)}:{number}")
                values[multiline_key] = "\n".join(multiline)
                multiline_key = None
                multiline = []
            else:
                multiline.append(raw)
            continue
        if not stripped or stripped.startswith("#"):
            continue
        match = _TOP_LEVEL_ASSIGNMENT.match(raw)
        if not match:
            audit.error(f"unsupported TOML syntax: {path.relative_to(ROOT)}:{number}")
            continue
        key, raw_value = match.groups()
        if key in values:
            audit.error(f"duplicate TOML key {key}: {path.relative_to(ROOT)}:{number}")
            continue
        raw_value = raw_value.strip()
        if raw_value.startswith('"""'):
            rest = raw_value[3:]
            if '"""' in rest:
                value, _, trailing = rest.partition('"""')
                if trailing.strip() and not trailing.lstrip().startswith("#"):
                    audit.error(f"unexpected TOML text: {path.relative_to(ROOT)}:{number}")
                values[key] = value
            else:
                multiline_key = key
                multiline = [rest] if rest else []
            continue
        if not (len(raw_value) >= 2 and raw_value.startswith('"') and raw_value.endswith('"')):
            audit.error(f"Codex agent TOML values must be strings: {path.relative_to(ROOT)}:{number}")
            continue
        try:
            values[key] = json.loads(raw_value)
        except json.JSONDecodeError as exc:
            audit.error(f"invalid TOML string in {path.relative_to(ROOT)}:{number}: {exc}")
    if multiline_key is not None:
        audit.error(f"unterminated TOML multiline string {multiline_key}: {path.relative_to(ROOT)}")
    required = {"name", "description", "developer_instructions"}
    optional = {"model", "model_reasoning_effort", "sandbox_mode", "nickname"}
    missing = sorted(required - set(values))
    extras = sorted(set(values) - required - optional)
    if missing:
        audit.error(f"Codex agent {path.name} missing keys: {', '.join(missing)}")
    if extras:
        audit.error(f"Codex agent {path.name} has unsupported top-level keys: {', '.join(extras)}")
    if values.get("name") != path.stem:
        audit.error(f"Codex agent name does not match filename: {path.relative_to(ROOT)}")
    if not missing and not extras and values.get("name") == path.stem:
        audit.checks.append(f"Codex agent TOML basic validity: {path.name}")


def _agent_and_rule_manifest(audit: Audit) -> None:
    for name in REQUIRED_RULES:
        for vendor in ("codex", "claude"):
            path = ROOT / f".{vendor}" / "rules" / name
            audit.require(path.is_file(), f"rule present: .{vendor}/rules/{name}", f"missing rule: .{vendor}/rules/{name}")
    for name in REQUIRED_AGENTS:
        codex = ROOT / ".codex" / "agents" / f"{name}.toml"
        claude = ROOT / ".claude" / "agents" / f"{name}.md"
        audit.require(codex.is_file(), f"Codex agent present: {name}", f"missing Codex agent: {codex.relative_to(ROOT)}")
        audit.require(claude.is_file(), f"Claude agent present: {name}", f"missing Claude agent: {claude.relative_to(ROOT)}")
    for path in sorted((ROOT / ".codex" / "agents").glob("*.toml")):
        _basic_toml(path, audit)

    # Binding rules are deliberately vendor-neutral and byte-identical.  Compare
    # the full manifests too: a new one-sided rule is drift, even if the four
    # required core files still match.
    codex_rules = {path.name: path for path in (ROOT / ".codex" / "rules").glob("*.md")}
    claude_rules = {path.name: path for path in (ROOT / ".claude" / "rules").glob("*.md")}
    audit.require(
        set(codex_rules) == set(claude_rules),
        "binding rule manifests match",
        "Claude/Codex binding rule manifest drift: "
        f"Codex-only={sorted(set(codex_rules) - set(claude_rules))}, "
        f"Claude-only={sorted(set(claude_rules) - set(codex_rules))}",
    )
    for name in sorted(set(codex_rules) & set(claude_rules)):
        left_bytes = codex_rules[name].read_bytes()
        right_bytes = claude_rules[name].read_bytes()
        audit.require(
            left_bytes == right_bytes,
            f"binding rule parity: {name}",
            f"Claude/Codex binding rule drift: {name}",
        )
        if left_bytes == right_bytes:
            audit.fingerprints[f"rule:{name}"] = _sha256(left_bytes)


def _learnings_parity(audit: Audit) -> None:
    # `.claude/learnings/` is canonical: it is what /learn, /curate-learnings and the
    # murmur-learn skills write.  `.codex/learnings/` is a generated byte copy produced by
    # scripts/agent-sync-learnings, so a hand-edit on the mirror silently loses the lesson at
    # the next sync.  Compare the full manifests too: a one-sided file is drift even when
    # every shared file still matches byte for byte.
    canonical = {path.name: path for path in (ROOT / ".claude" / "learnings").glob("*.md")}
    mirror = {path.name: path for path in (ROOT / ".codex" / "learnings").glob("*.md")}
    audit.require(
        bool(canonical),
        "canonical learnings tree present",
        "missing canonical learnings tree: .claude/learnings/*.md",
    )
    audit.require(
        set(canonical) == set(mirror),
        "learnings manifests match",
        "Claude/Codex learnings manifest drift (run scripts/agent-sync-learnings): "
        f"canonical-only={sorted(set(canonical) - set(mirror))}, "
        f"mirror-only={sorted(set(mirror) - set(canonical))}",
    )
    for name in sorted(set(canonical) & set(mirror)):
        left_bytes = canonical[name].read_bytes()
        right_bytes = mirror[name].read_bytes()
        audit.require(
            left_bytes == right_bytes,
            f"learnings mirror parity: {name}",
            f"Claude/Codex learnings drift: {name} — run scripts/agent-sync-learnings",
        )
        if left_bytes == right_bytes:
            audit.fingerprints[f"learnings:{name}"] = _sha256(left_bytes)


# Constructs the binding rulesets ban outright (`.claude/rules/angular-zoneless.md`
# "Banned -> replacement").  A learnings bullet may still NAME one — telling a
# reviewer to flag it is the whole point — so a bullet is only an error when it
# names one with nothing in it marking the construct as wrong.
LEARNINGS_BANNED_CONSTRUCTS = (
    "allowsignalwrites",
    "*ngif",
    "*ngfor",
    "*ngswitch",
    "standalone: true",
    "provideexperimentalzonelesschangedetection",
    "zone.js",
)
LEARNINGS_PROHIBITIVE_MARKERS = (
    "never",
    "no-op",
    "deprecated",
    "banned",
    "refuse",
    "delete",
    "remove",
    "gone",
    "forbidden",
    "avoid",
    "reintroduce",
    "do not",
    "don't",
    "flag ",
)


def _recurring_bullets(text: str) -> List[Tuple[int, str]]:
    """Slice `## Recurring patterns` into (1-based start line, bullet) blocks."""

    lines = text.splitlines()
    start = None
    for index, line in enumerate(lines):
        if line.strip() == "## Recurring patterns":
            start = index + 1
            break
    if start is None:
        return []
    end = len(lines)
    for index in range(start, len(lines)):
        if lines[index].startswith("## "):
            end = index
            break
    bullets: List[Tuple[int, str]] = []
    for index in range(start, end):
        line = lines[index]
        if line[:1] in {"-", "*"} or not bullets:
            bullets.append((index + 1, line))
        else:
            number, body = bullets[-1]
            bullets[-1] = (number, body + "\n" + line)
    return bullets


def _learnings_lint(audit: Audit) -> None:
    # CLAUDE.md and AGENTS.md now declare `## Recurring patterns` binding imperatives
    # that "outrank the agent's own general guidance", and verifier.py splices the same
    # tier into reviewer prompts.  `_semantic_lint` never looks at learnings/, so before
    # this a bullet could instruct an agent to write exactly what `.claude/rules/` bans
    # and stay green through `ng lint`, `ng build` and the audit alike — which is how a
    # bullet telling agents to pass `{ allowSignalWrites: true }` survived the v22
    # migration that deleted the flag repo-wide.
    directory = ROOT / ".claude" / "learnings"
    offenders = 0
    for path in sorted(directory.glob("*.md")):
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeError) as exc:
            audit.error(f"cannot lint learnings file {path.relative_to(ROOT)}: {exc}")
            offenders += 1
            continue
        for number, bullet in _recurring_bullets(text):
            lowered = bullet.lower()
            if any(marker in lowered for marker in LEARNINGS_PROHIBITIVE_MARKERS):
                continue
            for construct in LEARNINGS_BANNED_CONSTRUCTS:
                if construct in lowered:
                    offenders += 1
                    audit.error(
                        "binding learnings bullet prescribes a repo-banned construct "
                        f"({construct}): {path.relative_to(ROOT)}:{number}"
                    )
    audit.require(
        offenders == 0,
        "binding learnings agree with the banned-construct rules",
        f"{offenders} binding learnings bullet(s) contradict .claude/rules/",
    )


def _adversarial_prompt_contract(audit: Audit) -> None:
    path = ROOT / ".agents" / "harness" / "prompts" / "combined-reviewer.md"
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as exc:
        audit.error(f"cannot read adversarial reviewer prompt: {exc}")
        return
    audit.require(
        ADVERSARIAL_PROMPT_MARKER in text,
        "adversarial prompt audit marker present",
        f"adversarial prompt missing audit marker: {ADVERSARIAL_PROMPT_MARKER}",
    )
    missing = [marker for marker in ADVERSARIAL_BUG_MARKERS if marker not in text]
    audit.require(
        not missing,
        "adversarial prompt names all seven shipped bug classes",
        "adversarial prompt missing shipped bug classes: " + ", ".join(missing),
    )
    if not missing and ADVERSARIAL_PROMPT_MARKER in text:
        audit.fingerprints["adversarial-reviewer-prompt"] = _sha256(text.encode("utf-8"))


def _toml_table(text: str, name: str) -> Optional[str]:
    match = re.search(
        rf"^\[{re.escape(name)}\]\s*$\n(.*?)(?=^\[|\Z)",
        text,
        flags=re.MULTILINE | re.DOTALL,
    )
    return match.group(1) if match else None


def _codex_permission_profiles(audit: Audit) -> None:
    path = ROOT / ".codex" / "config.toml"
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as exc:
        audit.error(f"cannot read Codex config: {exc}")
        return

    audit.require(
        not re.search(r"^\s*sandbox_mode\s*=", text, flags=re.MULTILINE),
        "Codex named permission profiles are not shadowed by legacy sandbox_mode",
        ".codex/config.toml must not combine named profiles with sandbox_mode",
    )
    for profile, workspace_access in (("murmur_harness_reviewer", "read"),):
        filesystem = _toml_table(text, f"permissions.{profile}.filesystem")
        workspace = _toml_table(text, f'permissions.{profile}.filesystem.":workspace_roots"')
        network = _toml_table(text, f"permissions.{profile}.network")
        audit.require(
            filesystem is not None and re.search(r'^\s*":root"\s*=\s*"read"\s*$', filesystem, re.MULTILINE) is not None,
            f"Codex {profile} keeps toolchain reads explicit",
            f"Codex {profile} must grant :root read before carving denied paths",
        )
        missing = sorted(
            denied
            for denied in CODEX_REQUIRED_DENY_PATHS
            if filesystem is None
            or re.search(rf'^\s*"{re.escape(denied)}"\s*=\s*"deny"\s*$', filesystem, re.MULTILINE) is None
        )
        audit.require(
            not missing,
            f"Codex {profile} credential/private-data deny list preserved",
            f"Codex {profile} lost denied paths: {', '.join(missing)}",
        )
        audit.require(
            workspace is not None
            and re.search(rf'^\s*"\."\s*=\s*"{workspace_access}"\s*$', workspace, re.MULTILINE) is not None
            and '"**/.env" = "deny"' in workspace,
            f"Codex {profile} workspace access is {workspace_access} with secret globs denied",
            f"Codex {profile} workspace access/secret deny policy is incomplete",
        )
        audit.require(
            network is not None and re.search(r"^\s*enabled\s*=\s*false\s*$", network, re.MULTILINE) is not None,
            f"Codex {profile} command network is disabled",
            f"Codex {profile} must disable command network",
        )

    runner = ROOT / ".agents" / "harness" / "runtime.py"
    try:
        runner_text = runner.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as exc:
        audit.error(f"cannot read Harness runtime for Codex profile audit: {exc}")
        return
    audit.require(
        'default_permissions="{permission_profile}"' in runner_text
        and 'permissions.{permission_profile}.filesystem={filesystem_profile}' in runner_text
        and 'permissions.{permission_profile}.network.enabled=false' in runner_text
        and 'permission_profile = "murmur_harness_reviewer"' in runner_text,
        "Harness runtime selects and inlines the Codex reviewer permission profile",
        "Harness runtime must select and inline the Codex reviewer permission profile",
    )
    audit.require(
        re.search(
            r'"--tools",\s*tools,\s*"--allowedTools",\s*tools,',
            runner_text,
        )
        is not None
        and '"autoAllowBashIfSandboxed": False' in runner_text
        and 'tools = ""' in runner_text
        and "model_cwd = reviewer_execution_cwd()" in runner_text,
        "Claude reviewer has an empty tool surface and isolated non-project cwd",
        "Claude reviewer must pass empty --tools/--allowedTools values, override "
        "project sandbox Bash auto-approval, and run outside the project tree",
    )
    audit.require(
        "REVIEWER_TOOL_DENIAL" in runner_text
        and '"/usr/bin/printf \'%s\\\\n\' "' in runner_text
        and 'f"hooks.PreToolUse={reviewer_guard_config}"' in runner_text
        and '"--dangerously-bypass-hook-trust"' in runner_text
        and "'web_search=\"disabled\"'" in runner_text
        and '"features.apps=false"' in runner_text
        and '"features.plugins=false"' in runner_text
        and '"features.multi_agent=false"' in runner_text,
        "Codex reviewer has a runner-owned deny-all tool boundary",
        "Codex reviewer must deny every local tool with the protocol-bound "
        "PreToolUse guard and disable hosted/networked tool surfaces",
    )


def _hook_entries(document: Any, event: str) -> List[Mapping[str, Any]]:
    if not isinstance(document, dict):
        return []
    hooks = document.get("hooks")
    if not isinstance(hooks, dict):
        return []
    entries = hooks.get(event)
    return [entry for entry in entries if isinstance(entry, dict)] if isinstance(entries, list) else []


def _commands_for_bash(document: Any) -> List[str]:
    result: List[str] = []
    for entry in _hook_entries(document, "PreToolUse"):
        matcher = str(entry.get("matcher", ""))
        if matcher not in {"Bash", "^Bash$"}:
            continue
        hooks = entry.get("hooks")
        if isinstance(hooks, list):
            for hook in hooks:
                if isinstance(hook, dict) and isinstance(hook.get("command"), str):
                    result.append(hook["command"])
    return result


def _normalized_wrapper(path: Path) -> bytes:
    text = path.read_text(encoding="utf-8")
    text = re.sub(r"--vendor\s+(codex|claude)", "--vendor VENDOR", text)
    return text.encode("utf-8")


def _hook_parity(documents: Mapping[str, Any], audit: Audit) -> None:
    for vendor in ("codex", "claude"):
        hooks_dir = ROOT / f".{vendor}" / "hooks"
        for name in WRAPPER_NAMES:
            path = hooks_dir / f"{name}.sh"
            audit.require(path.is_file(), f"hook wrapper present: .{vendor}/{name}", f"missing hook wrapper: {path.relative_to(ROOT)}")
            if path.is_file():
                text = path.read_text(encoding="utf-8")
                audit.require(
                    ".agents/harness/hook_guard.py" in text
                    and f"--vendor {vendor}" in text
                    and "BASH_SOURCE[0]" in text
                    and 'HOOK_DIR/../..' in text,
                    f"hook wrapper rooted in canonical guard: .{vendor}/{name}",
                    f"hook wrapper bypasses canonical guard or wrong vendor: {path.relative_to(ROOT)}",
                )

    for name in WRAPPER_NAMES:
        codex = ROOT / ".codex" / "hooks" / f"{name}.sh"
        claude = ROOT / ".claude" / "hooks" / f"{name}.sh"
        if codex.is_file() and claude.is_file():
            left = _normalized_wrapper(codex)
            right = _normalized_wrapper(claude)
            audit.require(
                left == right,
                f"canonical wrapper parity: {name}",
                f"Claude/Codex wrapper drift: {name}",
            )
            audit.fingerprints[f"wrapper:{name}"] = _sha256(left)

    auto_codex = ROOT / ".codex" / "hooks" / "autoformat.sh"
    auto_claude = ROOT / ".claude" / "hooks" / "autoformat.sh"
    audit.require(
        auto_codex.is_file() and auto_claude.is_file() and auto_codex.read_bytes() == auto_claude.read_bytes(),
        "autoformat hook parity",
        "Claude/Codex autoformat hooks differ",
    )

    for vendor, key in (("codex", ".codex/hooks.json"), ("claude", ".claude/settings.json")):
        commands = _commands_for_bash(documents.get(key))
        for name in HOOK_NAMES:
            matches = [command for command in commands if f".{vendor}/hooks/{name}.sh" in command]
            audit.require(
                len(matches) == 1,
                f"{vendor} wiring: {name}",
                f"{vendor} must wire exactly one {name} Bash hook",
            )
            if len(matches) == 1:
                anchor = "git rev-parse --show-toplevel" if vendor == "codex" else "$CLAUDE_PROJECT_DIR"
                audit.require(
                    anchor in matches[0],
                    f"{vendor} wiring is project-root anchored: {name}",
                    f"{vendor} hook wiring is not rooted for {name}",
                )

    settings = documents.get(".claude/settings.json")
    env = settings.get("env", {}) if isinstance(settings, dict) else {}
    audit.require(
        isinstance(env, dict) and env.get("MURMUR_FINISH_GUARD") == "enforce",
        "Claude finish guard defaults to enforce",
        "Claude MURMUR_FINISH_GUARD must default to enforce",
    )
    permissions = settings.get("permissions", {}) if isinstance(settings, dict) else {}
    deny = permissions.get("deny", []) if isinstance(permissions, dict) else []
    missing_denies = sorted(REQUIRED_DENIES - set(deny if isinstance(deny, list) else []))
    audit.require(
        not missing_denies,
        "Claude credential/private-data deny list preserved",
        "Claude deny settings lost protected entries: " + ", ".join(missing_denies),
    )

    handler = ROOT / ".agents" / "harness" / "hook_guard.py"
    config = ROOT / ".agents" / "harness" / "config.json"
    schemas = sorted((ROOT / ".agents" / "harness" / "schemas").glob("*.json"))
    if handler.is_file():
        audit.fingerprints["canonical-hook-guard"] = _sha256(handler.read_bytes())
    if config.is_file():
        audit.fingerprints["harness-config"] = _sha256(config.read_bytes())
    schema_digest = hashlib.sha256()
    for path in schemas:
        schema_digest.update(path.name.encode("utf-8") + b"\x00" + path.read_bytes() + b"\x00")
    audit.fingerprints["schema-bundle"] = schema_digest.hexdigest()
    audit.fingerprints["parity-manifest"] = _sha256(_canonical_json(audit.fingerprints))


def _lexical(path: PurePosixPath) -> PurePosixPath:
    """Collapse `.` and `..` without touching the filesystem."""

    parts: List[str] = []
    for part in path.parts:
        if part == ".":
            continue
        if part == ".." and parts and parts[-1] not in {"..", "/"}:
            parts.pop()
            continue
        parts.append(part)
    return PurePosixPath(*parts) if parts else PurePosixPath(".")


def _expanded_home(entry: str) -> str:
    """Expand a leading `~`/`$HOME`/`${HOME}` the way Claude Code would.

    An undeterminable home degrades to the unexpanded text: the `.git` component
    test still applies, only the ancestor test stops reaching through `~`.
    """

    text = entry.strip().replace("\\", "/")
    try:
        home = Path.home().as_posix()
    except (OSError, RuntimeError):
        return text
    for token in ("${HOME}", "$HOME", "~"):
        if text == token:
            return home
        if text.startswith(token + "/"):
            return home + text[len(token) :]
    return text


def _covers_git_directory(entry: str) -> Optional[str]:
    """Why this sandbox write grant reaches a Git directory, or None.

    Two ways in, and the second is the one a blocked operator reaches for. A
    literal `.git` component is the obvious form; the failure message names it,
    so deleting the `/.git` suffix both silences the rule and WIDENS the grant.
    A sandbox write grant is a SUBTREE grant, so `/`, `$HOME`, and the repository
    directory itself all still confer write on `<repo>/.git/hooks` — which runs
    unsandboxed on the operator's next commit.

    The component test is case-folded because the default macOS filesystem is
    case-insensitive, so `.GIT` names the same directory.
    """

    text = _expanded_home(entry)
    trimmed = text.rstrip("/") or "/"
    candidate = PurePosixPath(trimmed)
    if any(part.lower() == ".git" for part in candidate.parts):
        return f"grants sandbox write to a Git directory: {entry}"
    root = _lexical(PurePosixPath(ROOT.as_posix()))
    target = _lexical(
        candidate
        if candidate.is_absolute()
        else PurePosixPath((ROOT / trimmed).as_posix())
    )
    try:
        root.relative_to(target)
    except ValueError:
        return None
    return (
        f"grants sandbox write to {entry}, which covers the repository root "
        f"{root} and therefore {root.name}/.git"
    )


def _path_components(entry: str) -> Tuple[str, ...]:
    """Meaningful trailing components of a filesystem policy entry.

    Compared component-wise, never resolved: the tracked file declares
    project-relative paths (`../meetnotes/.git`) while a machine-local override
    declares the absolute form of the same directory, and the audit must stay
    hermetic — resolving either form would make the result depend on the machine
    it runs on.
    """

    text = _expanded_home(entry).rstrip("/")
    return tuple(
        part for part in PurePosixPath(text).parts if part not in {".", "..", "/"}
    )


def _deny_write_covered(
    tracked: Tuple[str, ...], local: Sequence[Tuple[str, ...]]
) -> bool:
    """True when some local `denyWrite` entry still denies all of `tracked`.

    A deny is a SUBTREE deny, so a local entry covers the tracked one when it
    names the same directory or an ancestor of it.  On trailing components that
    is exactly: some leading run of the tracked components is a trailing run of
    the local ones.
    """

    if not tracked:
        return True
    return any(
        entry[-size:] == tracked[:size]
        for entry in local
        for size in range(1, len(tracked) + 1)
    )


_GLOB_CHARACTER = re.compile(r"[*?\[]")


def _permission_target(entry: str) -> str:
    match = re.fullmatch(r"\s*([A-Za-z_]+)\((.*)\)\s*", entry, re.DOTALL)
    return match.group(2) if match else entry.strip()


def _deny_literal(entry: str) -> Optional[str]:
    """Literal path prefix a deny rule protects, or None for a pure-glob rule.

    `Read(~/.ssh/**)` protects everything under `~/.ssh/`, so a local
    `Read(~/.ssh/*)`, `Read(~/.ssh/id_rsa)` or `Bash(cat ~/.ssh/id_rsa)` re-grants
    it even though none of the three is the tracked string.  `Read(**/*.pem)`
    exposes no literal prefix at all and can only ever be matched exactly.
    """

    target = _permission_target(entry)
    match = _GLOB_CHARACTER.search(target)
    literal = (target[: match.start()] if match else target).strip()
    return literal if len(literal) >= MIN_DENY_LITERAL else None


def _mode_rank(mode: str) -> int:
    return (
        PERMISSION_MODES.index(mode)
        if mode in PERMISSION_MODES
        else len(PERMISSION_MODES)
    )


def _mapping(document: Any, key: str) -> Mapping[str, Any]:
    value = document.get(key) if isinstance(document, Mapping) else None
    return value if isinstance(value, Mapping) else {}


def _string_list(document: Mapping[str, Any], key: str) -> List[str]:
    value = document.get(key)
    return [entry for entry in value if isinstance(entry, str)] if isinstance(value, list) else []


def _policy_acknowledgements(document: Mapping[str, Any], audit: Audit) -> Dict[str, str]:
    """Parse the documented `"<key>: <reason>"` escape hatch.

    An acknowledgement downgrades its key to a warning, so it has to carry a
    recorded justification: a bare key, an empty reason, or a key this audit
    does not police is itself a failure rather than a mute button.
    """

    raw = document.get(LOCAL_POLICY_ACK_KEY)
    if raw is None:
        return {}
    if not isinstance(raw, list):
        audit.error(f"{LOCAL_POLICY_ACK_KEY} must be an array of \"<key>: <reason>\" strings")
        return {}
    acknowledged: Dict[str, str] = {}
    for entry in raw:
        if not isinstance(entry, str):
            audit.error(f"{LOCAL_POLICY_ACK_KEY} entries must be strings: {entry!r}")
            continue
        key, separator, reason = entry.partition(":")
        key = key.strip()
        reason = reason.strip()
        if not separator or not reason:
            audit.error(
                f"{LOCAL_POLICY_ACK_KEY} entry records no reason: {entry!r}; "
                'use "<key>: <why this machine needs it>"'
            )
            continue
        if key not in LOCAL_POLICY_KEYS:
            audit.error(
                f"{LOCAL_POLICY_ACK_KEY} names an unaudited key: {key!r}; "
                f"expected one of {', '.join(LOCAL_POLICY_KEYS)}"
            )
            continue
        acknowledged[key] = reason
    return acknowledged


def _local_settings(documents: Mapping[str, Any], audit: Audit) -> None:
    """Hold the untracked per-machine Claude override to the tracked posture.

    `.claude/settings.local.json` is machine-local and git-ignored, so it is the
    one file that can silently reverse the declared sandbox policy while every
    parity check and fingerprint still passes.  Absence is the CI contract: the
    file cannot exist on a runner, `run_audit` has no CI flag to branch on
    (`--ci` only pins output stability), and threading one through would change
    every section signature — so the existence guard alone keeps this section
    from ever failing a job for a file that cannot be there.

    The one check that runs BEFORE that guard reads only the tracked document:
    it asserts the reversal model still covers every sandbox scalar the tracked
    document declares, so a hardening added to `.claude/settings.json` cannot
    become an unpoliced key that a local override may quietly flip.
    """

    tracked = documents.get(".claude/settings.json")
    tracked_sandbox = _mapping(tracked, "sandbox")
    # Runs before the existence guard, and reads only the tracked document: it
    # asserts this audit still MODELS the posture it claims to defend, which is
    # exactly the thing CI must not be blind to.
    unmodelled = sorted(
        name
        for name, value in tracked_sandbox.items()
        if isinstance(value, bool) and name not in SANDBOX_SCALAR_REVERSALS
    )
    if unmodelled:
        audit.error(
            ".claude/settings.json declares sandbox scalars this audit does not model: "
            + ", ".join(unmodelled)
            + "; give each one a SANDBOX_SCALAR_REVERSALS direction"
        )

    path = ROOT.joinpath(*LOCAL_SETTINGS_RELATIVE)
    if not path.is_file():
        audit.checks.append("no local Claude settings override present")
        return

    reported = len(audit.errors)
    local = _load_json(path, audit)
    if not isinstance(local, dict):
        # `_load_json` returns None for a read/parse failure it already reported
        # and for a literal `null` document it did not; only the latter needs a
        # message, so anything non-object that stayed silent gets one here.
        if len(audit.errors) == reported:
            audit.error(f"{'/'.join(LOCAL_SETTINGS_RELATIVE)} must contain a JSON object")
        return
    if not isinstance(tracked, dict):
        audit.error("cannot audit the local Claude override without tracked .claude/settings.json")
        return

    acknowledged = _policy_acknowledgements(local, audit)
    findings: List[Tuple[str, str]] = []

    # Every comparison below is DERIVED from the tracked document, never a second
    # copy of its values: a policy change in .claude/settings.json moves the
    # comparison with it instead of leaving a stale duplicate behind.
    local_sandbox = _mapping(local, "sandbox")
    for name, reversal in sorted(SANDBOX_SCALAR_REVERSALS.items()):
        if reversal is None:
            continue
        declared = not reversal
        if tracked_sandbox.get(name) is declared and local_sandbox.get(name) is reversal:
            findings.append(
                (
                    f"sandbox.{name}",
                    f"local Claude override sets sandbox.{name}="
                    f"{'true' if reversal else 'false'} while .claude/settings.json "
                    f"declares {'true' if declared else 'false'}",
                )
            )

    local_filesystem = _mapping(local_sandbox, "filesystem")
    for entry in _string_list(local_filesystem, "allowWrite"):
        reach = _covers_git_directory(entry)
        if reach is not None:
            findings.append(
                ("sandbox.filesystem.allowWrite", f"local Claude override {reach}")
            )

    # The sandbox-side deny list, and the same protection as the rule above seen
    # from the other side. The array REPLACES rather than merges, so a local file
    # that redeclares it without a tracked entry drops that entry outright.
    tracked_filesystem = _mapping(tracked_sandbox, "filesystem")
    if isinstance(local_filesystem.get("denyWrite"), list):
        local_deny_write = [
            _path_components(entry)
            for entry in _string_list(local_filesystem, "denyWrite")
        ]
        dropped = [
            entry
            for entry in _string_list(tracked_filesystem, "denyWrite")
            if not _deny_write_covered(_path_components(entry), local_deny_write)
        ]
        if dropped:
            findings.append(
                (
                    "sandbox.filesystem.denyWrite",
                    "local Claude override redeclares sandbox.filesystem.denyWrite "
                    "without tracked entries: " + ", ".join(dropped),
                )
            )

    tracked_permissions = _mapping(tracked, "permissions")
    local_permissions = _mapping(local, "permissions")
    # Only the ALLOW side can weaken a deny. Claude Code unions deny lists across
    # settings sources and deny beats allow, so a local deny list that does not
    # restate a tracked entry has not dropped it — testing for that produced a
    # failure on a strictly-tightening override and taught the operator to ack
    # away the key that also covers this real re-grant.
    local_allow = _string_list(local_permissions, "allow")
    regrants: List[str] = []
    for denied in _string_list(tracked_permissions, "deny"):
        literal = _deny_literal(denied)
        matches = [
            allowed
            for allowed in local_allow
            if allowed == denied or (literal is not None and literal in allowed)
        ]
        if matches:
            regrants.append(f"{', '.join(matches)} re-grants {denied}")
    if regrants:
        findings.append(
            (
                "permissions.allow",
                "local Claude override re-grants tracked permissions.deny entries: "
                + "; ".join(regrants),
            )
        )

    local_mode = local_permissions.get("defaultMode")
    tracked_mode = tracked_permissions.get("defaultMode")
    if isinstance(local_mode, str) and local_mode:
        baseline = tracked_mode if isinstance(tracked_mode, str) and tracked_mode else "default"
        if local_mode != baseline and _mode_rank(local_mode) >= _mode_rank(baseline):
            findings.append(
                (
                    "permissions.defaultMode",
                    f"local Claude override sets permissions.defaultMode={local_mode} "
                    f"while the tracked posture is {baseline}",
                )
            )

    # `env` overrides `env`, and .claude/settings.json's MURMUR_FINISH_GUARD is
    # already asserted load-bearing by `_hook_parity`; hook_guard._finish_guard
    # returns None outright when it reads "off".
    tracked_env = _mapping(tracked, "env")
    local_env = _mapping(local, "env")
    for name in sorted(tracked_env):
        if name in local_env and local_env[name] != tracked_env[name]:
            findings.append(
                (
                    "env",
                    f"local Claude override changes tracked env {name}: "
                    f"{tracked_env[name]!r} -> {local_env[name]!r}",
                )
            )

    for key, message in findings:
        reason = acknowledged.get(key)
        if reason is None:
            audit.error(message)
        else:
            audit.warn(f"{message} [acknowledged: {reason}]")
    if not findings:
        audit.checks.append("local Claude settings override preserves the declared posture")


def _semantic_lint(audit: Audit) -> None:
    paths = [ROOT / "AGENTS.md", ROOT / "CLAUDE.md"]
    for directory, suffix in (
        (ROOT / ".codex" / "rules", "*.md"),
        (ROOT / ".claude" / "rules", "*.md"),
        (ROOT / ".codex" / "agents", "*.toml"),
        (ROOT / ".claude" / "agents", "*.md"),
    ):
        paths.extend(sorted(directory.glob(suffix)))
    for path in paths:
        if not path.is_file():
            continue
        try:
            lines = path.read_text(encoding="utf-8").splitlines()
        except (OSError, UnicodeError) as exc:
            audit.error(f"cannot lint instruction file {path.relative_to(ROOT)}: {exc}")
            continue
        for number, line in enumerate(lines, 1):
            lowered = line.lower()
            # Historical/migration warnings are legitimate; active stack/role
            # declarations are not.
            if "angular 18" in lowered or "angular-18" in lowered:
                historical = any(token in lowered for token in ("trained on", "migration", "legacy", "previous", "old "))
                if not historical:
                    audit.error(f"active Angular 18 instruction: {path.relative_to(ROOT)}:{number}")
            if re.search(r"provideExperimentalZonelessChangeDetection\s*\(", line):
                audit.error(f"active experimental zoneless provider: {path.relative_to(ROOT)}:{number}")
            if "allowsignalwrites" in lowered:
                requires = "required" in lowered or ("must" in lowered and "must not" not in lowered)
                deprecated = "deprecated" in lowered or "no-op" in lowered
                if requires and not deprecated:
                    audit.error(f"active allowSignalWrites requirement: {path.relative_to(ROOT)}:{number}")
    if not any("active Angular 18" in error or "experimental zoneless" in error or "allowSignalWrites" in error for error in audit.errors):
        audit.checks.append("critical Angular instruction semantics are current")


def _remote_workflow_contract(audit: Audit) -> None:
    workflow = ROOT / ".github" / "workflows" / "ci.yml"
    ci_script = ROOT / "scripts" / "ci.sh"
    try:
        workflow_text = workflow.read_text(encoding="utf-8")
        ci_text = ci_script.read_text(encoding="utf-8")
    except OSError as exc:
        audit.error(f"cannot read remote-audit workflow contract: {exc}")
        return

    dedicated_mapping = [
        line.strip()
        for line in workflow_text.splitlines()
        if line.lstrip().startswith("MURMUR_REMOTE_AUDIT_TOKEN:")
    ]
    expected_privileged_mapping = (
        "MURMUR_REMOTE_AUDIT_TOKEN: ${{ ((github.event_name == 'schedule' || "
        "github.event_name == 'workflow_dispatch') && github.ref == "
        "'refs/heads/murmur') && secrets.MURMUR_REMOTE_AUDIT_TOKEN || '' }}"
    )
    public_mapping = re.findall(
        r"(?m)^\s+MURMUR_PUBLIC_REMOTE_AUDIT_TOKEN:\s*"
        r"\$\{\{[^\n]*github\.event_name\s*==\s*'pull_request'[^\n]*github\.token[^\n]*\}\}\s*$",
        workflow_text,
    )
    audit.require(
        dedicated_mapping == [expected_privileged_mapping]
        and workflow_text.count("secrets.MURMUR_REMOTE_AUDIT_TOKEN") == 1,
        "privileged remote audit secret is scoped to trusted default-branch monitoring",
        "ci.yml must expose MURMUR_REMOTE_AUDIT_TOKEN exactly once and only to "
        "schedule/workflow_dispatch runs on refs/heads/murmur",
    )
    audit.require(
        len(public_mapping) == 1,
        "ordinary GitHub token is scoped to pull-request public audit",
        "ci.yml must pass github.token only through the pull-request public-audit variable",
    )
    privileged_fallback_markers = (
        "secrets.MURMUR_REMOTE_AUDIT_TOKEN !=",
        "|| github.token",
        "GH_TOKEN: ${{",
    )
    audit.require(
        not any(marker in workflow_text for marker in privileged_fallback_markers),
        "privileged remote audit has no ordinary-token fallback expression",
        "ci.yml contains a privileged-token fallback or direct GH_TOKEN expression",
    )
    required_ci_fragments = (
        'remote_audit_token="${MURMUR_REMOTE_AUDIT_TOKEN:-}"',
        "unset MURMUR_REMOTE_AUDIT_TOKEN MURMUR_PUBLIC_REMOTE_AUDIT_TOKEN GH_TOKEN GITHUB_TOKEN",
        'if [ -z "$remote_audit_token" ]; then',
        'GH_TOKEN="$remote_audit_token" scripts/agent-remote-audit',
        'GH_TOKEN="$public_audit_token" scripts/agent-remote-audit --public',
    )
    audit.require(
        all(fragment in ci_text for fragment in required_ci_fragments),
        "ci.sh selects remote-audit credentials fail-closed before repo checks",
        "ci.sh must preflight the dedicated token, isolate public/privileged credentials, and drop exports",
    )


def _headless_e2e_no_egress_contract(audit: Audit) -> None:
    paths = (
        ROOT / "src-tauri" / "examples" / "e2e_core.rs",
        ROOT / "scripts" / "e2e-core.sh",
        ROOT / "scripts" / "e2e-mix.sh",
        ROOT / "scripts" / "ci.sh",
        ROOT / ".github" / "workflows" / "ci.yml",
    )
    try:
        documents = {path: path.read_text(encoding="utf-8") for path in paths}
    except OSError as exc:
        audit.error(f"cannot read headless E2E no-egress contract: {exc}")
        return

    rust = documents[paths[0]]
    core_script = documents[paths[1]]
    mix_script = documents[paths[2]]
    workflow = documents[paths[4]]
    forbidden_cloud_switches = (
        "ClaudeCodeProvider",
        "MURMUR_E2E_PROVIDER",
        "MURMUR_E2E_ALLOW_CLOUD",
    )
    audit.require(
        not any(marker in text for text in documents.values() for marker in forbidden_cloud_switches),
        "headless E2E has no raw cloud-provider branch or opt-in switch",
        "headless E2E must remain a deterministic no-egress fake by construction",
    )
    audit.require(
        'let note = stub_note(&req);' in rust
        and 'provider mode: deterministic-stub (no egress)' in rust,
        "Rust headless E2E always selects the deterministic stub",
        "e2e_core.rs must unconditionally use and report the deterministic no-egress stub",
    )
    audit.require(
        "export MURMUR_E2E_NO_EGRESS=1" in core_script
        and "export MURMUR_E2E_NO_EGRESS=1" in mix_script
        and re.search(r'(?m)^\s+MURMUR_E2E_NO_EGRESS:\s*"1"\s*$', workflow) is not None,
        "headless E2E shells and GitHub gate declare no-egress mode",
        "headless E2E and CI must retain the explicit no-egress environment invariant",
    )


def _harness_v2_contract(audit: Audit) -> None:
    required = (
        ".agents/harness/cli.py",
        ".agents/harness/verifier.py",
        ".agents/harness/v2_selftest.py",
        ".agents/harness/v2_fault_selftest.py",
        ".agents/harness/metrics_selftest.py",
        ".agents/harness/prompts/combined-reviewer.md",
        ".agents/harness/schemas/v2-task.schema.json",
        ".agents/harness/schemas/v2-plan.schema.json",
        ".agents/harness/schemas/v2-review.schema.json",
        ".agents/harness/schemas/v2-evidence.schema.json",
        ".agents/harness/schemas/v2-commit-intent.schema.json",
        ".agents/harness/schemas/v2-commit.schema.json",
    )
    audit.require(
        all((ROOT / path).is_file() for path in required),
        "Harness v2 dispatcher, verifier, prompt, selftest, and schemas exist",
        "Harness v2 executable/schema surface is incomplete",
    )
    obsolete = (
        ".agents/harness/task_runner.py",
        ".agents/harness/eval_runner.py",
        ".agents/harness/evals",
        ".agents/harness/prompts/implementer.md",
        ".agents/harness/prompts/spec-reviewer.md",
        ".agents/harness/prompts/adversarial-reviewer.md",
        ".agents/harness/schemas/task.schema.json",
        ".agents/harness/schemas/model-result.schema.json",
        ".agents/harness/schemas/review.schema.json",
        ".agents/harness/schemas/attestation.schema.json",
        ".agents/harness/schemas/commit.schema.json",
    )

    def retired_surface_is_absent(path: str) -> bool:
        candidate = ROOT / path
        if not candidate.is_dir():
            return not candidate.exists()
        return not any(
            child.is_file() or child.is_symlink()
            for child in candidate.rglob("*")
        )

    audit.require(
        all(retired_surface_is_absent(path) for path in obsolete),
        "retired Harness v1 executable, writer, repair, and eval surfaces are absent",
        "remove every retired Harness v1 executable, writer, repair, and eval surface",
    )
    config = _load_json(ROOT / ".agents" / "harness" / "config.json", audit)
    canonical = config.get("canonical_checks", {}) if isinstance(config, dict) else {}
    required_checks = {
        "harness-python",
        "harness-v2-selftest",
        "receipt-selftest",
        "hook-selftest",
        "config-audit",
    }
    audit.require(
        isinstance(canonical, dict)
        and required_checks.issubset(canonical)
        and config.get("v2_max_parallel_reviews") == 3
        and isinstance(config.get("review_retry_max_delay_seconds"), int),
        "Harness v2 concurrency, retry, and control-plane checks are pinned",
        "Harness v2 config must pin max 3 reviews, retry delay, and all self-check ids",
    )
    try:
        wrapper = (ROOT / "scripts" / "agent-harness").read_text(encoding="utf-8")
        dispatcher = (ROOT / ".agents" / "harness" / "cli.py").read_text(
            encoding="utf-8"
        )
        ci = (ROOT / "scripts" / "ci.sh").read_text(encoding="utf-8")
        workflow = (ROOT / ".github" / "workflows" / "ci.yml").read_text(
            encoding="utf-8"
        )
        receipt_gate = (
            ROOT / "scripts" / "verify-harness-attestation"
        ).read_text(encoding="utf-8")
        finish_guard = (ROOT / ".agents" / "harness" / "hook_guard.py").read_text(
            encoding="utf-8"
        )
    except OSError as exc:
        audit.error(f"cannot read Harness v2 wrapper/CI contract: {exc}")
        return
    audit.require(
        ".agents/harness/cli.py" in wrapper,
        "agent-harness wrapper dispatches through the single verifier-only CLI",
        "scripts/agent-harness must execute .agents/harness/cli.py",
    )
    audit.require(
        all(
            f'"{script}"' in dispatcher
            for script in (
                "v2_selftest.py",
                "v2_fault_selftest.py",
                "metrics_selftest.py",
            )
        ),
        "Harness selftest aggregates lifecycle, fault, and metrics suites",
        "Harness CLI selftest must retain all deterministic sub-suites",
    )
    audit.require(
        "scripts/verify-harness-attestation --selftest" in ci
        and ci.index("scripts/verify-harness-attestation --selftest")
        < ci.index("scripts/agent-config-audit --ci"),
        "strict temp-git receipt selftest runs before expensive CI work",
        "scripts/ci.sh must run receipt --selftest before config/product gates",
    )
    token_capture = 'remote_audit_token="${MURMUR_REMOTE_AUDIT_TOKEN:-}"'
    public_token_capture = (
        'public_audit_token="${MURMUR_PUBLIC_REMOTE_AUDIT_TOKEN:-}"'
    )
    token_scrub = (
        "unset MURMUR_REMOTE_AUDIT_TOKEN MURMUR_PUBLIC_REMOTE_AUDIT_TOKEN "
        "GH_TOKEN GITHUB_TOKEN"
    )
    receipt_selftest = "scripts/verify-harness-attestation --selftest"

    def token_quarantine_contract(candidate: str) -> bool:
        positions: Dict[str, List[int]] = {
            statement: []
            for statement in (
                token_capture,
                public_token_capture,
                token_scrub,
                receipt_selftest,
            )
        }
        for offset, statement in _shell_executable_statements(candidate):
            if statement in positions:
                positions[statement].append(offset)
        return (
            all(len(found) == 1 for found in positions.values())
            and positions[token_capture][0] < positions[token_scrub][0]
            and positions[public_token_capture][0] < positions[token_scrub][0]
            and positions[token_scrub][0] < positions[receipt_selftest][0]
        )

    comment_spoof = ci.replace(
        f"  {token_scrub}",
        f"  # {token_scrub}",
        1,
    ).replace(
        f"  {receipt_selftest}",
        f"  {receipt_selftest}\n"
        "  unset MURMUR_REMOTE_AUDIT_TOKEN MURMUR_PUBLIC_REMOTE_AUDIT_TOKEN\n"
        "  unset GH_TOKEN GITHUB_TOKEN",
        1,
    )
    heredoc_spoof = ci.replace(
        f"  {token_scrub}",
        "  cat <<'TOKEN_SCRUB'\n"
        f"{token_scrub}\n"
        "TOKEN_SCRUB",
        1,
    ).replace(
        f"  {receipt_selftest}",
        f"  {receipt_selftest}\n"
        "  unset MURMUR_REMOTE_AUDIT_TOKEN MURMUR_PUBLIC_REMOTE_AUDIT_TOKEN\n"
        "  unset GH_TOKEN GITHUB_TOKEN",
        1,
    )
    arithmetic_shift = ci.replace(
        f"  {token_scrub}",
        "  shifted=$((offset << shift))\n"
        f"  {token_scrub}",
        1,
    )
    audit.require(
        token_quarantine_contract(arithmetic_shift),
        "CI quarantine parser distinguishes arithmetic shifts from heredocs",
        "scripts/ci.sh token quarantine parser must not treat shell arithmetic "
        "left shift as a heredoc opener",
    )
    audit.require(
        token_quarantine_contract(ci)
        and not token_quarantine_contract(comment_spoof)
        and not token_quarantine_contract(heredoc_spoof),
        "CI quarantines audit tokens before any repository command",
        "scripts/ci.sh must copy audit credentials into unexported locals, unset "
        "all exported token names with executable shell statements, and only "
        "then run the receipt selftest; comments and heredocs are not evidence",
    )
    monotonic_receipt_markers = (
        "Lane B cannot downgrade an agent/v2 branch",
        "agent/v2 branches require v2 receipts on every authored commit",
        "downgrades a v2 receipt history",
        "v2 receipt downgrade rejected",
        "agent/v2 rejects legacy v1 receipt",
        "ordinary agent branch accepts explicit Lane B",
        "agent/v2 rejects Lane B downgrade",
        "receipted history rejects Lane B downgrade",
    )
    audit.require(
        all(marker in receipt_gate for marker in monotonic_receipt_markers),
        "receipt policy is monotonic from Lane B/v1 into v2 and selftests the downgrade boundary",
        "verify-harness-attestation must reserve agent/v2, reject Lane B after receipts, "
        "reject v2-to-v1 downgrade, and retain RED selftests for each boundary",
    )
    rust_lane = re.compile(
        r"(?m)^\s+run:\s+bash scripts/ci\.sh rust\s*$"
    )
    web_lane = re.compile(
        r"(?m)^\s+run:\s+bash scripts/ci\.sh web\s*$"
    )

    def thin_lane_contract(candidate: str) -> bool:
        return (
            re.search(r"(?m)^\s+merge_group:\s*$", candidate) is not None
            and len(rust_lane.findall(candidate)) == 1
            and len(web_lane.findall(candidate)) == 1
        )

    duplicate_rust_lane = workflow + "\n        run: bash scripts/ci.sh rust\n"
    duplicate_web_lane = workflow + "\n        run: bash scripts/ci.sh web\n"
    audit.require(
        thin_lane_contract(workflow)
        and not thin_lane_contract(duplicate_rust_lane)
        and not thin_lane_contract(duplicate_web_lane),
        "GitHub merge queue remains a thin ci.sh wrapper",
        "ci.yml must trigger merge_group and contain exactly one real run step "
        "for each scripts/ci.sh lane; duplicate-step mutations must fail",
    )
    base_checkout = re.compile(
        r"(?m)^\s+ref:\s*\$\{\{\s*github\.event\.pull_request\.base\.sha\s*\}\}\s*$"
    )
    trusted_extract = re.compile(
        r'(?m)^\s+git show "\$BASE_SHA:scripts/verify-harness-attestation" '
        r'> "\$TRUSTED_VERIFIER"\s*$'
    )
    trusted_exec = re.compile(
        r'(?m)^\s+python3 "\$TRUSTED_VERIFIER"\s+\\\s*$'
    )
    candidate_exec = re.compile(
        r"(?m)^\s+python3\s+(?:\./)?scripts/verify-harness-attestation(?:\s+\\)?\s*$"
    )
    pr_only_receipt = re.compile(
        r"(?m)^\s+if:\s*github\.event_name\s*==\s*'pull_request'\s*$"
    )

    def trusted_receipt_anchor(text: str) -> bool:
        return (
            len(base_checkout.findall(text)) == 1
            and len(trusted_extract.findall(text)) == 1
            and len(trusted_exec.findall(text)) == 1
            and candidate_exec.search(text) is None
            and 'git cat-file -e "${BASE_SHA}^{commit}"' in text
            and 'git cat-file -e "${HEAD_SHA}^{commit}"' in text
            and 'test -s "$TRUSTED_VERIFIER"' in text
        )

    candidate_execution_mutation = workflow.replace(
        'python3 "$TRUSTED_VERIFIER"',
        "python3 scripts/verify-harness-attestation",
        1,
    )
    head_extraction_mutation = workflow.replace(
        "$BASE_SHA:scripts/verify-harness-attestation",
        "$HEAD_SHA:scripts/verify-harness-attestation",
        1,
    )
    audit.require(
        trusted_receipt_anchor(workflow)
        and not trusted_receipt_anchor(candidate_execution_mutation)
        and not trusted_receipt_anchor(head_extraction_mutation),
        "GitHub receipt gate executes only the verifier extracted from the exact PR base SHA",
        "ci.yml must checkout pull_request.base.sha, extract the receipt verifier from "
        "that commit, fail closed on missing blobs, and never execute the candidate copy",
    )
    audit.require(
        len(pr_only_receipt.findall(workflow)) == 1
        and re.search(r"(?m)^\s+merge_group:\s*$", workflow) is not None
        and re.search(
            r"(?m)^\s+types:\s*\[\s*checks_requested\s*\]\s*$",
            workflow,
        )
        is not None
        and "needs: [attestation, rust, web]" in workflow
        and "if: always()" in workflow
        and '[ "${{ github.event_name }}" = "pull_request" ]' in workflow
        and '${{ needs.attestation.result }}" != "success"' in workflow
        and '${{ needs.attestation.result }}" != "skipped"' in workflow,
        "PR receipts stay PR-payload-bound while merge queue runs aggregate safely",
        "the receipt job must remain pull_request-only; the aggregate must require "
        "receipt success on PRs, allow only the expected skip elsewhere, and include "
        "both full CI lanes on checks_requested merge groups",
    )
    audit.require(
        "_v2_verifier_module" in finish_guard
        and "validate_hashed_document" in finish_guard
        and "the Harness owns the durable commit intent and receipt" in finish_guard,
        "finish guard validates task manifests and delegates durable commits",
        "finish guard must validate task manifests and delegate durable commits "
        "to agent-harness",
    )


def run_audit() -> Audit:
    audit = Audit()
    documents = _json_audit(audit)
    _bash_syntax(audit)
    _agent_and_rule_manifest(audit)
    _learnings_parity(audit)
    _learnings_lint(audit)
    _codex_permission_profiles(audit)
    _hook_parity(documents, audit)
    _local_settings(documents, audit)
    _adversarial_prompt_contract(audit)
    _semantic_lint(audit)
    _remote_workflow_contract(audit)
    _headless_e2e_no_egress_contract(audit)
    _harness_v2_contract(audit)
    return audit


def main(argv: Optional[Sequence[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ci", action="store_true", help="stable CI output; warnings remain non-blocking")
    parser.add_argument("--json", action="store_true", help="emit a machine-readable report")
    args = parser.parse_args(argv)
    audit = run_audit()
    report = {
        "ok": not audit.errors,
        "checks": audit.checks,
        "errors": audit.errors,
        "warnings": audit.warnings,
        "fingerprints": audit.fingerprints,
    }
    if args.json:
        print(json.dumps(report, indent=2, sort_keys=True))
    else:
        for message in audit.errors:
            print(f"[FAIL] {message}")
        for message in audit.warnings:
            print(f"[WARN] {message}")
        for name, digest in sorted(audit.fingerprints.items()):
            print(f"[HASH] {name}: {digest}")
        print(f"agent config audit: {'PASS' if not audit.errors else 'FAIL'} ({len(audit.checks)} checks, {len(audit.warnings)} warnings)")
    return 0 if not audit.errors else 1


if __name__ == "__main__":
    raise SystemExit(main())
