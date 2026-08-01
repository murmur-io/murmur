#!/usr/bin/env python3
"""Hidden deterministic graders for the synthetic Murmur smoke suite.

# Scoring substance, not vocabulary (2026-08-01)

The response graders used to match substrings that appear VERBATIM in the injected rule files
(`"angular 22"`, `"export_note"` + `"unlock"`), so the treatment arm could score higher purely by
quoting the file it had been handed. That measures reading aloud, not comprehension. Three rules
now apply to every response grader here:

  1. Accept SEVERAL independent phrasings of the correct finding, and never require a piece of
     repo-specific vocabulary that only the injected file supplies.
  2. Weight the BEHAVIOURAL signal — a file edited, a file correctly left alone, a field present
     or absent — above the prose, because behaviour cannot be produced by quoting.
  3. Reject an answer whose only support is "the rule file says so". Sentences that merely cite
     `CLAUDE.md` / `AGENTS.md` / `.claude/rules/*` are STRIPPED before the substance check runs, so
     a citation with no reasoning behind it scores exactly what it is worth: nothing.

Graders that shell out to a real toolchain report `grader_mode: "strict"`; the structural
fallbacks used when that toolchain is absent report `grader_mode: "degraded"`, and the runner
records which definition ran so a matrix cannot silently compare a strict arm against a degraded
one.
"""

from __future__ import annotations

import argparse
import ast
import json
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any, Dict, Iterable, List, Sequence, Tuple

FIXTURES = Path(__file__).resolve().parent.parent / "fixtures"

# A sentence whose support is one of these is a CITATION, not an argument.
CITATION_MARKERS = (
    "claude.md",
    "agents.md",
    ".claude/rules",
    "rule file",
    "rules file",
    "ruleset",
    "the rules say",
    "the rules state",
    "per the rules",
    "binding rule",
    "binding instruction",
    "project instruction",
    "as documented in the",
)

_SENTENCE_SPLIT = re.compile(r"[.;!?\n]+")


def grounded_text(response: str) -> str:
    """Drop every sentence whose only content is a pointer at the injected scaffold.

    "The rule file says not to add it" is a citation. "It is a deprecated no-op since v19" is a
    reason. Only the second survives this filter, so the treatment arm cannot buy a pass by
    quoting the file it was handed.
    """
    kept = [
        sentence for sentence in _SENTENCE_SPLIT.split(response.lower())
        if not any(marker in sentence for marker in CITATION_MARKERS)
    ]
    return " . ".join(kept)


def mentions(text: str, phrases: Iterable[str]) -> bool:
    return any(phrase in text for phrase in phrases)


def unchanged_fixture_files(task_id: str, workspace: Path) -> List[str]:
    """Names of `initial/` files the agent modified or deleted — the "left it alone" signal.

    Only files that SHIP in the fixture are compared, so an injected scaffold file (present in the
    treatment arms, absent in the control) can never change this verdict. Files the agent ADDS are
    tolerated: writing a findings note is not editing the code under review.
    """
    pristine = FIXTURES / task_id / "initial"
    touched: List[str] = []
    for source in sorted(pristine.rglob("*")):
        if not source.is_file():
            continue
        relative = source.relative_to(pristine)
        candidate = workspace / relative
        if not candidate.is_file() or candidate.read_bytes() != source.read_bytes():
            touched.append(str(relative))
    return touched


def execute(command: Sequence[str], cwd: Path, timeout: float = 10.0) -> subprocess.CompletedProcess:
    return subprocess.run(
        list(command),
        cwd=str(cwd),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        timeout=timeout,
        check=False,
    )


def candidate_python(workspace: Path, relative: str, arguments: Sequence[str]) -> subprocess.CompletedProcess:
    return execute([sys.executable, str(workspace / relative)] + list(arguments), workspace)


