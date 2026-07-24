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
from pathlib import Path
from typing import Any, Dict, Iterable, List, Mapping, Optional, Sequence, Tuple


ROOT = Path(__file__).resolve().parents[2]
HOOK_NAMES = ("block-bash", "secret-scan", "finish-guard")
WRAPPER_NAMES = (*HOOK_NAMES, "selftest")
REQUIRED_RULES = ("rust-tauri.md", "angular-zoneless.md", "lock-model.md", "agentic-workflow.md")
ADVERSARIAL_PROMPT_MARKER = "control-plane-audit: shipped-bug-classes-v1"
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
        isinstance(config, dict) and config.get("schema_version") == 1,
        "harness config schema_version=1",
        "harness config must be an object with schema_version=1",
    )
    audit.require(
        isinstance(config, dict) and config.get("default_writer") in ("codex", "claude"),
        "harness default_writer is a real model vendor",
        "harness default_writer must be codex or claude",
    )
    audit.require(
        isinstance(config, dict) and config.get("default_reviewer") in ("codex", "claude"),
        "harness default_reviewer is a real model vendor",
        "harness default_reviewer must be codex or claude",
    )
    audit.require(
        isinstance(config, dict)
        and isinstance(config.get("task_timeout_seconds"), int)
        and 1 <= config["task_timeout_seconds"] <= 86_400,
        "harness has a bounded task-wide wall deadline",
        "harness task_timeout_seconds must be between 1 and 86400",
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


def _adversarial_prompt_contract(audit: Audit) -> None:
    path = ROOT / ".agents" / "harness" / "prompts" / "adversarial-reviewer.md"
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
    for profile, workspace_access in (
        ("murmur_harness_writer", "write"),
        ("murmur_harness_reviewer", "read"),
    ):
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

    runner = ROOT / ".agents" / "harness" / "task_runner.py"
    try:
        runner_text = runner.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as exc:
        audit.error(f"cannot read task runner for Codex profile audit: {exc}")
        return
    audit.require(
        'default_permissions="{permission_profile}"' in runner_text
        and 'permissions.{permission_profile}.filesystem={filesystem_profile}' in runner_text
        and 'permissions.{permission_profile}.network.enabled=false' in runner_text
        and '"murmur_harness_writer" if role == "writer" else "murmur_harness_reviewer"' in runner_text,
        "task runner selects and inlines Codex writer/reviewer permission profiles",
        "task runner must select and inline the Codex permission profile per role",
    )


def _eval_adapter_security(audit: Audit) -> None:
    path = ROOT / ".agents" / "harness" / "eval_runner.py"
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as exc:
        audit.error(f"cannot read eval runner security envelope: {exc}")
        return
    audit.require(
        "permissions.murmur_eval.filesystem=" in text
        and 'default_permissions="murmur_eval"' in text
        and '":minimal"="read"' in text
        and "permissions.murmur_eval.network.enabled=false" in text,
        "Codex eval adapter uses an inline minimal/network-off profile",
        "Codex eval adapter must use an inline minimal/network-off permission profile",
    )
    audit.require(
        '"failIfUnavailable": True' in text
        and '"allowUnsandboxedCommands": False' in text
        and '"denyRead": ["~/"]' in text
        and '"deniedDomains": ["*"]' in text
        and '"CLAUDE_CODE_SUBPROCESS_ENV_SCRUB"' in text,
        "Claude eval adapter fails closed with home/network/env isolation",
        "Claude eval adapter must fail closed with home/network/env isolation",
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

    dedicated_mapping = re.findall(
        r"(?m)^\s+MURMUR_REMOTE_AUDIT_TOKEN:\s*"
        r"\$\{\{\s*secrets\.MURMUR_REMOTE_AUDIT_TOKEN\s*\}\}\s*$",
        workflow_text,
    )
    public_mapping = re.findall(
        r"(?m)^\s+MURMUR_PUBLIC_REMOTE_AUDIT_TOKEN:\s*"
        r"\$\{\{[^\n]*github\.event_name\s*==\s*'pull_request'[^\n]*github\.token[^\n]*\}\}\s*$",
        workflow_text,
    )
    audit.require(
        len(dedicated_mapping) == 1,
        "privileged remote audit receives exactly the dedicated secret",
        "ci.yml must pass MURMUR_REMOTE_AUDIT_TOKEN directly from its same-named secret",
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


def run_audit() -> Audit:
    audit = Audit()
    documents = _json_audit(audit)
    _bash_syntax(audit)
    _agent_and_rule_manifest(audit)
    _codex_permission_profiles(audit)
    _eval_adapter_security(audit)
    _hook_parity(documents, audit)
    _adversarial_prompt_contract(audit)
    _semantic_lint(audit)
    _remote_workflow_contract(audit)
    _headless_e2e_no_egress_contract(audit)
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
