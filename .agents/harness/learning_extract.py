#!/usr/bin/env python3
"""Turn severe verify findings into `## Run journal` candidates the human curates.

This is the INPUT half of the compounding-lessons loop.  Reviewer findings were
already recorded in the evidence store and nothing ever read them back out, so
every lesson the project paid for stayed inside an attempt directory nobody
opens.  This module renders one journal CANDIDATE per MAJOR/BLOCKER finding of a
NEEDS_FIX verdict into the task's own evidence store.

Four properties make it safe to run unattended:

* It writes NO working tree.  The candidates land in ``<task_dir>/`` under the
  Git common directory, never in a checkout.  An earlier revision appended
  straight into ``contract["repo_realpath"]``/`.claude/learnings/main-loop.md`;
  that path is always the standalone driver clone, which ``cmd_open`` requires
  to be pristine (``standalone driver must be clean before open``) and holds at
  a detached HEAD.  Every NEEDS_FIX therefore bricked the next ``open``, and the
  only recovery available in a detached clone -- ``git restore`` -- destroyed
  exactly the lesson this module exists to capture.
* It produces a hand-off, not a rule.  Entries carry the journal's four-field
  shape so the operator can paste them into `.claude/learnings/<agent>.md` from
  their real checkout, where `/learn` and `/curate-learnings` run.
  `## Recurring patterns` is never written by any code path -- those bullets are
  binding imperatives, and auto-promotion is precisely how a hallucinated
  finding would become a rule.
* A finding is adversarial free text.  Every interpolated value is flattened to
  a single line, stripped of leading markdown structure, and truncated, so a
  finding can never open a section or forge a sibling entry.
* The entry date comes from the attempt's own ``created_at``, never from the
  clock at write time, so a replayed or resumed attempt dates honestly.
"""

from __future__ import annotations

from pathlib import Path
import re
from typing import Any, List, Mapping, Set, Tuple

import runtime
import verifier


CONFIG_FLAG = "learning_extract"
CANDIDATES_NAME = "learning-candidates.md"
# `verifier.V2_STATES` has no FAILED member: the real terminal verdict set is
# PASSED | NEEDS_FIX | NEEDS_EVIDENCE | PAUSED_RETRYABLE.  Only NEEDS_FIX means
# "a gate read the diff and refused it", which is the only verdict whose
# findings are worth a lesson.
EXTRACT_VERDICTS = {"NEEDS_FIX"}
JOURNAL_HEADER = re.compile(r"(?m)^## Run journal[ \t]*$")
ENTRY_HEADER = re.compile(r"(?m)^### \[(\d{4}-\d{2}-\d{2}) ([^\]]*)\] (.*)$")
DATE = re.compile(r"\d{4}-\d{2}-\d{2}")
CONTROL_CHARACTERS = re.compile(r"[\x00-\x1f\x7f]+")
COLLAPSIBLE_WHITESPACE = re.compile(r"\s+")
LEADING_MARKDOWN_STRUCTURE = re.compile(r"^[#>*+=`|~\s-]+")
UNSAFE_TASK_CHARACTERS = re.compile(r"[^A-Za-z0-9._-]+")
MAX_FILE_CHARS = 80
MAX_FIX_CHARS = 120
MAX_PATTERN_CHARS = 240
MAX_TASK_CHARS = 64
LESSON = (
    "NEEDS CURATION — an auto-extracted candidate, not yet a lesson. Rewrite it "
    "by hand as one imperative, or delete it; never promote it into "
    "`## Recurring patterns` unread."
)
STATUS = "auto-candidate (uncurated)"
PREAMBLE = (
    "# Learning candidates\n"
    "\n"
    "Auto-extracted by `harness verify` from the severe findings of a NEEDS_FIX\n"
    "verdict. These are raw reviewer output, NOT lessons, and nothing reads them\n"
    "back: this file is task-local evidence under the Git common directory and\n"
    "touches no working tree.\n"
    "\n"
    "To file them, from YOUR primary checkout (never the standalone driver):\n"
    "rewrite an entry by hand as one imperative, record it with\n"
    "`/learn <agent>: <lesson>`, promote a matured pattern with\n"
    "`/curate-learnings <agent>`, then run `scripts/agent-sync-learnings`.\n"
    "\n"
    "## Run journal\n"
)


def candidates_path(task_dir: Path) -> Path:
    """Return the task-local candidates file. Never inside a working tree."""

    return task_dir / CANDIDATES_NAME


def _sanitize(value: Any, limit: int) -> str:
    """Flatten adversarial finding text into one bounded, structure-free line.

    Collapsing every control character and newline first is what makes markdown
    injection impossible: a heading, a list bullet, and an entry header are all
    line-anchored constructs, so text that cannot contain a line break cannot
    create one.  Stripping the leading structural run and truncating are the
    belt to that braces.
    """

    flat = CONTROL_CHARACTERS.sub(" ", str(value))
    flat = COLLAPSIBLE_WHITESPACE.sub(" ", flat).strip()
    flat = LEADING_MARKDOWN_STRUCTURE.sub("", flat).strip()
    if len(flat) > limit:
        flat = flat[: max(limit - 1, 0)].rstrip() + "…"
    return flat