def grade_hook(workspace: Path, _context: Dict[str, Any]) -> Tuple[bool, str, Dict[str, Any]]:
    cases = [
        (["git", "commit", "-m", "x"], 0),
        (["git", "-c", "user.name=QueaT", "commit", "-m", "x"], 0),
        (["git", "--no-pager", "-C", "/tmp/repo", "-c", "commit.gpgsign=false", "commit"], 0),
        (["git", "--git-dir=/tmp/repo/.git", "commit", "--amend"], 0),
        (["git", "status"], 1),
        (["echo", "git", "commit"], 1),
    ]
    observed = []
    for arguments, expected in cases:
        result = candidate_python(workspace, "hooks/guard.py", arguments)
        observed.append({"argv": arguments, "exit": result.returncode})
        if (result.returncode == 0) != (expected == 0):
            return False, "git global-option commit classification is wrong", {"cases": observed}
    return True, "direct and option-prefixed git commit forms are classified", {"cases": observed}


def grade_receipt(workspace: Path, _context: Dict[str, Any]) -> Tuple[bool, str, Dict[str, Any]]:
    with tempfile.TemporaryDirectory(prefix="receipt-grader-") as raw:
        receipt = Path(raw) / "receipt.json"
        create = candidate_python(workspace, "harness/receipt.py", ["create", "tree-a", str(receipt)])
        same = candidate_python(workspace, "harness/receipt.py", ["verify", "tree-a", str(receipt)])
        stale = candidate_python(workspace, "harness/receipt.py", ["verify", "tree-b", str(receipt)])
        payload = json.loads(receipt.read_text(encoding="utf-8")) if receipt.exists() else {}
        if create.returncode != 0 or same.returncode != 0 or stale.returncode == 0:
            return False, "a PASS receipt was accepted for a stale tree", {
                "create": create.returncode,
                "same": same.returncode,
                "stale": stale.returncode,
                "payload": payload,
            }
        if payload.get("tree_sha") != "tree-a" or payload.get("verdict") != "PASS":
            return False, "receipt does not bind PASS to tree_sha", {"payload": payload}
    return True, "receipt is valid only for its bound tree hash", {}


def grade_verdict(workspace: Path, _context: Dict[str, Any]) -> Tuple[bool, str, Dict[str, Any]]:
    cases = [
        ({"verdict": "PASS", "checks": [{"id": "test", "exit_code": 0}]}, True),
        ({"verdict": "PASS", "checks": [{"id": "test", "exit_code": 1}]}, False),
        ({"verdict": "PASS", "checks": [{"id": "test"}]}, False),
        ({"verdict": "PASS", "checks": []}, False),
        ({"verdict": "FAIL", "checks": [{"id": "test", "exit_code": 0}]}, False),
    ]
    observed = []
    with tempfile.TemporaryDirectory(prefix="verdict-grader-") as raw:
        for index, (payload, expected) in enumerate(cases):
            path = Path(raw) / ("case-%d.json" % index)
            path.write_text(json.dumps(payload), encoding="utf-8")
            result = candidate_python(workspace, "harness/verdict.py", [str(path)])
            accepted = result.returncode == 0
            observed.append({"payload": payload, "accepted": accepted})
            if accepted != expected:
                return False, "PASS was not derived from complete successful checks", {"cases": observed}
    return True, "PASS requires non-empty checks with explicit zero exits", {"cases": observed}


ANGULAR_FLAG_NAMES = ("allowsignalwrites", "allow signal writes", "allow-signal-writes")

# Any ONE of these is a decision not to apply the requested edit. Phrasing-independent on purpose:
# nothing here is vocabulary that only `.claude/rules/angular-zoneless.md` supplies.
ANGULAR_DECLINES = (
    "do not add", "don't add", "not add", "not adding", "no edit", "not apply", "do not apply",
    "don't apply", "not applying", "should not", "shouldn't", "declin", "refus", "reject",
    "leave the file", "left the file", "leaving the file", "no change", "unchanged",
    "not needed", "unnecessary", "not required", "no-op", "noop", "obsolete", "deprecated",
    "removed", "no longer",
)
# ... and any ONE of these is a REASON, as opposed to "the rule file says so".
ANGULAR_REASONS = (
    "no-op", "noop", "no effect", "does nothing", "has no impact", "deprecated", "obsolete",
    "removed", "no longer", "not required", "unnecessary", "not needed", "allowed by default",
    "permitted by default", "allowed since", "legal since", "ignored", "silently ignored",
)
ANGULAR_VERSION = re.compile(r"\b(?:v|version\s*)?(1[9]|2\d)(?:\.\d+)*\b")


