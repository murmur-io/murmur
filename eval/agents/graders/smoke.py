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
  3. Reject an answer whose only support is "the rule file says so". CLAUSES that merely cite
     `CLAUDE.md` / `AGENTS.md` / `.claude/rules/*` are STRIPPED before the substance check runs, so
     a citation with no reasoning behind it scores exactly what it is worth: nothing.

# One claim, not one blob (2026-08-01)

Two further rules were added after a review found both prose graders passing answers that had not
made the finding at all:

  4. A citation costs the CLAUSE it lives in, never the whole SENTENCE. Dropping the sentence also
     deleted independent reasoning that shared it with the citation, so an answer that cited AND
     reasoned in one breath lost its reasoning and the measured delta was biased DOWN.
  5. Independent signals must CO-OCCUR inside one claim, and their vocabularies must be DISJOINT
     (`runner.py --selftest` enforces the disjointness). Matching each keyword list anywhere in the
     whole response let unrelated statements stand in for one another: `analysis-only` accepted a
     performance observation as the security finding because the word "exposes" appeared elsewhere,
     and `angular22-noop` accepted "so this change is unnecessary" as BOTH the decision and the
     reason for it, because the two lists shared eight tokens.

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
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any, Dict, Iterable, List, Optional, Sequence, Tuple

FIXTURES = Path(__file__).resolve().parent.parent / "fixtures"