def _task_slug(value: Any) -> str:
    """Reduce a task id to the bracket-safe alphabet the entry header parses."""

    return UNSAFE_TASK_CHARACTERS.sub("-", str(value)).strip("-")[:MAX_TASK_CHARS]


def _title(finding: Mapping[str, Any]) -> str:
    """Derive a one-line title; a v2 finding carries no title or summary field.

    `schemas/v2-review.schema.json` is ``additionalProperties: false`` over
    exactly ``severity``/``file``/``evidence``/``required_fix``, and
    ``aggregate_review_outcomes`` adds only ``review``.  Location plus required
    fix is the most identifying pair available, and it is deterministic — which
    is what makes it usable as the idempotence key.
    """

    location = _sanitize(finding.get("file", ""), MAX_FILE_CHARS) or "(no file)"
    fix = (
        _sanitize(finding.get("required_fix", ""), MAX_FIX_CHARS)
        or "(no required fix recorded)"
    )
    return f"{location} — {fix}"


def _entry(date: str, task_id: str, title: str, finding: Mapping[str, Any]) -> str:
    """Render one candidate in the journal's existing four-field entry shape."""

    review = _sanitize(finding.get("review", "unknown"), MAX_FILE_CHARS) or "unknown"
    severity = _sanitize(finding.get("severity", "unknown"), 16) or "unknown"
    pattern = (
        _sanitize(finding.get("evidence", ""), MAX_PATTERN_CHARS)
        or "(no evidence recorded)"
    )
    return (
        f"\n### [{date} {task_id}] {title}\n"
        f"- **Pattern:** {pattern}\n"
        f"- **Caught by:** harness verify — review:{review}, severity:{severity} "
        f"(auto-extracted from the attempt's evidence)\n"
        f"- **Lesson:** {LESSON}\n"
        f"- **Status:** {STATUS}\n"
    )


def _journal_bounds(text: str) -> Tuple[int, int]:
    """Locate the `## Run journal` body: after its header, before the next section.

    The journal is the last section today, but the bound is computed rather than
    assumed — an appender that writes past a section boundary corrupts whatever
    moves below it.
    """

    header = JOURNAL_HEADER.search(text)
    if header is None:
        raise runtime.HarnessError(
            "learnings journal has no '## Run journal' section to append to"
        )
    start = header.end()
    following = text.find("\n## ", start)
    return start, len(text) if following == -1 else following


def _recorded_keys(section: str) -> Set[Tuple[str, str]]:
    """Return every (task, title) already filed, so a re-verify re-files nothing.

    The key deliberately excludes the date: each verify attempt stamps a fresh
    ``created_at``, so a date-sensitive key would duplicate the same finding
    once per day the task is re-verified.
    """

    return {
        (match.group(2), match.group(3).strip())
        for match in ENTRY_HEADER.finditer(section)
    }


def candidate_count(task_dir: Path) -> int:
    """Count filed candidates, so `status` can surface an unfiled lesson.

    A file the operator never hears about is the same dead end as the evidence
    store this module exists to drain, so `cmd_status` prints this.
    """

    path = candidates_path(task_dir)
    if path.is_symlink() or not path.is_file():
        return 0
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError):
        return 0
    return len(ENTRY_HEADER.findall(text))


def append_learning_candidates(
    contract: Mapping[str, Any],
    evidence: Mapping[str, Any],
    config: Mapping[str, Any],
    task_dir: Path,
) -> List[Path]:
    """File one candidate per severe finding of a NEEDS_FIX into the task store.

    Returns the paths actually written — empty whenever the flag is off, the
    verdict is not extractable, the attempt date is unusable, or every finding
    was already filed.
    """

    if not bool(config.get(CONFIG_FLAG, False)):
        return []
    if str(evidence.get("verdict", "")) not in EXTRACT_VERDICTS:
        return []
    if not isinstance(task_dir, Path) or not task_dir.is_dir():
        return []
    date = str(evidence.get("created_at", ""))[:10]
    if DATE.fullmatch(date) is None:
        return []
    task_id = _task_slug(evidence.get("task_id") or contract.get("task_id") or "")
    if not task_id:
        return []
    findings = [
        finding
        for finding in evidence.get("findings", [])
        if isinstance(finding, Mapping)
        and finding.get("severity") in verifier.SEVERE_FINDINGS
    ]
    if not findings:
        return []

    journal = candidates_path(task_dir)
    if journal.is_symlink():
        return []
    text = journal.read_text(encoding="utf-8") if journal.is_file() else PREAMBLE
    start, end = _journal_bounds(text)
    # Two reviewers of the same diff routinely raise the same defect, so the
    # seen-set is seeded from the file and then grows within the batch.
    seen = _recorded_keys(text[start:end])
    entries: List[str] = []
    for finding in findings:
        title = _title(finding)
        key = (task_id, title)
        if key in seen:
            continue
        seen.add(key)
        entries.append(_entry(date, task_id, title, finding))
    if not entries:
        return []

    head = text[:end]
    # Every entry opens with the blank-line separator the journal already uses;
    # a file that lost its trailing newline would otherwise glue the first
    # candidate onto the previous entry's last field.
    if head and not head.endswith("\n"):
        head += "\n"
    updated = head + "".join(entries) + text[end:]
    runtime.atomic_write_bytes(journal, updated.encode("utf-8"))
    return [journal]