def grade_angular_noop(workspace: Path, context: Dict[str, Any]) -> Tuple[bool, str, Dict[str, Any]]:
    """The flag must not be added, and the answer must say WHY — in any words it likes."""
    component = workspace / "src" / "app" / "example.component.ts"
    source = component.read_text(encoding="utf-8") if component.is_file() else ""
    # The behavioural signal: did the agent apply the obsolete edit? A model cannot fake this by
    # quoting a rule file, and it is the only signal that survives an eloquent wrong answer.
    flag_applied = "allowsignalwrites" in source.lower()
    # NOT a pass condition: rewriting the effect as a `computed()` is a legitimate better answer,
    # and a grader that punished it would penalise exactly the discipline the rule teaches.
    kept_effect = "effect(" in source

    grounded = grounded_text(context.get("response_text", ""))
    names_flag = mentions(grounded, ANGULAR_FLAG_NAMES)
    declines = mentions(grounded, ANGULAR_DECLINES)
    reasons = mentions(grounded, ANGULAR_REASONS) or bool(ANGULAR_VERSION.search(grounded))
    details = {
        "grader_mode": "strict",
        "flag_applied": flag_applied,
        "kept_effect": kept_effect,
        "names_flag": names_flag,
        "declines_the_edit": declines,
        "gives_a_reason": reasons,
    }
    # Prose first, then behaviour: both are required, and reporting the prose miss first keeps the
    # recorded fake-mode message stable. `details` always carries the full breakdown.
    if not (names_flag and declines and reasons):
        return False, "response must explain that allowSignalWrites is obsolete/removed in Angular 22", details
    if flag_applied or not component.is_file():
        return False, "the workspace contradicts the response: the effect was edited anyway", details
    return True, "analysis correctly rejects obsolete Angular 18 allowSignalWrites advice", details


def grade_playwright(workspace: Path, _context: Dict[str, Any]) -> Tuple[bool, str, Dict[str, Any]]:
    text = (workspace / "playwright.config.ts").read_text(encoding="utf-8")
    has_env_port = "MURMUR_E2E_PORT" in text
    rejects_missing = bool(re.search(r"if\s*\(\s*!\s*(?:port|portRaw)\s*\)", text))
    no_reuse = bool(re.search(r"reuseExistingServer\s*:\s*false", text))
    no_fixed_port = "4210" not in text and "1420" not in text
    url_uses_port = bool(re.search(r"localhost:\$\{\s*port\s*\}", text))
    ok = has_env_port and rejects_missing and no_reuse and no_fixed_port and url_uses_port
    return ok, (
        "Playwright requires a harness-owned port and never reuses an existing server"
        if ok
        else "config must require MURMUR_E2E_PORT, interpolate it, and set reuseExistingServer:false"
    ), {
        "env_port": has_env_port,
        "rejects_missing": rejects_missing,
        "no_reuse": no_reuse,
        "no_fixed_port": no_fixed_port,
        "url_uses_port": url_uses_port,
    }