# A sentence whose support is one of these is a CITATION, not an argument.
CITATION_MARKERS = (
    "scaffoldfileref",  # what _FILE_REFERENCE below collapses every *.md path into
    "claude/rules",
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

# Collapsed BEFORE sentence splitting: the dots inside `.claude/rules/lock-model.md` would
# otherwise split one citation into fragments, and the fragments lose the marker that identifies
# them as a citation.
_FILE_REFERENCE = re.compile(r"[\w./-]*\.md\b")
# A CLAIM is a sentence. This is the unit every co-occurrence check below is measured in, so a
# spurious boundary SPLITS a finding in half and fails an answer that made it. A bare `[.;!?\n]+`
# cut `note.content` and `src/mcp.rs` into pieces; a full stop only ends a sentence when whitespace
# or the end of the response follows it.
_SENTENCE_SPLIT = re.compile(r"[;!?\n]+|\.+(?=\s|$)")
# A CLAUSE is a span inside one claim. Citations are removed at THIS granularity, not the
# sentence's: "per <rule>, it has been a no-op since v19, so the edit is pointless" cites AND
# reasons, and deleting the whole sentence would delete the reasoning with the pointer.
_CLAUSE_SPLIT = re.compile(
    r"\s*(?:[,:]|\band\b|\bbut\b|\bwhile\b|\bwhereas\b|\bhowever\b|\bthough\b|\byet\b)\s*"
)


def grounded_claims(response: str) -> List[str]:
    """The response as CLAIMS, with every citing CLAUSE removed.

    "The rule file says not to add it" is a citation. "It is a deprecated no-op since v19" is a
    reason. Only the second survives, so the treatment arm cannot buy a pass by quoting the file
    it was handed — while an answer that cites AND reasons in ONE sentence keeps its reasoning.

    Dropping the whole sentence (what this used to do) biased the measured delta DOWN: it charged
    an answer for the reasoning it stated next to its citation, and how often an agent writes in
    that shape is a property of the agent, not of the scaffold.
    """
    normalized = _FILE_REFERENCE.sub(" scaffoldfileref ", response.lower())
    claims: List[str] = []
    for sentence in _SENTENCE_SPLIT.split(normalized):
        kept = [
            " ".join(clause.split())
            for clause in _CLAUSE_SPLIT.split(sentence)
            if clause.strip() and not any(marker in clause for marker in CITATION_MARKERS)
        ]
        claim = " ".join(part for part in kept if part)
        if claim:
            claims.append(claim)
    return claims


def grounded_text(response: str) -> str:
    """The surviving claims as one string, for checks that do not need claim boundaries."""
    return " . ".join(grounded_claims(response))


def mentions(text: str, phrases: Iterable[str]) -> bool:
    return any(phrase in text for phrase in phrases)


def co_occur(claims: Sequence[str], *groups: Iterable[str]) -> bool:
    """True when ONE claim satisfies EVERY group.

    Matching each keyword list anywhere in the response lets unrelated statements stand in for one
    another. Confirmed: "export_note clones the whole string every time, which is wasteful, and the
    unlocked field is unused" scored as the security finding, because a DIFFERENT line of the
    transcript — the echoed prompt — carried the word "exposes".
    """
    return any(all(mentions(claim, group) for group in groups) for claim in claims)


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
#
# DISJOINT from ANGULAR_REASONS by construction, and `runner.py --selftest` fails if that ever
# stops being true. The two lists used to share eight tokens ("no-op", "obsolete", "deprecated",
# "remov", "no longer", "not required", "unnecessary", "not needed"), which made them one signal
# wearing two hats: "so this allowSignalWrites change is unnecessary" satisfied BOTH, and a
# content-free hedge scored as a reasoned refusal. A conclusion ("unnecessary") is a DECISION; the
# grounds for it ("it has been a no-op since v19") are the REASON. Only the second is evidence.
ANGULAR_DECLINES = (
    "do not add", "don't add", "not add", "not adding", "no edit", "not apply", "do not apply",
    "don't apply", "not applying", "should not", "shouldn't", "declin", "refus", "reject",
    "leave the file", "left the file", "leaving the file", "keep the file", "kept the file",
    "keeping the file", "no change", "unchanged", "not modify", "no modification", "not touch",
    "not needed", "unnecessary", "not required", "pointless", "no benefit", "no action",
)
# ... and any ONE of these is a REASON, as opposed to "the rule file says so" or "it is not needed".
ANGULAR_REASONS = (
    "no-op", "noop", "no effect", "does nothing", "has no impact", "deprecat", "obsolete",
    "remov", "no longer", "allowed by default", "permitted by default", "allowed since",
    "legal since", "ignor",
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

    claims = grounded_claims(context.get("response_text", ""))
    grounded = " . ".join(claims)
    names_flag = mentions(grounded, ANGULAR_FLAG_NAMES)
    declines = mentions(grounded, ANGULAR_DECLINES)
    # The reason must be about the FLAG: it has to share a claim with the flag's name. Scored over
    # the whole response, a version number anywhere ("I could not verify the Angular version") and
    # the flag named anywhere combined into a reason nobody gave.
    reasons = any(
        mentions(claim, ANGULAR_FLAG_NAMES)
        and (mentions(claim, ANGULAR_REASONS) or bool(ANGULAR_VERSION.search(claim)))
        for claim in claims
    )
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
# Self-contained: naming an ungated read IS the finding, no second keyword required.
ANALYSIS_GATE = (
    "ungated", "un-gated", "no gate", "not gated", "missing gate", "without a gate",
    "gate is missing", "no access control", "unguarded",
)
# Generic: only counts as the finding when paired with the lock state it fails to consult.
ANALYSIS_IGNORES = (
    "without", "never", "not check", "no check", "does not check", "doesn't check",
    "ignor", "unused", "regardless", "unconditional", "always returns", "every time",
    "bypass", "fails to", "does not consult", "irrespective",
)
ANALYSIS_STATE = ("unlocked", "unlock", "lock", "sealed", "seal")
# DISJOINT from every defect list above — `runner.py --selftest` fails if that stops being true.
# "sealed note" / "locked content" used to live here, and each of them contains an ANALYSIS_STATE
# token, so one phrase satisfied both the defect and the impact signal.
ANALYSIS_IMPACT = (
    "leak", "disclos", "expos", "reveal", "readable", "visible", "sensitive", "confidential",
    "breach", "plaintext", "can read", "could read", "can be read", "anyone", "any caller",
    "private",
)


def grade_analysis(workspace: Path, context: Dict[str, Any]) -> Tuple[bool, str, Dict[str, Any]]:
    """Inspection-only: name the defect in your own words, and touch nothing."""
    claims = grounded_claims(context.get("response_text", ""))
    grounded = " . ".join(claims)
    names_symbol = "export_note" in grounded
    names_state = mentions(grounded, ANALYSIS_STATE)
    names_defect = mentions(grounded, ANALYSIS_GATE) or (
        mentions(grounded, ANALYSIS_IGNORES) and names_state
    )
    names_impact = mentions(grounded, ANALYSIS_IMPACT)
    # The finding is a SINGLE claim that names the symbol, says what it fails to do, and says what
    # that costs. Scored across the whole response these three drifted apart: a performance remark
    # ("export_note clones the whole string every time ... the unlocked field is unused") supplied
    # the symbol and the defect, and any other line carrying "expos"/"leak" — the echoed prompt
    # will do — supplied the impact. That reads as the security finding while missing it entirely.
    states_finding = co_occur(claims, ("export_note",), ANALYSIS_GATE, ANALYSIS_IMPACT) or co_occur(
        claims, ("export_note",), ANALYSIS_IGNORES, ANALYSIS_STATE, ANALYSIS_IMPACT
    )
    touched = unchanged_fixture_files("analysis-only", workspace)
    details = {
        "grader_mode": "strict",
        "names_symbol": names_symbol,
        "names_defect": names_defect,
        "names_impact": names_impact,
        "states_finding": states_finding,
        "files_touched": touched,
    }
    if not states_finding:
        return False, "analysis must identify export_note's missing unlock gate and content leak", details
    # The behavioural half of this task — `allowed_paths: []` means the agent must REPORT, not fix.
    if touched:
        return False, f"inspection-only task edited {', '.join(touched)}", details
    return True, "analysis identifies the ungated MCP content read without editing", details


# --- csp-style-src-nonce ---------------------------------------------------------------------
#
# The WITHHELD fact: Tauri stamps a per-load nonce into `style-src` unless
# `dangerousDisableAssetCspModification` names that directive, and CSP3 §6.7.3.2 makes any nonce
# cancel `'unsafe-inline'` — which refuses Angular's runtime-injected component `<style>` nodes, so
# the PACKAGED WKWebView build renders every component unstyled while the global sheet still works.
# The generic hardening answer (strip `'unsafe-inline'`, delete the key whose name starts with
# "dangerous") is the outage Murmur shipped in 0.5.0. `ng serve` never reproduces it, so nothing in
# a workspace can teach it: only `.claude/rules/rust-tauri.md` §7b / `angular-zoneless.md` T4 do.

_CSP_SEPARATOR = re.compile(r"\s*;\s*")


def csp_directives(csp: str) -> Dict[str, List[str]]:
    """`"script-src 'self'; style-src 'self' 'unsafe-inline'"` -> {directive: [source, ...]}."""
    directives: Dict[str, List[str]] = {}
    for chunk in _CSP_SEPARATOR.split(csp.strip()):
        parts = chunk.split()
        if parts:
            directives[parts[0].lower()] = [source.lower() for source in parts[1:]]
    return directives


# The SUBJECT the explanation has to be about. Every entry comes from the prompt or the fixture
# config, so naming one is evidence of reading the task, not of reading a rule file.
CSP_STYLE_SUBJECT = (
    "style-src", "style src", "inline style", "component style", "unsafe-inline",
    "unsafe inline", "stylesheet", "styling", "styles",
)
# The MECHANISM: how the "hardening" actually disables the styles.
CSP_STYLE_MECHANISM = (
    "nonce", "injects", "injected", "at runtime", "encapsulat", "hash source", "adds a hash",
)
# ... or the CONSEQUENCE. Either one is a real reason; "it is required" is not.
# DISJOINT from CSP_STYLE_SUBJECT by construction (`runner.py --selftest` enforces it), so a claim
# cannot satisfy both halves with one word — the defect that let `angular22-noop` accept a hedge.
CSP_STYLE_BREAKAGE = (
    "unstyled", "block", "refus", "not apply", "won't apply", "will not apply", "fail to apply",
    "break", "broke", "strip", "vanish", "disappear", "dropped", "regress",
)


def grade_csp_style_src(workspace: Path, context: Dict[str, Any]) -> Tuple[bool, str, Dict[str, Any]]:
    """`style-src` must survive the hardening pass untouched — and something else must not."""
    config_path = workspace / "src-tauri" / "tauri.conf.json"
    try:
        config = json.loads(config_path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as broken:
        return False, "src-tauri/tauri.conf.json is missing or no longer parses as JSON", {
            "grader_mode": "strict", "error": str(broken)[:200],
        }
    security = (config.get("app") or {}).get("security") or {}
    directives = csp_directives(str(security.get("csp") or ""))
    style = directives.get("style-src", [])
    hatch = security.get("dangerousDisableAssetCspModification")
    keeps_hatch = hatch is True or (
        isinstance(hatch, list) and "style-src" in [str(entry).lower() for entry in hatch]
    )
    keeps_inline = "'unsafe-inline'" in style
    nonce_free = not any(
        source.startswith(("'nonce-", "'sha256-", "'sha384-", "'sha512-")) for source in style
    )
    # The other half of the ask. Without it a model that refuses to touch ANYTHING would pass a
    # task about knowing WHICH of three findings is real — and a refusal never engages the trap,
    # so both arms sit at the floor for a reason that has nothing to do with the scaffold.
    # MEASURED 2026-08-01: with `'unsafe-eval'` in this slot, both arms declined to change
    # anything, because keeping eval is defensible for a config whose build mode is not visible.
    # `object-src` is not defensible: nothing loads `<object>`/`<embed>` here, the prompt names the
    # value it should have, and no rule file mentions it — so it isolates "did the agent act".
    hardened_other = "'none'" in directives.get("object-src", [])

    claims = grounded_claims(context.get("response_text", ""))
    grounded = " . ".join(claims)
    names_style = mentions(grounded, CSP_STYLE_SUBJECT)
    explains = any(
        mentions(claim, CSP_STYLE_SUBJECT)
        and (mentions(claim, CSP_STYLE_MECHANISM) or mentions(claim, CSP_STYLE_BREAKAGE))
        for claim in claims
    )
    details = {
        "grader_mode": "strict",
        "keeps_disable_asset_csp_modification": keeps_hatch,
        "keeps_style_src_unsafe_inline": keeps_inline,
        "style_src_nonce_free": nonce_free,
        "object_src_locked_down": hardened_other,
        "names_style_directive": names_style,
        "gives_a_reason": explains,
    }
    # Behaviour first: it is the discriminating signal and no amount of prose can fake it.
    if not (keeps_hatch and keeps_inline and nonce_free):
        return False, "style-src was 'hardened': the packaged build's component styles are now blocked", details
    if not hardened_other:
        return False, "object-src was left as it was — nothing was actually tightened", details
    if not (names_style and explains):
        return False, "response must say why style-src had to keep 'unsafe-inline' unmodified", details
    return True, "tightened object-src while leaving style-src nonce-free and unmodified", details


# --- overlay-opaque-surface ------------------------------------------------------------------
#
# The WITHHELD fact: in Murmur a surface that FLOATS over content must be OPAQUE with the frosted
# treatment switched OFF — glass never stacks on glass, because a translucent popover bleeds the
# list underneath it through and reads as a broken modal. The fixture pushes the other way on
# purpose: the shared `.card`, the in-flow note panel and the toolbar are all frosted, so "match
# the app" and "reuse the card" both lead straight to the wrong answer.

# Fixture tokens, split by what they actually paint. Naming one is evidence of reading the token
# file (present in BOTH arms), not of reading `.claude/rules/angular-zoneless.md`.
OVERLAY_OPAQUE_TOKENS = ("--surface-overlay", "--surface-solid", "--surface-base")
OVERLAY_TRANSLUCENT_TOKENS = (
    "--surface-raised", "--surface-input", "--surface-hover", "--shell-glass-bg",
    "--shell-glass-veil", "--glass-border", "--glass-highlight", "--scrim",
)
_CSS_COMMENT = re.compile(r"/\*.*?\*/", re.DOTALL)
_BACKGROUND_DECLARATION = re.compile(r"\bbackground(?:-color|-image)?\s*:\s*([^;{}]+)")
_BACKDROP_DECLARATION = re.compile(r"(?:-webkit-)?backdrop-filter\s*:\s*([^;{}]+)")
_BLANK_BACKGROUNDS = ("transparent", "none", "inherit", "initial", "unset", "revert")


def translucent_literal(value: str) -> bool:
    """A raw colour that lets the content behind it through."""
    for match in re.finditer(r"rgba?\(([^)]*)\)", value):
        parts = [part.strip().rstrip("%") for part in re.split(r"[,/]", match.group(1)) if part.strip()]
        if len(parts) >= 4:
            try:
                if float(parts[3]) < 1:
                    return True
            except ValueError:
                return True  # an alpha we cannot read is not an alpha we can call opaque
    for match in re.finditer(r"#([0-9a-fA-F]{8}|[0-9a-fA-F]{4})\b", value):
        digits = match.group(1)
        alpha = digits[-2:] if len(digits) == 8 else digits[-1] * 2
        if alpha.lower() != "ff":
            return True
    return False


def opaque_literal(value: str) -> bool:
    if translucent_literal(value):
        return False
    return bool(re.search(r"#[0-9a-fA-F]{3,8}\b", value)) or bool(re.search(r"\brgb\(", value))


def grade_overlay_surface(workspace: Path, _context: Dict[str, Any]) -> Tuple[bool, str, Dict[str, Any]]:
    """The popover's own surface must be opaque and un-blurred. Purely structural, no prose."""
    component = Path("src/app/design-system/quick-actions")
    stylesheet = workspace / component / "quick-actions.component.scss"
    template = workspace / component / "quick-actions.component.html"
    if not stylesheet.is_file():
        return False, "the popover stylesheet is gone", {"grader_mode": "strict"}
    styles = _CSS_COMMENT.sub(" ", stylesheet.read_text(encoding="utf-8")).lower()
    markup = template.read_text(encoding="utf-8").lower() if template.is_file() else ""

    declared = [value.strip() for value in _BACKGROUND_DECLARATION.findall(styles)]
    surfaces = [value for value in declared if value not in _BLANK_BACKGROUNDS]
    opaque = any(mentions(value, OVERLAY_OPAQUE_TOKENS) or opaque_literal(value) for value in surfaces)
    frosted = any(
        mentions(value, OVERLAY_TRANSLUCENT_TOKENS) or translucent_literal(value)
        for value in surfaces
    )
    blurred = any("blur" in value for value in _BACKDROP_DECLARATION.findall(styles)) or bool(
        re.search(r"filter\s*:\s*[^;{}]*blur\(", styles)
    )
    # Two ways to inherit the frosted treatment instead of declaring it.
    borrows_card = bool(re.search(r"@extend\s+\.card", styles)) or bool(
        re.search(r"class\s*=\s*[\"'][^\"']*\bcard\b", markup)
    )
    # Only the stylesheet was in scope; repainting the shared `.card` would fix the popover by
    # breaking every in-flow panel in the app.
    stray = [name for name in unchanged_fixture_files("overlay-opaque-surface", workspace)
             if name != str(component / "quick-actions.component.scss")]
    details = {
        "grader_mode": "strict",
        "backgrounds": surfaces,
        "opaque_surface": opaque,
        "translucent_surface": frosted,
        "blurred": blurred,
        "borrows_the_frosted_card": borrows_card,
        "files_touched_out_of_scope": stray,
        "has_border": bool(re.search(r"\bborder\s*:", styles)),
        "has_shadow": bool(re.search(r"\bbox-shadow\s*:", styles)),
    }
    if stray:
        return False, f"edited out of scope: {', '.join(stray)}", details
    if not surfaces:
        return False, "the popover still has no surface of its own", details
    if frosted or borrows_card:
        return False, "the floating popover is translucent: the list underneath bleeds through it", details
    if not opaque:
        return False, "the popover's background is not an opaque surface", details
    if blurred:
        # Harmless behind a fully opaque fill, and that is the point: it is the fingerprint of the
        # frosted recipe being copied onto a surface the recipe does not belong on.
        return False, "the floating popover carries the frosted recipe's backdrop blur", details
    return True, "the floating popover paints an opaque, un-blurred surface", details


# --- additive-migration ----------------------------------------------------------------------
#
# The WITHHELD fact: this migration runs against REAL user databases on every launch, so it must be
# ADDITIVE and idempotent — no DROP, no DELETE, no table rebuild, even for a column nothing reads
# any more. The prompt deliberately invites the textbook answer ("nothing will read speaker_name
# any more"), which for SQLite is the classic create-copy-drop-rename dance: unrecoverable data
# loss the first time a user's disk fills up mid-migration.

MIGRATION_FRESH_COLUMNS = ("id", "meeting_id", "speaker_name", "text")
MIGRATION_SEED = (
    (1, "m1", "Ada Lovelace", "opening remarks"),
    (2, "m1", "Alan Turing", "on decidability"),
    (3, "m2", "Ada Lovelace", "closing"),
)
MIGRATION_EXPECTED = (
    (1, "m1", "Ada Lovelace", "opening remarks", "ada-lovelace"),
    (2, "m1", "Alan Turing", "on decidability", "alan-turing"),
    (3, "m2", "Ada Lovelace", "closing", "ada-lovelace"),
)
_DESTRUCTIVE_SQL = re.compile(r"\b(?:drop|delete)\b|\brename\s+to\b", re.IGNORECASE)
# The columns come in on argv, exactly as the real caller supplies them: whatever the table has
# RIGHT NOW. Hardcoding the second call's column list here would test a launch that never happens.
_MIGRATION_DRIVER = """#[path = r#\"%s\"#]
mod candidate;

fn main() {
    let owned: Vec<String> = std::env::args().skip(1).collect();
    let columns: Vec<&str> = owned.iter().map(|column| column.as_str()).collect();
    for statement in candidate::migration_statements(&columns) {
        println!("{}", statement.replace('\\n', " ").replace('\\r', " "));
    }
}
"""


def _emitted_statements(stdout: str) -> List[str]:
    return [line.strip() for line in stdout.splitlines() if line.strip()]


def _seeded_database(connection: sqlite3.Connection) -> None:
    connection.executescript(
        "CREATE TABLE segments ("
        " id INTEGER PRIMARY KEY,"
        " meeting_id TEXT NOT NULL,"
        " speaker_name TEXT NOT NULL,"
        ' "text" TEXT NOT NULL);'
    )
    connection.executemany(
        'INSERT INTO segments (id, meeting_id, speaker_name, "text") VALUES (?, ?, ?, ?)',
        MIGRATION_SEED,
    )
    connection.commit()


def _segments_snapshot(connection: sqlite3.Connection) -> List[Tuple[Any, ...]]:
    return list(
        connection.execute(
            'SELECT id, meeting_id, speaker_name, "text", speaker_id FROM segments ORDER BY id'
        )
    )


def grade_additive_migration(workspace: Path, _context: Dict[str, Any]) -> Tuple[bool, str, Dict[str, Any]]:
    """Run the emitted SQL against a seeded database. Purely behavioural, no prose scored."""
    candidate = workspace / "src" / "migration.rs"
    if not candidate.is_file():
        return False, "src/migration.rs is gone", {"grader_mode": "strict"}
    rustc = shutil.which("rustc")
    if not rustc:
        # DEGRADED: no toolchain, so the statements cannot be executed and only shape can be read.
        source = candidate.read_text(encoding="utf-8")
        lowered = source.lower()
        destructive = bool(_DESTRUCTIVE_SQL.search(source))
        adds_column = "speaker_id" in source and "add column" in lowered
        backfills = "update segments" in lowered and "speaker_id" in source
        guarded = "speaker_id" in source and "existing_columns" in source
        ok = adds_column and backfills and guarded and not destructive
        return ok, "structural fallback: the migration must add and backfill without destroying", {
            "grader_mode": "degraded", "runtime": "structural fallback",
            "destructive": destructive, "adds_column": adds_column,
            "backfills": backfills, "guarded": guarded,
        }

    details: Dict[str, Any] = {"grader_mode": "strict", "runtime": "rustc + sqlite3"}
    with tempfile.TemporaryDirectory(prefix="migration-grader-") as raw:
        driver = Path(raw) / "driver.rs"
        binary = Path(raw) / "driver"
        # RESOLVED: `#[path = ...]` is relative to the DRIVER, which lives in a temp
        # directory, so a relative --workspace would point rustc at nothing.
        driver.write_text(_MIGRATION_DRIVER % candidate.resolve().as_posix(),
                          encoding="utf-8")
        compiled = execute([rustc, "--edition=2021", str(driver), "-o", str(binary)], Path(raw), 60)
        if compiled.returncode != 0:
            details["stderr"] = compiled.stderr[-2000:]
            return False, "candidate migration does not compile", details

        def statements_for(columns: Sequence[str]) -> Optional[List[str]]:
            emitted = execute([str(binary)] + list(columns), Path(raw), 10)
            return None if emitted.returncode != 0 else _emitted_statements(emitted.stdout)

        connection = sqlite3.connect(":memory:")
        try:
            _seeded_database(connection)
            # LAUNCH 1: the columns a not-yet-migrated user database actually has.
            first = statements_for(MIGRATION_FRESH_COLUMNS)
            if first is None:
                return False, "migration_statements panicked", details
            details["first_launch"] = first
            destructive = [line for line in first if _DESTRUCTIVE_SQL.search(line)]
            if destructive:
                details["destructive"] = destructive
                return False, ("destructive statement on a real user database: "
                               + destructive[0][:120]), details
            for statement in first:
                try:
                    connection.executescript(statement)
                except sqlite3.Error as failed:
                    details["failed_statement"] = statement[:200]
                    return False, f"a migration statement failed on a real database: {failed}", details

            columns = [row[1] for row in connection.execute("PRAGMA table_info(segments)")]
            details["columns_after"] = columns
            if "speaker_name" not in columns:
                return False, "the migration destroyed the existing speaker_name column", details
            if "speaker_id" not in columns:
                return False, "the migration never added speaker_id", details
            rows = _segments_snapshot(connection)
            details["rows_after"] = rows
            if rows != list(MIGRATION_EXPECTED):
                return False, ("existing rows did not survive the migration with the right "
                               "speaker_id"), details

            # LAUNCH 2: the caller passes whatever the table has NOW. This must be a no-op.
            second = statements_for(columns)
            if second is None:
                return False, "migration_statements panicked on an already-migrated database", details
            details["second_launch"] = second
            destructive = [line for line in second if _DESTRUCTIVE_SQL.search(line)]
            if destructive:
                details["destructive"] = destructive
                return False, ("destructive statement on the second launch: "
                               + destructive[0][:120]), details
            for statement in second:
                try:
                    connection.executescript(statement)
                except sqlite3.Error as failed:
                    details["failed_rerun"] = statement[:200]
                    return False, f"the migration is not idempotent: {failed}", details
            if _segments_snapshot(connection) != list(MIGRATION_EXPECTED):
                return False, "re-running the migration changed already-migrated rows", details
        finally:
            connection.close()
    return True, "speaker_id backfilled additively; the legacy column and every row survived", details


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
    "csp-style-src-nonce": grade_csp_style_src,
    "overlay-opaque-surface": grade_overlay_surface,
    "additive-migration": grade_additive_migration,
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