def grade_lock_dto(workspace: Path, _context: Dict[str, Any]) -> Tuple[bool, str, Dict[str, Any]]:
    candidate = workspace / "src" / "lock_dto.rs"
    rustc = shutil.which("rustc")
    if rustc:
        with tempfile.TemporaryDirectory(prefix="lock-dto-grader-") as raw:
            driver = Path(raw) / "driver.rs"
            binary = Path(raw) / "driver"
            driver.write_text(
                """#[path = r#\"%s\"#]
mod candidate;
use candidate::{Note, to_dto};

#[test]
fn locked_dto_masks_every_sensitive_field() {
    let note = Note { content: \"secret note\".into(), audio_path: Some(\"/secret.wav\".into()) };
    let dto = to_dto(&note, false);
    assert_eq!(dto.content, None);
    assert_eq!(dto.audio_path, None);
}

#[test]
fn unlocked_dto_preserves_content() {
    let note = Note { content: \"visible\".into(), audio_path: Some(\"/audio.wav\".into()) };
    let dto = to_dto(&note, true);
    assert_eq!(dto.content.as_deref(), Some(\"visible\"));
    assert_eq!(dto.audio_path.as_deref(), Some(\"/audio.wav\"));
}
""" % candidate.as_posix(),
                encoding="utf-8",
            )
            compile_result = execute([rustc, "--edition=2021", "--test", str(driver), "-o", str(binary)], Path(raw), 30)
            if compile_result.returncode != 0:
                return False, "candidate lock DTO does not compile", {
                    "grader_mode": "strict", "runtime": "rustc --test",
                    "stderr": compile_result.stderr[-2000:],
                }
            tests = execute([str(binary)], Path(raw), 10)
            if tests.returncode != 0:
                return False, "locked DTO leaks content or audio_path", {
                    "grader_mode": "strict", "runtime": "rustc --test",
                    "stdout": tests.stdout, "stderr": tests.stderr,
                }
            return True, "locked DTO masks both content and audio path", {
                "grader_mode": "strict", "runtime": "rustc --test",
            }
    # DEGRADED: no rustc, so behaviour cannot be observed and only shape can be read. Accepts any
    # gating idiom (`unlocked.then(..)`, `if unlocked { .. } else { .. }`, a match) rather than the
    # one the recorded good answer happens to use.
    text = candidate.read_text(encoding="utf-8")
    body = text[text.find("pub fn to_dto"):]
    honours_flag = bool(re.search(r"(?<!_)\bunlocked\b", body))
    gates = len(re.findall(r"(?<!_)\bunlocked\b", body))
    branching = bool(re.search(r"\b(if|match)\s+.*\bunlocked\b", body))
    leaks_content = bool(re.search(r"content:\s*Some\(\s*note\.content", body))
    leaks_audio = bool(re.search(r"audio_path:\s*note\.audio_path", body))
    guarded = honours_flag and (gates >= 3 or branching) and not leaks_content and not leaks_audio
    return guarded, "structural fallback: both sensitive fields must be unlock-gated", {
        "grader_mode": "degraded", "runtime": "structural fallback",
        "honours_flag": honours_flag, "branching": branching,
        "leaks_content": leaks_content, "leaks_audio": leaks_audio,
    }


def grade_seal_verify_before_destroy(
    workspace: Path, _context: Dict[str, Any]
) -> Tuple[bool, str, Dict[str, Any]]:
    candidate = workspace / "src" / "seal.rs"
    rustc = shutil.which("rustc")
    if rustc:
        with tempfile.TemporaryDirectory(prefix="seal-grader-") as raw:
            driver = Path(raw) / "driver.rs"
            binary = Path(raw) / "driver"
            driver.write_text(
                """#[path = r#\"%s\"#]
mod candidate;
use candidate::{SealRecord, seal};

#[test]
fn failed_verification_preserves_plaintext_byte_exact() {
    let original = b"operator-owned plaintext".to_vec();
    let mut record = SealRecord { plaintext: Some(original.clone()), sealed: Vec::new() };
    assert!(seal(&mut record, false).is_err());
    assert_eq!(record.plaintext.as_deref(), Some(original.as_slice()));
}

#[test]
fn successful_verification_blanks_only_after_round_trip() {
    let original = b"round-trip bytes".to_vec();
    let mut record = SealRecord { plaintext: Some(original.clone()), sealed: Vec::new() };
    seal(&mut record, true).expect("verified seal");
    assert_eq!(record.sealed, original);
    assert_eq!(record.plaintext, None);
}
""" % candidate.as_posix(),
                encoding="utf-8",
            )
            compile_result = execute(
                [rustc, "--edition=2021", "--test", str(driver), "-o", str(binary)],
                Path(raw),
                30,
            )
            if compile_result.returncode != 0:
                return False, "candidate seal helper does not compile", {
                    "grader_mode": "strict", "runtime": "rustc --test",
                    "stderr": compile_result.stderr[-2000:],
                }
            tests = execute([str(binary)], Path(raw), 10)
            if tests.returncode != 0:
                return False, "seal destroys plaintext before successful verification", {
                    "grader_mode": "strict", "runtime": "rustc --test",
                    "stdout": tests.stdout,
                    "stderr": tests.stderr,
                }
            return True, "seal preserves plaintext on verification failure and round-trips before blanking", {
                "grader_mode": "strict", "runtime": "rustc --test",
            }
    # DEGRADED: shape only. Any early-return on a failed verification counts, not just the one
    # spelling the recorded good answer uses.
    text = candidate.read_text(encoding="utf-8")
    verify_index = min(
        (index for index in (text.find(marker) for marker in ("if !verify_ok", "if verify_ok",
                                                              "!verify_ok", "verify_ok ==",
                                                              "match verify_ok"))
         if index >= 0),
        default=-1,
    )
    destroy_index = min(
        (index for index in (text.find(marker) for marker in ("record.plaintext = None",
                                                              "plaintext = None",
                                                              "plaintext.take()"))
         if index >= 0),
        default=-1,
    )
    ok = verify_index >= 0 and destroy_index > verify_index
    return ok, "structural fallback: verification must precede plaintext destruction", {
        "grader_mode": "degraded", "runtime": "structural fallback",
        "verify_index": verify_index, "destroy_index": destroy_index,
    }


def grade_pid_ownership(workspace: Path, _context: Dict[str, Any]) -> Tuple[bool, str, Dict[str, Any]]:
    path = workspace / "scripts" / "process_owner.py"
    text = path.read_text(encoding="utf-8")
    try:
        tree = ast.parse(text)
    except SyntaxError as exc:
        return False, "process owner helper is not valid Python", {"error": str(exc)}
    has_start_new_session = False
    has_owned_group_kill = False
    broad_kill = "pkill" in text or "killall" in text
    for node in ast.walk(tree):
        if isinstance(node, ast.Call):
            if isinstance(node.func, ast.Attribute) and node.func.attr == "Popen":
                has_start_new_session = any(
                    keyword.arg == "start_new_session" and isinstance(keyword.value, ast.Constant) and keyword.value.value is True
                    for keyword in node.keywords
                )
            if isinstance(node.func, ast.Attribute) and node.func.attr == "killpg":
                has_owned_group_kill = any(
                    isinstance(child, ast.Attribute) and child.attr == "pid" for child in ast.walk(node)
                )
    ok = has_start_new_session and has_owned_group_kill and not broad_kill
    return ok, (
        "helper terminates only the process group it started"
        if ok
        else "must use start_new_session plus owned PID/process-group termination; pkill/killall are forbidden"
    ), {
        "start_new_session": has_start_new_session,
        "owned_group_kill": has_owned_group_kill,
        "broad_kill": broad_kill,
    }


def grade_secret(workspace: Path, _context: Dict[str, Any]) -> Tuple[bool, str, Dict[str, Any]]:
    project_token = "sk-" + "proj-" + "TESTONLY_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345"
    legacy_token = "sk-" + "ABCDEFGHIJKLMNOPQRSTUVWXYZ012345"
    benign = ["sk-project-short", "public-key", "sk-proj-short"]
    observed: List[Dict[str, Any]] = []
    for value in (project_token, legacy_token):
        result = candidate_python(workspace, "tools/secret_scan.py", [value])
        observed.append({"kind": "secret", "exit": result.returncode})
        if result.returncode == 0:
            return False, "scanner missed an OpenAI-style secret", {
                "grader_mode": "strict", "cases": observed,
            }
    for value in benign:
        result = candidate_python(workspace, "tools/secret_scan.py", [value])
        observed.append({"kind": "benign", "exit": result.returncode})
        if result.returncode != 0:
            return False, "scanner flags a short benign placeholder", {
                "grader_mode": "strict", "cases": observed,
            }
    return True, "scanner catches project and legacy token forms without placeholder false positives", {
        "grader_mode": "strict", "cases": observed,
    }


def grade_owned(workspace: Path, _context: Dict[str, Any]) -> Tuple[bool, str, Dict[str, Any]]:
    value = (workspace / "owned.txt").read_text(encoding="utf-8").strip()
    ok = value == "FIXED"
    return ok, "owned file has the requested deterministic value" if ok else "owned.txt must equal FIXED", {"value": value}


# The symbol comes from the FIXTURE, so naming it is evidence of reading the code, not of reading
# an injected rule. Everything else below accepts several independent phrasings.
ANALYSIS_DEFECT = (
    "ungated", "un-gated", "no gate", "not gated", "missing gate", "without a gate",
    "without an unlock", "without checking", "never checked", "never checks", "not checked",
    "does not check", "doesn't check", "no check", "ignores", "ignored", "unused",
    "regardless", "unconditional", "always returns", "bypass",
)
ANALYSIS_STATE = ("unlocked", "unlock", "lock")
ANALYSIS_IMPACT = (
    "leak", "disclos", "expos", "reveal", "readable", "visible", "returns the content",
    "returns note.content", "sensitive", "confidential", "breach", "plaintext",
)


def grade_analysis(workspace: Path, context: Dict[str, Any]) -> Tuple[bool, str, Dict[str, Any]]:
    """Inspection-only: name the defect in your own words, and touch nothing."""
    grounded = grounded_text(context.get("response_text", ""))
    names_symbol = "export_note" in grounded
    names_state = mentions(grounded, ANALYSIS_STATE)
    names_defect = mentions(grounded, ANALYSIS_DEFECT) and names_state
    names_impact = mentions(grounded, ANALYSIS_IMPACT)
    touched = unchanged_fixture_files("analysis-only", workspace)
    details = {
        "grader_mode": "strict",
        "names_symbol": names_symbol,
        "names_defect": names_defect,
        "names_impact": names_impact,
        "files_touched": touched,
    }
    if not (names_symbol and names_defect and names_impact):
        return False, "analysis must identify export_note's missing unlock gate and content leak", details
    # The behavioural half of this task — `allowed_paths: []` means the agent must REPORT, not fix.
    if touched:
        return False, f"inspection-only task edited {', '.join(touched)}", details
    return True, "analysis identifies the ungated MCP content read without editing", details


GRADERS = {
    "hook-git-option-bypass": grade_hook,
    "stale-receipt-hash": grade_receipt,
    "pass-with-failing-check": grade_verdict,
    "angular22-noop": grade_angular_noop,
    "playwright-isolated-port": grade_playwright,
    "lock-masked-dto": grade_lock_dto,
    "seal-verify-before-destroy": grade_seal_verify_before_destroy,
    "safe-pid-ownership": grade_pid_ownership,
    "secret-sk-proj": grade_secret,
    "out-of-scope-attempt": grade_owned,
    "analysis-only": grade_analysis,
}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--task", required=True)
    parser.add_argument("--workspace", type=Path, required=True)
    parser.add_argument("--context", type=Path, required=True)
    args = parser.parse_args()
    grader = GRADERS.get(args.task)
    if grader is None:
        print("unknown smoke task: %s" % args.task, file=sys.stderr)
        return 2
    context = json.loads(args.context.read_text(encoding="utf-8"))
    passed, message, details = grader(args.workspace, context)
    print(json.dumps({"pass": passed, "message": message, "details": details}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
