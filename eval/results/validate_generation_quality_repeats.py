#!/usr/bin/env python3
"""Validate and combine the two preregistered local-vs-Sol quality repetitions."""

from __future__ import annotations

import argparse
import gzip
import hashlib
import json
import math
import os
import re
import subprocess
import sys
from collections import defaultdict
from pathlib import Path
from typing import Any


QWEN4 = "qwen3-4b-instruct-2507-q4-k-m"
QWEN1 = "qwen3-1.7b-q4-k-m"
SOL = "gpt-5.6-sol-requested-high"
QWEN4_FILENAME = "Qwen_Qwen3-4B-Instruct-2507-Q4_K_M.gguf"
QWEN1_FILENAME = "Qwen_Qwen3-1.7B-Q4_K_M.gguf"
QWEN4_BYTES = 2_497_280_736
QWEN1_BYTES = 1_282_439_584
QWEN4_SHA256 = "2fde00ce69dd4899c70d020845e2638353015bba0fdf161b3eb965f2bca4464e"
QWEN1_SHA256 = "72c5c3cb38fa32d5256e2fe30d03e7a64c6c79e668ad84057e3bd66e250b24fb"
LOCAL_RUNTIME_VERSION = "murmur-brain-workspace-build"
CODEX_BINARY = Path("/opt/homebrew/bin/codex")
EXPECTED_ORDERS = {
    "1": [QWEN4, QWEN1, SOL],
    "2": [SOL, QWEN1, QWEN4],
}
HEAVY_CASE_IDS = {
    "summary-pl-kestrel",
    "summary-en-cedar",
    "meeting-chat-pl-delta",
    "note-popup-refine-pl",
    "note-popup-shorten-en",
    "note-popup-actions-pl",
    "note-popup-decisions-en",
    "note-popup-fact-check-pl",
    "ask-vault-pl-orchid",
    "summary-pl-lumen-holdout",
    "meeting-chat-en-fjord-holdout",
    "note-popup-actions-en-holdout",
    "ask-vault-en-quartz-holdout",
    "fact-extract-en-helix",
    "fact-extract-pl-zuraw",
}
LIGHT_CASE_IDS = {
    "live-current-en-nimbus",
    "live-current-pl-ember-holdout",
    "live-bullets-pl-polaris",
}
EXPECTED_CASE_IDS = {
    QWEN4: HEAVY_CASE_IDS,
    QWEN1: LIGHT_CASE_IDS,
    SOL: HEAVY_CASE_IDS | LIGHT_CASE_IDS,
}
REPO_ROOT = Path(__file__).resolve().parents[2]
MEASUREMENT_FILES = {
    "evaluatorFileSha256": REPO_ROOT / "src-tauri/src/eval/generation_quality.rs",
    "fixtureFileSha256": REPO_ROOT
    / "src-tauri/src/eval/fixtures/local-cloud-quality.json",
    "repeatValidatorFileSha256": Path(__file__).resolve(),
}
SOURCE_FILES = [
    "src/eval/generation_quality.rs",
    "src/eval/generation_retrieval.rs",
    "src/eval/fixtures/local-cloud-quality.json",
    "src/eval/fixtures/rag-bakeoff-synthetic.json",
    "src/eval/bakeoff.rs",
    "src/eval/corpus.rs",
    "src/eval/mod.rs",
    "Cargo.toml",
    ".cargo/config.toml",
    "../Cargo.toml",
    "../Cargo.lock",
    "../.cargo/config.toml",
    "../crates/murmur-brain/Cargo.toml",
    "../eval/results/validate_generation_quality_repeats.py",
    "src/summarize/local.rs",
    "src/summarize/claude_code.rs",
    "src/summarize/codex_cli.rs",
    "src/summarize/mod.rs",
    "src/summarize/provider.rs",
    "src/summarize/redact.rs",
    "src/summarize/ner_deberta.rs",
    "src/summarize/egress_log.rs",
    "src/summarize/meta.rs",
    "src/summarize/roles.rs",
    "src/summarize/action_items.rs",
    "src/summarize/temporal.rs",
    "src/summarize/related_context.rs",
    "src/summarize/template.rs",
    "src/summarize/chat.rs",
    "src/summarize/vault_chat.rs",
    "src/commands/enrich.rs",
    "src/commands/mod.rs",
    "src/commands/ask.rs",
    "src/agent.rs",
    "src/pipeline.rs",
    "src/transcribe/live.rs",
    "src/transcribe/bullets.rs",
    "src/transcribe/model.rs",
    "src/transcribe/mod.rs",
    "src/transcribe/types.rs",
    "src/audio/wake.rs",
    "src/voice_action.rs",
    "src/tools.rs",
    "src/facts.rs",
    "src/user_memory.rs",
    "src/commands/facts.rs",
    "src/brain_reactions.rs",
    "src/storage/db.rs",
    "src/storage/egress_store.rs",
    "src/storage/meetings_store.rs",
    "src/storage/notes_store.rs",
    "src/storage/models.rs",
    "src/storage/mod.rs",
    "src/storage/graph_store.rs",
    "src/embed.rs",
    "src/embed/candle_bert.rs",
    "src/perf.rs",
    "src/settings/config.rs",
    "src/settings/mod.rs",
    "src/settings/postures.rs",
    "src/prompts.rs",
    "src/reason.rs",
    "src/reason/sidecar.rs",
    "src/error.rs",
    "src/links.rs",
    "../crates/murmur-brain/src/main.rs",
    "../crates/murmur-brain/src/brain_ipc.rs",
]
REQUIRED_SOURCE_FILES = {
    "src/settings/postures.rs",
    "src/summarize/claude_code.rs",
    "src/summarize/action_items.rs",
    "src/summarize/temporal.rs",
    "src/summarize/related_context.rs",
    "src/summarize/meta.rs",
    "src/embed/candle_bert.rs",
    "src/perf.rs",
    "src/storage/models.rs",
    "src/storage/meetings_store.rs",
    "src/storage/notes_store.rs",
    "src/storage/graph_store.rs",
    "src/transcribe/model.rs",
    "src/transcribe/types.rs",
    "src/audio/wake.rs",
    "src/error.rs",
    "src/links.rs",
    "Cargo.toml",
    "../Cargo.toml",
    "../Cargo.lock",
    ".cargo/config.toml",
    "../.cargo/config.toml",
    "../crates/murmur-brain/Cargo.toml",
}
ARM_METADATA_KEYS = {
    "armId",
    "modelRequested",
    "effort",
    "effortTransport",
    "effortEffectiveAttested",
    "modelClass",
    "modelFilename",
    "modelBytes",
    "modelSha256",
    "runtimeVersion",
    "runtimeSha256",
    "sidecarIdleSecs",
    "sidecarReadySecs",
    "sidecarHardCapSecs",
}
SCORE_KEYS = {
    "diagnosticScore",
    "casePass",
    "criticalFailure",
    "requiredGroupsHit",
    "requiredGroupsTotal",
    "formatPass",
    "sectionPass",
    "languagePass",
    "forbiddenPass",
    "constraintPass",
    "provenancePass",
    "toolPolicyPass",
    "relationPass",
    "stateApplicationPass",
    "branchConvergencePass",
    "closedWorldPass",
    "structuredLabelsPass",
    "criticalErrors",
}
CASE_RESULT_KEYS = {
    "caseId",
    "casePayloadSha256",
    "surface",
    "language",
    "modelClass",
    "holdout",
    "routeInputSha256",
    "generationProfile",
    "productRoute",
    "comparisonScope",
    "routeInputChars",
    "outputChars",
    "outputSha256",
    "durationMs",
    "output",
    "surfaceOutput",
    "surfaceOutputSha256",
    "rawModelOutput",
    "rawModelOutputSha256",
    "rawModelFormatPass",
    "structuredSchemaPass",
    "structuredLabelsPass",
    "structuredEnvelopePass",
    "error",
    "toolSteps",
    "toolPolicyScore",
    "toolPolicyPass",
    "stateApplicationPass",
    "branchConverged",
    "provenance",
    "provenanceSha256",
    "toolStepsSha256",
    "egressReceiptStartOrdinal",
    "egressReceiptEndOrdinal",
    "egressReceiptCount",
    "egressReceiptSha256",
    "dimensions",
    "score",
    "caseRecordSha256",
}
DIMENSION_KEYS = {
    "retrievalQuality",
    "toolAgentExecution",
    "finalProductOutputContract",
}
DIMENSION_VERDICTS = {"pass", "fail", "not_measured", "not_applicable"}
DIMENSION_AGGREGATE_KEYS = {
    "observations",
    "applicableObservations",
    "measuredObservations",
    "passedObservations",
    "failedObservations",
    "notMeasuredObservations",
    "notApplicableObservations",
    "coverageRate",
    "passRate",
}
RAW_AGGREGATE_KEYS = {
    "cases",
    "callSuccessRate",
    "casePassRate",
    "criticalFailureCases",
    "diagnosticScoreMean",
    "toolPolicyMean",
    "meanDurationMs",
    "retrievalQuality",
    "toolAgentExecution",
    "finalProductOutputContract",
}
EGRESS_LEDGER_KEYS = {
    "required",
    "sqlitePersistenceVerified",
    "temporaryDatabaseCleaned",
    "attemptedRows",
    "persistedRows",
    "persistenceFailures",
    "contentFreeRowsSha256",
    "providerIds",
    "callKinds",
    "rows",
}
EGRESS_ROW_KEYS = {
    "ordinal",
    "providerId",
    "destination",
    "modelRequested",
    "callKind",
    "modelServed",
    "promptTokens",
    "completionTokens",
    "totalTokens",
    "cachedTokens",
    "redactionsEmail",
    "redactionsCard",
    "redactionsPhone",
    "redactionsName",
    "systemBytes",
    "userBytes",
}
REPORT_KEYS = {
    "schemaVersion",
    "runLabel",
    "generatedAt",
    "repositoryCommit",
    "sourceFingerprintSha256",
    "manifestSha256",
    "promptVersion",
    "syntheticOnly",
    "holdoutInterpretation",
    "benchmarkDesign",
    "evidenceScope",
    "evidenceLimits",
    "retrievalLane",
    "retrievalQuality",
    "snapshotStart",
    "snapshotEnd",
    "environment",
    "egressLedger",
    "sameCallerEnvelopeModelStack",
    "arms",
    "localComposite",
    "pairedComparison",
}
RETRIEVAL_QUALITY_KEYS = {
    "required",
    "surface",
    "attribution",
    "fixtureSha256",
    "corpusSourceSha256",
    "embedderId",
    "realEmbedder",
    "modelFiles",
    "anchorDate",
    "k",
    "candidateLimit",
    "cosineFloor",
    "cases",
    "aggregates",
    "visibilityGate",
    "temporaryDatabaseCleaned",
}
RETRIEVAL_CASE_KEYS = {
    "caseId",
    "language",
    "queryPayloadSha256",
    "expectedMeetings",
    "expectedIdHashes",
    "rankings",
    "metrics",
}
RETRIEVAL_METRIC_KEYS = {
    "recallAtK",
    "ndcgAtK",
    "reciprocalRank",
}
RETRIEVAL_AGGREGATE_KEYS = {
    "recallAtK",
    "ndcgAtK",
    "mrr",
    "queries",
}
MODEL_FILE_KEYS = {"filename", "bytes", "sha256"}
RAW_PAIRED_CASE_KEYS = {
    "caseId",
    "casePayloadSha256",
    "surface",
    "comparisonKind",
    "localArm",
    "referenceArm",
    "holdout",
    "comparisonScope",
    "localRouteInputSha256",
    "referenceRouteInputSha256",
    "localGenerationProfile",
    "referenceGenerationProfile",
    "localCasePass",
    "referenceCasePass",
    "localCallSuccess",
    "referenceCallSuccess",
    "localCriticalFailure",
    "referenceCriticalFailure",
    "localDiagnosticScore",
    "referenceDiagnosticScore",
    "referenceMinusLocal",
}
RAW_PAIRED_AGGREGATE_KEYS = {
    "localArm",
    "referenceArm",
    "comparisonKind",
    "cohort",
    "matchedCases",
    "localCasePassRate",
    "referenceCasePassRate",
    "localCallSuccessRate",
    "referenceCallSuccessRate",
    "localSurfaceMacroPassRate",
    "referenceSurfaceMacroPassRate",
    "localCriticalFailureCases",
    "referenceCriticalFailureCases",
    "referenceMinusLocalMean",
}
MODEL_ONLY_REPORT_KEYS = {
    "laneId",
    "entrypoint",
    "equalityBoundary",
    "providerRenderedPromptsByteIdentical",
    "effectiveModelInputsAttestedIdentical",
    "limitations",
    "arms",
    "pairs",
    "aggregates",
}
MODEL_ONLY_ARM_KEYS = {"armId", "modelRequested", "aggregates", "cases"}
MODEL_ONLY_CASE_KEYS = {
    "caseId",
    "casePayloadSha256",
    "surface",
    "language",
    "modelClass",
    "holdout",
    "armId",
    "modelRequested",
    "systemSha256",
    "userSha256",
    "envelopeSha256",
    "systemBytes",
    "userBytes",
    "systemChars",
    "userChars",
    "projection",
    "outputContract",
    "opaqueSubstitutionCount",
    "opaqueSubstitutionsSha256",
    "callCount",
    "rawOutputChars",
    "rawOutputSha256",
    "outputChars",
    "outputSha256",
    "output",
    "provenance",
    "provenanceSha256",
    "stateApplicationPass",
    "durationMs",
    "error",
    "egressReceiptStartOrdinal",
    "egressReceiptEndOrdinal",
    "egressReceiptCount",
    "egressReceiptSha256",
    "redactionsEmail",
    "redactionsCard",
    "redactionsPhone",
    "redactionsName",
    "score",
    "caseRecordSha256",
}
COMPOSITE_AGGREGATE_KEYS = {
    "cases",
    "callSuccessRate",
    "casePassRate",
    "surfaceMacroPassRate",
    "criticalFailureCases",
    "diagnosticScoreMean",
}
MODEL_ONLY_PAIR_KEYS = {
    "caseId",
    "casePayloadSha256",
    "surface",
    "holdout",
    "localArm",
    "referenceArm",
    "envelopeSha256",
    "localCasePass",
    "referenceCasePass",
    "localCallSuccess",
    "referenceCallSuccess",
    "localCriticalFailure",
    "referenceCriticalFailure",
    "localDiagnosticScore",
    "referenceDiagnosticScore",
    "referenceMinusLocal",
}
MODEL_ONLY_PAIRED_AGGREGATE_KEYS = {
    "localArm",
    "referenceArm",
    "cohort",
    "matchedCases",
    "localCasePassRate",
    "referenceCasePassRate",
    "localCallSuccessRate",
    "referenceCallSuccessRate",
    "localSurfaceMacroPassRate",
    "referenceSurfaceMacroPassRate",
    "localCriticalFailureCases",
    "referenceCriticalFailureCases",
    "referenceMinusLocalMean",
}
MODEL_ONLY_EQUALITY_FIELDS = (
    "casePayloadSha256",
    "systemSha256",
    "userSha256",
    "envelopeSha256",
    "systemBytes",
    "userBytes",
    "systemChars",
    "userChars",
    "projection",
    "outputContract",
    "opaqueSubstitutionCount",
    "opaqueSubstitutionsSha256",
    "callCount",
)
ENVIRONMENT_KEYS = {
    "hardwareModel",
    "cpuBrand",
    "memoryBytes",
    "osVersion",
    "osBuild",
    "nameRedactorMode",
    "trackedDiffSha256",
    "workingTreeDirty",
    "armOrder",
    "repetition",
}
SNAPSHOT_KEYS = {
    "repositoryCommit",
    "sourceFingerprintSha256",
    "manifestSha256",
    "evaluatorFileSha256",
    "fixtureFileSha256",
    "repeatValidatorFileSha256",
    "trackedDiffSha256",
    "workingTreeDirty",
}
EVIDENCE_MANIFEST_KEYS = {
    "schemaVersion",
    "kind",
    "evidenceMethod",
    "repetitions",
    "combined",
    "producerSnapshot",
    "runtimeIdentities",
}

MAX_LOGICAL_JSON_BYTES = 32 * 1024 * 1024
MAX_COMPRESSED_ARCHIVE_BYTES = 16 * 1024 * 1024
PRIVACY_PATTERNS = (
    ("email", re.compile(r"(?<![\w.+-])[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}(?![\w.-])", re.I)),
    ("phone", re.compile(r"(?<!\w)\+(?:[\s().-]*\d){7,15}(?!\w)")),
    ("macos_user_path", re.compile(r"(?<![\w/])/Users/[^/\s\"']+(?:/[^\s\"']*)?")),
    ("unix_user_path", re.compile(r"(?<![\w/])/home/[^/\s\"']+(?:/[^\s\"']*)?")),
    ("windows_user_path", re.compile(r"(?i)(?<![A-Z0-9_])[A-Z]:\\Users\\[^\\\s\"']+")),
    ("external_url", re.compile(r"(?i)https?://[^\s\"'<>]+")),
    ("file_url", re.compile(r"(?i)file://[^\s\"'<>]+")),
    ("pem_private_key", re.compile(r"-----BEGIN (?:[A-Z0-9 ]+ )?PRIVATE KEY-----")),
    ("bearer_token", re.compile(r"(?i)\bbearer\s+[A-Z0-9._~+/=-]{12,}")),
    ("jwt", re.compile(r"(?<![A-Za-z0-9_-])eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}(?![A-Za-z0-9_-])")),
    ("api_token", re.compile(r"(?<![A-Za-z0-9])(?:sk-[A-Za-z0-9_-]{16,}|ghp_[A-Za-z0-9]{16,}|xox[baprs]-[A-Za-z0-9-]{10,}|AKIA[0-9A-Z]{16})(?![A-Za-z0-9])")),
)


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ValueError(message)


def privacy_violation(value: Any, path: str = "$") -> tuple[str, str] | None:
    """Return only a rule id + JSON pointer; never echo a potentially sensitive value."""
    if isinstance(value, dict):
        for key, nested in value.items():
            violation = privacy_violation(nested, f"{path}/{key}")
            if violation is not None:
                return violation
    elif isinstance(value, list):
        for index, nested in enumerate(value):
            violation = privacy_violation(nested, f"{path}/{index}")
            if violation is not None:
                return violation
    elif isinstance(value, str):
        for rule, pattern in PRIVACY_PATTERNS:
            if pattern.search(value):
                return rule, path
    return None


def validate_artifact_privacy(value: Any, context: str) -> None:
    violation = privacy_violation(value)
    require(
        violation is None,
        f"{context}: privacy rule {violation[0]} failed at {violation[1]}"
        if violation is not None
        else f"{context}: privacy scan failed",
    )


def load(path: Path) -> dict[str, Any]:
    require(
        path.stat().st_size <= MAX_LOGICAL_JSON_BYTES,
        f"{path}: JSON exceeds the validation size cap",
    )
    with path.open(encoding="utf-8") as handle:
        value = json.load(handle)
    require(isinstance(value, dict), f"{path}: root must be an object")
    validate_artifact_privacy(value, str(path))
    return value


def load_bytes(value: bytes, context: str) -> dict[str, Any]:
    require(
        len(value) <= MAX_LOGICAL_JSON_BYTES,
        f"{context}: decompressed JSON exceeds the validation size cap",
    )
    parsed = json.loads(value.decode("utf-8"))
    require(isinstance(parsed, dict), f"{context}: root must be an object")
    validate_artifact_privacy(parsed, context)
    return parsed


def read_gzip_capped(path: Path) -> bytes:
    require(
        path.stat().st_size <= MAX_COMPRESSED_ARCHIVE_BYTES,
        f"{path}: compressed evidence exceeds the archive size cap",
    )
    with gzip.open(path, "rb") as handle:
        value = handle.read(MAX_LOGICAL_JSON_BYTES + 1)
    require(
        len(value) <= MAX_LOGICAL_JSON_BYTES,
        f"{path}: decompressed evidence exceeds the JSON size cap",
    )
    return value


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for block in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def sha256_bytes(value: bytes) -> str:
    return hashlib.sha256(value).hexdigest()


def resolve_evidence_path(relative: Any, context: str) -> Path:
    require(isinstance(relative, str) and relative, f"{context}: path is required")
    path = (REPO_ROOT / relative).resolve()
    root = REPO_ROOT.resolve()
    require(
        path == root or root in path.parents,
        f"{context}: path escapes the repository",
    )
    return path


def current_measurement_hashes() -> dict[str, str]:
    return {key: sha256_file(path) for key, path in MEASUREMENT_FILES.items()}


def current_source_fingerprint() -> str:
    digest = hashlib.sha256()
    source_root = REPO_ROOT / "src-tauri"
    for relative in SOURCE_FILES:
        relative_bytes = relative.encode("utf-8")
        digest.update(len(relative_bytes).to_bytes(8, "little"))
        digest.update(relative_bytes)
        path = source_root / relative
        require(path.is_file(), f"source fingerprint dependency is missing: {relative}")
        content = path.read_bytes()
        digest.update(len(content).to_bytes(8, "little"))
        digest.update(content)
    return digest.hexdigest()


def rust_source_fingerprint_files() -> list[str]:
    evaluator = MEASUREMENT_FILES["evaluatorFileSha256"].read_text(encoding="utf-8")
    declaration = re.search(
        r"const SOURCE_FINGERPRINT_FILES: &\[&str\] = &\[(.*?)\n\];",
        evaluator,
        flags=re.DOTALL,
    )
    require(declaration is not None, "Rust source-fingerprint dependency list is missing")
    return re.findall(r'"([^"]+)"', declaration.group(1))


def git_output(*args: str) -> bytes:
    return subprocess.run(
        ["git", *args],
        cwd=REPO_ROOT,
        check=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    ).stdout


def current_git_identity() -> tuple[str, str]:
    commit = git_output("rev-parse", "HEAD").decode("utf-8").strip()
    tracked_diff = hashlib.sha256(git_output("diff", "--binary", "HEAD")).hexdigest()
    return commit, tracked_diff


def prompt_hash(parts: list[str]) -> str:
    digest = hashlib.sha256()
    for part in parts:
        encoded = part.encode("utf-8")
        digest.update(len(encoded).to_bytes(8, "little"))
        digest.update(encoded)
    return digest.hexdigest()


def canonical_json_hash(value: Any) -> str:
    canonical = json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    return prompt_hash([canonical])


def valid_sha256(value: Any) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 64
        and all(character in "0123456789abcdef" for character in value)
    )


def case_payload_values(case: dict[str, Any]) -> list[Any]:
    """Candidate-independent fixture tuple; must mirror Rust case_payload_sha256."""
    return [
        "murmur-quality-case-payload-v2",
        case["id"],
        case["surface"],
        case["language"],
        case.get("transcript", ""),
        case.get("question", ""),
        case.get("dateIso", ""),
        case.get("titleHint", ""),
        case.get("vaultTitles", []),
        bool(case.get("labeled", False)),
        bool(case.get("diarizedOthers", False)),
        int(case.get("durationS", 0)),
        case.get("action", ""),
        case.get("selection", ""),
        case.get("before", ""),
        case.get("previousBullets", ""),
        case.get("toolResult", ""),
        case.get("searchResult", ""),
        case.get("searchTerms", []),
        case.get("floorCorpus", ""),
        case.get("syntheticRedactionEntities", []),
    ]


def case_payload_sha256(case: dict[str, Any]) -> str:
    return canonical_json_hash(case_payload_values(case))


def validate_fixture(fixture: dict[str, Any], context: str) -> dict[str, dict[str, Any]]:
    require(fixture.get("schemaVersion") == 9, f"{context}: expected schemaVersion 9")
    require(fixture.get("syntheticOnly") is True, f"{context}: fixture must be synthetic")
    require(
        set(fixture) == {"schemaVersion", "syntheticOnly", "cases"}
        and isinstance(fixture["cases"], list),
        f"{context}: fixture root schema differs",
    )
    fixture_cases = {case["id"]: case for case in fixture["cases"]}
    require(
        len(fixture_cases) == len(fixture["cases"]),
        f"{context}: duplicate case IDs",
    )
    heavy = {
        case["id"] for case in fixture["cases"] if case.get("modelClass") == "heavy"
    }
    light = {
        case["id"] for case in fixture["cases"] if case.get("modelClass") == "light"
    }
    require(heavy == HEAVY_CASE_IDS, f"{context}: heavy case set differs")
    require(light == LIGHT_CASE_IDS, f"{context}: light case set differs")
    require(
        set(fixture_cases) == HEAVY_CASE_IDS | LIGHT_CASE_IDS,
        f"{context}: exact case set differs",
    )
    for case_id, case in fixture_cases.items():
        require(
            isinstance(case.get("expected"), dict),
            f"{context}/{case_id}: expected contract is required",
        )
        redaction_entities = case.get("syntheticRedactionEntities")
        require(
            isinstance(redaction_entities, list)
            and len(redaction_entities) == len(set(redaction_entities))
            and all(isinstance(value, str) and value.strip() for value in redaction_entities),
            f"{context}/{case_id}: candidate-input redaction inventory differs",
        )
        candidate_inputs = "\n".join(
            [
                str(case.get(key, ""))
                for key in (
                    "transcript",
                    "question",
                    "dateIso",
                    "titleHint",
                    "action",
                    "selection",
                    "before",
                    "previousBullets",
                    "toolResult",
                    "searchResult",
                    "floorCorpus",
                )
            ]
            + [str(value) for value in case.get("vaultTitles", [])]
            + [str(value) for value in case.get("searchTerms", [])]
        )
        require(
            all(value in candidate_inputs for value in redaction_entities),
            f"{context}/{case_id}: redaction inventory must derive from non-oracle inputs",
        )
        if case["surface"] == "light_extraction":
            expected = case["expected"]
            facts = expected.get("structuredFacts")
            require(
                expected.get("format") == "structured_facts"
                and isinstance(facts, list)
                and len(facts) == 3
                and all(
                    isinstance(fact, dict)
                    and set(fact) == {"entity", "predicate", "object"}
                    and all(isinstance(fact[key], str) and fact[key] for key in fact)
                    for fact in facts
                ),
                f"{context}/{case_id}: exact structured-fact labels are required",
            )
    return fixture_cases


def retrieval_metric_from_hashes(
    ranked: list[str], expected: list[str], k: int
) -> dict[str, float]:
    if not expected:
        return {"recallAtK": 1.0, "ndcgAtK": 1.0, "reciprocalRank": 1.0}
    top = set(ranked[:k])
    recall = sum(value in top for value in expected) / len(expected) if k else 0.0
    gold = set(expected)
    counted: set[str] = set()
    dcg = 0.0
    for rank, value in enumerate(ranked[:k], start=1):
        if value in gold and value not in counted:
            counted.add(value)
            dcg += 1.0 / math.log2(rank + 1.0)
    ideal = sum(
        1.0 / math.log2(rank + 1.0)
        for rank in range(1, min(k, len(expected)) + 1)
    )
    ndcg = dcg / ideal if ideal else 0.0
    reciprocal = next(
        (1.0 / rank for rank, value in enumerate(ranked, start=1) if value in gold),
        0.0,
    )
    return {
        "recallAtK": recall,
        "ndcgAtK": ndcg,
        "reciprocalRank": reciprocal,
    }


def close_metric(left: Any, right: float) -> bool:
    return (
        isinstance(left, (int, float))
        and not isinstance(left, bool)
        and math.isfinite(float(left))
        and abs(float(left) - right) <= 1e-12
    )


def validate_retrieval_quality(report: dict[str, Any], context: str) -> dict[str, Any]:
    evidence = report.get("retrievalQuality")
    require(
        isinstance(evidence, dict) and set(evidence) == RETRIEVAL_QUALITY_KEYS,
        f"{context}: retrieval-quality evidence schema differs",
    )
    fixture_path = REPO_ROOT / "src-tauri/src/eval/fixtures/rag-bakeoff-synthetic.json"
    fixture_text = fixture_path.read_text(encoding="utf-8")
    fixture = json.loads(fixture_text)
    validate_artifact_privacy(fixture, str(fixture_path))
    require(
        isinstance(fixture, list) and fixture,
        f"{context}: retrieval fixture must be a non-empty query array",
    )
    require(
        evidence["required"] is True
        and evidence["surface"] == "ask_vault_retrieval"
        and evidence["attribution"]
        == "independent_synthetic_retrieval_lane_not_generation_quality"
        and evidence["fixtureSha256"] == prompt_hash([fixture_text])
        and evidence["corpusSourceSha256"]
        == sha256_file(REPO_ROOT / "src-tauri/src/eval/corpus.rs")
        and isinstance(evidence["embedderId"], str)
        and bool(evidence["embedderId"])
        and evidence["realEmbedder"] is True
        and evidence["anchorDate"] == "2026-06-29"
        and evidence["k"] == 5
        and evidence["candidateLimit"] == 20
        and close_metric(evidence["cosineFloor"], 0.78)
        and evidence["temporaryDatabaseCleaned"] is True
        and evidence["visibilityGate"]
        == "Db::search_visible_in_range + Db::search_semantic_visible + "
        "Db::search_hybrid_visible with empty session-unlock set",
        f"{context}: real, gated retrieval measurement provenance differs",
    )
    model_files = evidence["modelFiles"]
    require(
        isinstance(model_files, list)
        and bool(model_files)
        and len({model["filename"] for model in model_files}) == len(model_files),
        f"{context}: retrieval model-file provenance is incomplete",
    )
    for index, model in enumerate(model_files):
        require(
            isinstance(model, dict)
            and set(model) == MODEL_FILE_KEYS
            and isinstance(model["filename"], str)
            and bool(model["filename"])
            and type(model["bytes"]) is int
            and model["bytes"] > 0
            and valid_sha256(model["sha256"]),
            f"{context}: retrieval model file {index} identity differs",
        )
    cases = evidence["cases"]
    require(
        isinstance(cases, list) and len(cases) == len(fixture),
        f"{context}: retrieval case count differs from the fixture",
    )
    modes = {"fts_product", "semantic_product_floor", "hybrid_product"}
    per_case_metrics: list[dict[str, dict[str, float]]] = []
    for index, (case, query) in enumerate(zip(cases, fixture), start=1):
        expected_ids = query["expected_meeting_ids"]
        expected_hashes = [
            prompt_hash(["murmur-retrieval-meeting-id-v1", value])
            for value in expected_ids
        ]
        query_hash_parts = [
            "murmur-retrieval-case-payload-v2",
            query.get("lang", ""),
            query["query"],
            *expected_ids,
        ]
        require(
            isinstance(case, dict)
            and set(case) == RETRIEVAL_CASE_KEYS
            and case["caseId"] == f"retrieval-{index:02}"
            and case["language"] == query.get("lang", "")
            and case["expectedMeetings"] == len(expected_ids)
            and case["expectedIdHashes"] == expected_hashes
            and case["queryPayloadSha256"] == prompt_hash(query_hash_parts)
            and isinstance(case["rankings"], dict)
            and set(case["rankings"]) == modes
            and isinstance(case["metrics"], dict)
            and set(case["metrics"]) == modes,
            f"{context}: retrieval case {index} payload/ranking schema differs",
        )
        replayed: dict[str, dict[str, float]] = {}
        for mode in modes:
            ranking = case["rankings"][mode]
            metric = case["metrics"][mode]
            require(
                isinstance(ranking, list)
                and len(ranking) <= evidence["candidateLimit"]
                and len(ranking) == len(set(ranking))
                and all(valid_sha256(value) for value in ranking)
                and isinstance(metric, dict)
                and set(metric) == RETRIEVAL_METRIC_KEYS,
                f"{context}/retrieval-{index:02}/{mode}: ranking/metric schema differs",
            )
            expected_metric = retrieval_metric_from_hashes(
                ranking, expected_hashes, evidence["k"]
            )
            require(
                all(close_metric(metric[key], expected_metric[key]) for key in expected_metric),
                f"{context}/retrieval-{index:02}/{mode}: metrics do not replay from hashed ranking",
            )
            replayed[mode] = expected_metric
        per_case_metrics.append(replayed)
    expected_subsets = {
        "all": list(range(len(fixture))),
        "language:pl": [i for i, query in enumerate(fixture) if query.get("lang") == "pl"],
        "language:en": [i for i, query in enumerate(fixture) if query.get("lang") == "en"],
    }
    aggregates = evidence["aggregates"]
    require(
        isinstance(aggregates, dict) and set(aggregates) == set(expected_subsets),
        f"{context}: retrieval aggregate cohorts differ",
    )
    for cohort, indices in expected_subsets.items():
        stored_modes = aggregates[cohort]
        require(
            isinstance(stored_modes, dict) and set(stored_modes) == modes,
            f"{context}/{cohort}: retrieval aggregate mode set differs",
        )
        for mode in modes:
            stored = stored_modes[mode]
            require(
                isinstance(stored, dict)
                and set(stored) == RETRIEVAL_AGGREGATE_KEYS
                and stored["queries"] == len(indices),
                f"{context}/{cohort}/{mode}: retrieval aggregate schema differs",
            )
            expected_values = {
                "recallAtK": sum(
                    per_case_metrics[index][mode]["recallAtK"] for index in indices
                )
                / len(indices),
                "ndcgAtK": sum(
                    per_case_metrics[index][mode]["ndcgAtK"] for index in indices
                )
                / len(indices),
                "mrr": sum(
                    per_case_metrics[index][mode]["reciprocalRank"] for index in indices
                )
                / len(indices),
            }
            require(
                all(close_metric(stored[key], value) for key, value in expected_values.items()),
                f"{context}/{cohort}/{mode}: aggregate does not replay from case rankings",
            )
    return evidence


def dimension_aggregate(verdicts: list[str]) -> dict[str, Any]:
    measured = sum(verdict in {"pass", "fail"} for verdict in verdicts)
    passed = sum(verdict == "pass" for verdict in verdicts)
    not_measured = sum(verdict == "not_measured" for verdict in verdicts)
    not_applicable = sum(verdict == "not_applicable" for verdict in verdicts)
    applicable = measured + not_measured
    return {
        "observations": len(verdicts),
        "applicableObservations": applicable,
        "measuredObservations": measured,
        "passedObservations": passed,
        "failedObservations": measured - passed,
        "notMeasuredObservations": not_measured,
        "notApplicableObservations": not_applicable,
        "coverageRate": percent(measured, applicable) if applicable else None,
        "passRate": percent(passed, measured) if measured else None,
    }


def expected_dimensions(case: dict[str, Any], arm_id: str) -> dict[str, str]:
    score = case["score"]
    retrieval = (
        "not_measured" if case["surface"] == "ask_vault" else "not_applicable"
    )
    is_agent_loop = arm_id == SOL and case["surface"] in {"ask_vault", "live_current"}
    if is_agent_loop:
        require(
            isinstance(case["branchConverged"], bool)
            and isinstance(case["toolPolicyPass"], bool),
            f"{arm_id}/{case['caseId']}: cloud Ask/Live loop receipts are required",
        )
        tool_agent = (
            "pass"
            if case["branchConverged"] and case["toolPolicyPass"]
            else "fail"
        )
    else:
        require(
            case["branchConverged"] is None,
            f"{arm_id}/{case['caseId']}: tool-agent dimension applies only to cloud Ask/Live loops",
        )
        tool_agent = "not_applicable"
    final_pass = (
        case["error"] is None
        and score["requiredGroupsHit"] == score["requiredGroupsTotal"]
        and all(
            bool(score[key])
            for key in (
                "formatPass",
                "sectionPass",
                "languagePass",
                "forbiddenPass",
                "constraintPass",
                "provenancePass",
                "relationPass",
                "stateApplicationPass",
                "closedWorldPass",
                "structuredLabelsPass",
            )
        )
    )
    return {
        "retrievalQuality": retrieval,
        "toolAgentExecution": tool_agent,
        "finalProductOutputContract": "pass" if final_pass else "fail",
    }


def validate_arm_metadata(arm_id: str, metadata: dict[str, Any], context: str) -> None:
    require(set(metadata) == ARM_METADATA_KEYS, f"{context}: arm metadata schema differs")
    require(metadata["armId"] == arm_id, f"{context}: armId differs")
    require(
        isinstance(metadata["runtimeVersion"], str) and metadata["runtimeVersion"],
        f"{context}: runtimeVersion is required",
    )
    require(
        valid_sha256(metadata["runtimeSha256"]),
        f"{context}: runtimeSha256 is required",
    )
    if arm_id == SOL:
        require(
            metadata["modelRequested"] == "gpt-5.6-sol"
            and metadata["effort"] == "high"
            and metadata["effortTransport"]
            == '--config model_reasoning_effort="high"'
            and metadata["effortEffectiveAttested"] is False
            and metadata["modelClass"] == "reference",
            f"{context}: Sol model/effort transport/class identity differs",
        )
        require(
            all(
                metadata[key] is None
                for key in (
                    "modelFilename",
                    "modelBytes",
                    "modelSha256",
                    "sidecarIdleSecs",
                    "sidecarReadySecs",
                    "sidecarHardCapSecs",
                )
            ),
            f"{context}: Sol must not claim local model/sidecar metadata",
        )
        return
    expected = {
        QWEN4: (QWEN4_FILENAME, QWEN4_BYTES, QWEN4_SHA256, "heavy"),
        QWEN1: (QWEN1_FILENAME, QWEN1_BYTES, QWEN1_SHA256, "light"),
    }[arm_id]
    filename, size, digest, model_class = expected
    require(
        metadata["modelRequested"] == filename
        and metadata["modelFilename"] == filename
        and metadata["modelBytes"] == size
        and metadata["modelSha256"] == digest
        and metadata["modelClass"] == model_class
        and metadata["effort"] is None,
        f"{context}: local model identity differs",
    )
    require(
        metadata["runtimeVersion"] == LOCAL_RUNTIME_VERSION,
        f"{context}: local runtime version differs from the producer contract",
    )
    require(
        metadata["effortTransport"] is None
        and metadata["effortEffectiveAttested"] is None,
        f"{context}: local arm must not claim cloud effort transport/attestation",
    )
    require(
        (
            metadata["sidecarIdleSecs"],
            metadata["sidecarReadySecs"],
            metadata["sidecarHardCapSecs"],
        )
        == (300, 90, 180),
        f"{context}: production sidecar timeouts differ",
    )


def case_record_values(case: dict[str, Any]) -> list[Any]:
    score = case["score"]
    return [
        case["caseId"],
        case["casePayloadSha256"],
        case["surface"],
        case["language"],
        case["modelClass"],
        case["holdout"],
        case["routeInputSha256"],
        case["generationProfile"],
        case["productRoute"],
        case["comparisonScope"],
        case["routeInputChars"],
        case["outputChars"],
        case["outputSha256"],
        case["durationMs"],
        case["output"],
        case["surfaceOutput"],
        case["surfaceOutputSha256"],
        case["rawModelOutput"],
        case["rawModelOutputSha256"],
        case["rawModelFormatPass"],
        case["structuredSchemaPass"],
        case["structuredLabelsPass"],
        case["structuredEnvelopePass"],
        case["error"],
        case["toolSteps"],
        case["toolPolicyScore"],
        case["toolPolicyPass"],
        case["stateApplicationPass"],
        case["branchConverged"],
        case["provenance"],
        case["provenanceSha256"],
        case["toolStepsSha256"],
        case["egressReceiptStartOrdinal"],
        case["egressReceiptEndOrdinal"],
        case["egressReceiptCount"],
        case["egressReceiptSha256"],
        [
            case["dimensions"]["retrievalQuality"],
            case["dimensions"]["toolAgentExecution"],
            case["dimensions"]["finalProductOutputContract"],
        ],
        [
            score["diagnosticScore"],
            score["casePass"],
            score["criticalFailure"],
            score["requiredGroupsHit"],
            score["requiredGroupsTotal"],
            score["formatPass"],
            score["sectionPass"],
            score["languagePass"],
            score["forbiddenPass"],
            score["constraintPass"],
            score["provenancePass"],
            score["toolPolicyPass"],
            score["relationPass"],
            score["stateApplicationPass"],
            score["branchConvergencePass"],
            score["closedWorldPass"],
            score["structuredLabelsPass"],
            score["criticalErrors"],
        ],
    ]


def validate_case_content(
    case: dict[str, Any],
    fixture_case: dict[str, Any],
    arm_id: str,
    context: str,
) -> None:
    require(set(case) == CASE_RESULT_KEYS, f"{context}: case result schema differs")
    require(set(case["score"]) == SCORE_KEYS, f"{context}: score schema differs")
    require(
        set(case["dimensions"]) == DIMENSION_KEYS
        and all(
            verdict in DIMENSION_VERDICTS for verdict in case["dimensions"].values()
        ),
        f"{context}: dimension schema/verdict differs",
    )
    require(
        case["casePayloadSha256"] == case_payload_sha256(fixture_case),
        f"{context}: candidate-independent fixture payload commitment differs",
    )
    require(
        valid_sha256(case["routeInputSha256"])
        and isinstance(case["generationProfile"], str)
        and bool(case["generationProfile"])
        and isinstance(case["productRoute"], str)
        and bool(case["productRoute"])
        and isinstance(case["routeInputChars"], int)
        and case["routeInputChars"] > 0,
        f"{context}: route/profile provenance is incomplete",
    )
    output = case["output"]
    require(isinstance(output, str), f"{context}: output must be text")
    require(
        case["outputChars"] == len(output),
        f"{context}: outputChars differs from output",
    )
    require(
        case["outputSha256"] == prompt_hash([output]),
        f"{context}: outputSha256 differs from output",
    )
    for value_key, hash_key in (
        ("rawModelOutput", "rawModelOutputSha256"),
        ("surfaceOutput", "surfaceOutputSha256"),
    ):
        value = case[value_key]
        require(
            (value is None and case[hash_key] is None)
            or (isinstance(value, str) and case[hash_key] == prompt_hash([value])),
            f"{context}: {hash_key} differs from {value_key}",
        )
    for value_key, hash_key in (
        ("provenance", "provenanceSha256"),
        ("toolSteps", "toolStepsSha256"),
    ):
        values = case[value_key]
        require(
            isinstance(values, list) and all(isinstance(value, str) for value in values),
            f"{context}: {value_key} must be an array of strings",
        )
        require(
            case[hash_key] == prompt_hash(values),
            f"{context}: {hash_key} differs from {value_key}",
        )
    score = case["score"]
    require(
        all(
            isinstance(score[key], bool)
            for key in SCORE_KEYS
            - {
                "diagnosticScore",
                "requiredGroupsHit",
                "requiredGroupsTotal",
                "criticalErrors",
            }
        )
        and isinstance(score["criticalErrors"], list)
        and all(isinstance(error, str) for error in score["criticalErrors"]),
        f"{context}: score value types differ",
    )
    is_extraction = case["surface"] == "light_extraction"
    if is_extraction:
        require(
            all(
                isinstance(case[key], bool)
                for key in (
                    "structuredSchemaPass",
                    "structuredLabelsPass",
                    "structuredEnvelopePass",
                    "rawModelFormatPass",
                )
            )
            and case["structuredSchemaPass"] == score["formatPass"]
            and case["structuredLabelsPass"] == score["structuredLabelsPass"]
            and case["structuredEnvelopePass"] == case["rawModelFormatPass"],
            f"{context}: structured schema/labels/raw-envelope evidence differs",
        )
    else:
        require(
            all(
                case[key] is None
                for key in (
                    "structuredSchemaPass",
                    "structuredLabelsPass",
                    "structuredEnvelopePass",
                )
            ),
            f"{context}: structured evidence applies only to light extraction",
        )
    if case["productRoute"] in {
        "ask_vault_cloud_agentic",
        "ask_vault_cloud_agentic_then_floor_fallback",
    }:
        tool_steps = case["toolSteps"]
        get_indices = [
            index for index, step in enumerate(tool_steps) if step == "get_meeting"
        ]
        every_get_is_staged = all(
            any(
                prior in {"search_meetings", "search_semantic"}
                for prior in tool_steps[:index]
            )
            for index in get_indices
        )
        staged_get = bool(get_indices) and every_get_is_staged
        require(
            every_get_is_staged,
            f"{context}: every get_meeting requires a prior successful search",
        )
        if case["productRoute"] == "ask_vault_cloud_agentic":
            require(
                not case["provenance"] or staged_get,
                f"{context}: direct Ask provenance lacks staged tool execution",
            )
            require(
                case["toolPolicyPass"] is not True or staged_get,
                f"{context}: positive tool-policy receipt lacks staged read",
            )
    require(
        case["dimensions"] == expected_dimensions(case, arm_id),
        f"{context}: dimensions differ from committed case evidence",
    )
    require(
        case["caseRecordSha256"] == canonical_json_hash(case_record_values(case)),
        f"{context}: full case record commitment differs",
    )


def validate_egress_ledger(report: dict[str, Any], context: str) -> list[dict[str, Any]]:
    ledger = report.get("egressLedger")
    require(
        isinstance(ledger, dict) and set(ledger) == EGRESS_LEDGER_KEYS,
        f"{context}: egress ledger schema differs",
    )
    rows = ledger["rows"]
    require(
        ledger["required"] is True
        and ledger["sqlitePersistenceVerified"] is True
        and ledger["temporaryDatabaseCleaned"] is True
        and ledger["persistenceFailures"] == 0
        and isinstance(rows, list)
        and len(rows) > 0
        and ledger["attemptedRows"] == ledger["persistedRows"] == len(rows),
        f"{context}: durable SQLite egress proof is incomplete",
    )
    allowed_call_kinds = {
        "summarize",
        "summarize_error",
        "complete",
        "complete_error",
        "complete_json",
        "complete_json_error",
    }
    optional_count_keys = {
        "promptTokens",
        "completionTokens",
        "totalTokens",
        "cachedTokens",
    }
    required_count_keys = {
        "redactionsEmail",
        "redactionsCard",
        "redactionsPhone",
        "redactionsName",
        "systemBytes",
        "userBytes",
    }
    for index, row in enumerate(rows, start=1):
        require(
            isinstance(row, dict) and set(row) == EGRESS_ROW_KEYS,
            f"{context}: egress row {index} schema differs",
        )
        require(
            row["ordinal"] == index
            and row["providerId"] == "codex_cli"
            and row["modelRequested"] == "gpt-5.6-sol"
            and isinstance(row["destination"], str)
            and bool(row["destination"])
            and row["callKind"] in allowed_call_kinds,
            f"{context}: egress row {index} identity/order differs",
        )
        require(
            all(
                row[key] is None or (type(row[key]) is int and row[key] >= 0)
                for key in optional_count_keys
            )
            and all(
                type(row[key]) is int and row[key] >= 0 for key in required_count_keys
            )
            and (
                row["modelServed"] is None
                or isinstance(row["modelServed"], str)
            ),
            f"{context}: egress row {index} content-free counters differ",
        )
    require(
        ledger["contentFreeRowsSha256"] == canonical_json_hash(rows),
        f"{context}: content-free egress rows commitment differs",
    )
    require(
        ledger["providerIds"] == sorted({row["providerId"] for row in rows})
        == ["codex_cli"]
        and ledger["callKinds"] == sorted({row["callKind"] for row in rows}),
        f"{context}: egress provider/call-kind index differs",
    )
    return rows


def validate_case_receipt(
    case: dict[str, Any], arm_id: str, rows: list[dict[str, Any]], context: str
) -> set[int]:
    count = case["egressReceiptCount"]
    start = case["egressReceiptStartOrdinal"]
    end = case["egressReceiptEndOrdinal"]
    require(type(count) is int and count >= 0, f"{context}: invalid receipt count")
    if arm_id != SOL:
        require(
            count == 0
            and start is None
            and end is None
            and case["egressReceiptSha256"] == canonical_json_hash([]),
            f"{context}: local arm must have an empty egress receipt",
        )
        return set()
    require(
        count > 0
        and type(start) is int
        and type(end) is int
        and start > 0
        and end >= start
        and end - start + 1 == count,
        f"{context}: every Sol case needs a contiguous non-empty receipt",
    )
    selected = [row for row in rows if start <= row["ordinal"] <= end]
    require(
        len(selected) == count
        and [row["ordinal"] for row in selected] == list(range(start, end + 1))
        and case["egressReceiptSha256"] == canonical_json_hash(selected),
        f"{context}: per-case egress receipt differs from the durable ledger",
    )
    return {row["ordinal"] for row in selected}


def model_only_case_values(case: dict[str, Any]) -> list[Any]:
    score = {key: case["score"][key] for key in sorted(SCORE_KEYS)}
    return [
        case["caseId"],
        case["casePayloadSha256"],
        case["surface"],
        case["language"],
        case["modelClass"],
        case["holdout"],
        case["armId"],
        case["modelRequested"],
        case["systemSha256"],
        case["userSha256"],
        case["envelopeSha256"],
        case["systemBytes"],
        case["userBytes"],
        case["systemChars"],
        case["userChars"],
        case["projection"],
        case["outputContract"],
        case["opaqueSubstitutionCount"],
        case["opaqueSubstitutionsSha256"],
        case["callCount"],
        case["rawOutputChars"],
        case["rawOutputSha256"],
        case["outputChars"],
        case["outputSha256"],
        case["output"],
        case["provenance"],
        case["provenanceSha256"],
        case["stateApplicationPass"],
        case["durationMs"],
        case["error"],
        case["egressReceiptStartOrdinal"],
        case["egressReceiptEndOrdinal"],
        case["egressReceiptCount"],
        case["egressReceiptSha256"],
        case["redactionsEmail"],
        case["redactionsCard"],
        case["redactionsPhone"],
        case["redactionsName"],
        score,
    ]


def model_only_envelope_signature(case: dict[str, Any]) -> tuple[Any, ...]:
    return tuple(case[key] for key in MODEL_ONLY_EQUALITY_FIELDS)


def model_only_contract(surface: str) -> tuple[str, str]:
    return {
        "summary": (
            "summary_assembly",
            "summary_pipeline_assembly_then_deterministic_oracle_v6",
        ),
        "meeting_chat": ("raw_trimmed", "deterministic_oracle_v6"),
        "note_assist": (
            "raw_trimmed",
            "single_call_note_assist_deterministic_oracle_v6",
        ),
        "ask_vault": (
            "raw_trimmed",
            "single_call_no_tools_vault_answer_deterministic_oracle_v6",
        ),
        "live_current": (
            "raw_trimmed",
            "single_call_no_cascade_current_meeting_deterministic_oracle_v6",
        ),
        "live_bullets": (
            "live_bullets",
            "shared_live_bullet_parser_and_append_contract_v1",
        ),
        "light_extraction": (
            "structured_facts",
            "shared_parse_first_json_exact_fact_projection_v1",
        ),
    }[surface]


def composite_aggregate(cases: list[dict[str, Any]]) -> dict[str, Any]:
    require(bool(cases), "cannot aggregate an empty model-only cohort")
    by_surface: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for case in cases:
        by_surface[case["surface"]].append(case)
    return {
        "cases": len(cases),
        "callSuccessRate": percent(
            sum(case["error"] is None for case in cases), len(cases)
        ),
        "casePassRate": percent(
            sum(bool(case["score"]["casePass"]) for case in cases), len(cases)
        ),
        "surfaceMacroPassRate": rust_round(
            sum(
                sum(bool(case["score"]["casePass"]) for case in rows) / len(rows)
                for rows in by_surface.values()
            )
            / len(by_surface)
            * 100.0,
            1,
        ),
        "criticalFailureCases": sum(
            bool(case["score"]["criticalFailure"]) for case in cases
        ),
        "diagnosticScoreMean": mean(
            [float(case["score"]["diagnosticScore"]) for case in cases]
        ),
    }


def validate_model_only_arm_aggregates(
    arm: dict[str, Any], context: str
) -> None:
    grouped: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for case in arm["cases"]:
        for key in (
            "all_eligible",
            f"surface:{case['surface']}",
            f"language:{case['language']}",
            f"cohort:{'holdout' if case['holdout'] else 'calibration'}",
        ):
            grouped[key].append(case)
    require(
        isinstance(arm["aggregates"], dict)
        and set(arm["aggregates"]) == set(grouped),
        f"{context}: model-only aggregate groups differ",
    )
    for key, cases in grouped.items():
        stored = arm["aggregates"][key]
        require(
            isinstance(stored, dict)
            and set(stored) == COMPOSITE_AGGREGATE_KEYS
            and stored == composite_aggregate(cases),
            f"{context}/{key}: model-only aggregate does not replay",
        )


def validate_model_only_receipt(
    case: dict[str, Any], rows: list[dict[str, Any]], context: str
) -> set[int]:
    count = case["egressReceiptCount"]
    start = case["egressReceiptStartOrdinal"]
    end = case["egressReceiptEndOrdinal"]
    if case["armId"] != SOL:
        require(
            type(count) is int
            and count == 0
            and start is None
            and end is None
            and case["egressReceiptSha256"] == canonical_json_hash([])
            and all(
                case[key] == 0
                for key in (
                    "redactionsEmail",
                    "redactionsCard",
                    "redactionsPhone",
                    "redactionsName",
                )
            ),
            f"{context}: local model-only case must have an empty egress receipt",
        )
        return set()
    require(
        type(count) is int
        and count == 1
        and type(start) is int
        and end == start
        and start > 0,
        f"{context}: Sol model-only case requires exactly one receipt",
    )
    selected = [row for row in rows if row["ordinal"] == start]
    require(
        len(selected) == 1
        and selected[0]["providerId"] == "codex_cli"
        and selected[0]["modelRequested"] == "gpt-5.6-sol"
        and selected[0]["callKind"] == "complete"
        and all(
            selected[0][key] == 0
            for key in (
                "redactionsEmail",
                "redactionsCard",
                "redactionsPhone",
                "redactionsName",
            )
        )
        and case["egressReceiptSha256"] == canonical_json_hash(selected)
        and all(
            case[key] == 0
            for key in (
                "redactionsEmail",
                "redactionsCard",
                "redactionsPhone",
                "redactionsName",
            )
        ),
        f"{context}: Sol model-only durable zero-redaction receipt differs",
    )
    return {start}


def validate_receipt_partition(
    product_ordinals: set[int],
    model_only_ordinals: set[int],
    rows: list[dict[str, Any]],
    context: str,
) -> None:
    require(
        product_ordinals.isdisjoint(model_only_ordinals),
        f"{context}: product and model-only receipts overlap",
    )
    require(
        product_ordinals | model_only_ordinals
        == {row["ordinal"] for row in rows},
        f"{context}: product + model-only receipts do not partition the ledger",
    )


def validate_same_envelope_model_only(
    report: dict[str, Any],
    fixture_cases: dict[str, dict[str, Any]],
    rows: list[dict[str, Any]],
    context: str,
) -> tuple[dict[str, Any], set[int]]:
    lane = report.get("sameCallerEnvelopeModelStack")
    require(
        isinstance(lane, dict)
        and set(lane) == MODEL_ONLY_REPORT_KEYS
        and lane["laneId"] == "same_caller_envelope_model_stack_v3"
        and lane["entrypoint"] == "SummarizerProvider::complete_with_meta"
        and lane["equalityBoundary"]
        == "evaluator_owned_canonical_prescrubbed_system_user_utf8"
        and lane["providerRenderedPromptsByteIdentical"] is False
        and lane["effectiveModelInputsAttestedIdentical"] is False
        and isinstance(lane["limitations"], list)
        and len(lane["limitations"]) == 3
        and all(isinstance(value, str) and value for value in lane["limitations"]),
        f"{context}: same-envelope lane metadata/schema differs",
    )
    arms = {arm["armId"]: arm for arm in lane["arms"]}
    require(
        isinstance(lane["arms"], list)
        and len(arms) == len(lane["arms"]) == 3
        and set(arms) == {QWEN4, QWEN1, SOL},
        f"{context}: same-envelope arm set differs",
    )
    model_requested = {
        QWEN4: QWEN4_FILENAME,
        QWEN1: QWEN1_FILENAME,
        SOL: "gpt-5.6-sol",
    }
    receipt_ordinals: set[int] = set()
    for arm_id, arm in arms.items():
        require(
            isinstance(arm, dict)
            and set(arm) == MODEL_ONLY_ARM_KEYS
            and arm["modelRequested"] == model_requested[arm_id]
            and isinstance(arm["cases"], list),
            f"{context}/{arm_id}: same-envelope arm schema/identity differs",
        )
        case_ids = [case["caseId"] for case in arm["cases"]]
        require(
            len(case_ids) == len(set(case_ids))
            and set(case_ids) == EXPECTED_CASE_IDS[arm_id],
            f"{context}/{arm_id}: same-envelope exact case set differs",
        )
        for case in arm["cases"]:
            case_context = f"{context}/{arm_id}/{case['caseId']}"
            fixture = fixture_cases[case["caseId"]]
            projection, output_contract = model_only_contract(case["surface"])
            require(
                isinstance(case, dict)
                and set(case) == MODEL_ONLY_CASE_KEYS
                and set(case["score"]) == SCORE_KEYS
                and case["casePayloadSha256"] == case_payload_sha256(fixture)
                and case["surface"] == fixture["surface"]
                and case["language"] == fixture["language"]
                and case["modelClass"] == fixture["modelClass"]
                and case["holdout"] == bool(fixture.get("holdout", False))
                and case["armId"] == arm_id
                and case["modelRequested"] == model_requested[arm_id]
                and all(
                    valid_sha256(case[key])
                    for key in (
                        "systemSha256",
                        "userSha256",
                        "envelopeSha256",
                        "opaqueSubstitutionsSha256",
                        "rawOutputSha256",
                        "outputSha256",
                        "provenanceSha256",
                        "egressReceiptSha256",
                        "caseRecordSha256",
                    )
                )
                and case["projection"] == projection
                and case["outputContract"] == output_contract
                and type(case["callCount"]) is int
                and case["callCount"] == 1,
                f"{case_context}: same-envelope identity/contract differs",
            )
            require(
                type(case["systemBytes"]) is int
                and type(case["userBytes"]) is int
                and type(case["systemChars"]) is int
                and type(case["userChars"]) is int
                and case["systemBytes"] >= case["systemChars"] > 0
                and case["userBytes"] >= case["userChars"] > 0
                and type(case["opaqueSubstitutionCount"]) is int
                and case["opaqueSubstitutionCount"] >= 0
                and type(case["rawOutputChars"]) is int
                and case["rawOutputChars"] >= 0
                and type(case["outputChars"]) is int
                and case["outputChars"] == len(case["output"])
                and case["outputSha256"] == prompt_hash([case["output"]])
                and isinstance(case["provenance"], list)
                and all(isinstance(value, str) for value in case["provenance"])
                and case["provenanceSha256"] == prompt_hash(case["provenance"])
                and type(case["durationMs"]) is int
                and case["durationMs"] >= 0
                and all(
                    type(case[key]) is int and case[key] >= 0
                    for key in (
                        "redactionsEmail",
                        "redactionsCard",
                        "redactionsPhone",
                        "redactionsName",
                    )
                )
                and (case["error"] is None or isinstance(case["error"], str)),
                f"{case_context}: same-envelope byte/output/provenance fields differ",
            )
            score = case["score"]
            require(
                isinstance(score["diagnosticScore"], (int, float))
                and not isinstance(score["diagnosticScore"], bool)
                and math.isfinite(float(score["diagnosticScore"]))
                and type(score["requiredGroupsHit"]) is int
                and type(score["requiredGroupsTotal"]) is int
                and all(
                    isinstance(score[key], bool)
                    for key in SCORE_KEYS
                    - {
                        "diagnosticScore",
                        "requiredGroupsHit",
                        "requiredGroupsTotal",
                        "criticalErrors",
                    }
                )
                and isinstance(score["criticalErrors"], list)
                and all(
                    isinstance(error, str) for error in score["criticalErrors"]
                )
                and (
                    case["stateApplicationPass"] is None
                    or isinstance(case["stateApplicationPass"], bool)
                ),
                f"{case_context}: same-envelope score/state types differ",
            )
            case_receipts = validate_model_only_receipt(case, rows, case_context)
            require(
                receipt_ordinals.isdisjoint(case_receipts),
                f"{context}: overlapping same-envelope receipts",
            )
            receipt_ordinals.update(case_receipts)
            require(
                case["caseRecordSha256"]
                == canonical_json_hash(model_only_case_values(case)),
                f"{case_context}: model-only full record commitment differs",
            )
        validate_model_only_arm_aggregates(arm, f"{context}/{arm_id}")

    pair_rows = lane["pairs"]
    require(isinstance(pair_rows, list), f"{context}: model-only pairs must be an array")
    expected_pairs: dict[tuple[str, str], dict[str, Any]] = {}
    reference = {case["caseId"]: case for case in arms[SOL]["cases"]}
    for local_id in (QWEN4, QWEN1):
        for local in arms[local_id]["cases"]:
            ref = reference[local["caseId"]]
            require(
                model_only_envelope_signature(local)
                == model_only_envelope_signature(ref),
                f"{context}/{local_id}/{local['caseId']}: same caller envelope differs",
            )
            expected_pairs[(local_id, local["caseId"])] = {
                "caseId": local["caseId"],
                "casePayloadSha256": local["casePayloadSha256"],
                "surface": local["surface"],
                "holdout": local["holdout"],
                "localArm": local_id,
                "referenceArm": SOL,
                "envelopeSha256": local["envelopeSha256"],
                "localCasePass": local["score"]["casePass"],
                "referenceCasePass": ref["score"]["casePass"],
                "localCallSuccess": local["error"] is None,
                "referenceCallSuccess": ref["error"] is None,
                "localCriticalFailure": local["score"]["criticalFailure"],
                "referenceCriticalFailure": ref["score"]["criticalFailure"],
                "localDiagnosticScore": local["score"]["diagnosticScore"],
                "referenceDiagnosticScore": ref["score"]["diagnosticScore"],
                "referenceMinusLocal": rust_round(
                    float(ref["score"]["diagnosticScore"])
                    - float(local["score"]["diagnosticScore"]),
                    1,
                ),
            }
    actual_pairs: dict[tuple[str, str], dict[str, Any]] = {}
    for pair in pair_rows:
        require(
            isinstance(pair, dict) and set(pair) == MODEL_ONLY_PAIR_KEYS,
            f"{context}: model-only pair schema differs",
        )
        key = (pair["localArm"], pair["caseId"])
        require(key not in actual_pairs, f"{context}: duplicate model-only pair")
        actual_pairs[key] = pair
    require(
        actual_pairs == expected_pairs,
        f"{context}: model-only pairs do not replay from arm cases",
    )

    def paired_aggregate(local_arm: str, cohort: str) -> dict[str, Any] | None:
        subset = [
            pair
            for pair in pair_rows
            if (local_arm == "qwen-local-composite" or pair["localArm"] == local_arm)
            and (
                cohort == "all"
                or (cohort == "calibration" and not pair["holdout"])
                or (cohort == "holdout" and pair["holdout"])
            )
        ]
        if not subset:
            return None

        def surface_macro(key: str) -> float:
            grouped: dict[str, list[dict[str, Any]]] = defaultdict(list)
            for pair in subset:
                grouped[pair["surface"]].append(pair)
            return rust_round(
                sum(
                    sum(bool(pair[key]) for pair in values) / len(values)
                    for values in grouped.values()
                )
                / len(grouped)
                * 100.0,
                1,
            )

        return {
            "localArm": local_arm,
            "referenceArm": SOL,
            "cohort": cohort,
            "matchedCases": len(subset),
            "localCasePassRate": percent(
                sum(bool(pair["localCasePass"]) for pair in subset), len(subset)
            ),
            "referenceCasePassRate": percent(
                sum(bool(pair["referenceCasePass"]) for pair in subset), len(subset)
            ),
            "localCallSuccessRate": percent(
                sum(bool(pair["localCallSuccess"]) for pair in subset), len(subset)
            ),
            "referenceCallSuccessRate": percent(
                sum(bool(pair["referenceCallSuccess"]) for pair in subset), len(subset)
            ),
            "localSurfaceMacroPassRate": surface_macro("localCasePass"),
            "referenceSurfaceMacroPassRate": surface_macro("referenceCasePass"),
            "localCriticalFailureCases": sum(
                bool(pair["localCriticalFailure"]) for pair in subset
            ),
            "referenceCriticalFailureCases": sum(
                bool(pair["referenceCriticalFailure"]) for pair in subset
            ),
            "referenceMinusLocalMean": mean(
                [float(pair["referenceMinusLocal"]) for pair in subset]
            ),
        }

    expected_aggregates = {}
    for local_arm in (QWEN4, QWEN1, "qwen-local-composite"):
        for cohort in ("all", "calibration", "holdout"):
            aggregate = paired_aggregate(local_arm, cohort)
            if aggregate is not None:
                expected_aggregates[(local_arm, cohort)] = aggregate
    actual_aggregates = {}
    require(
        isinstance(lane["aggregates"], list),
        f"{context}: model-only paired aggregates must be an array",
    )
    for aggregate in lane["aggregates"]:
        require(
            isinstance(aggregate, dict)
            and set(aggregate) == MODEL_ONLY_PAIRED_AGGREGATE_KEYS,
            f"{context}: model-only paired aggregate schema differs",
        )
        key = (aggregate["localArm"], aggregate["cohort"])
        require(key not in actual_aggregates, f"{context}: duplicate paired aggregate")
        actual_aggregates[key] = aggregate
    require(
        actual_aggregates == expected_aggregates,
        f"{context}: model-only paired aggregates do not replay",
    )
    return lane, receipt_ordinals


def validate_raw_dimension_aggregates(
    arm: dict[str, Any], arm_id: str, context: str
) -> None:
    cases = arm["cases"]
    groups: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for case in cases:
        if case["comparisonScope"] == "offline_reference_ceiling":
            groups[f"reference_ceiling:surface:{case['surface']}"] .append(case)
            continue
        for key in (
            f"surface:{case['surface']}",
            f"language:{case['language']}",
            f"cohort:{'holdout' if case['holdout'] else 'calibration'}",
            "all_eligible",
        ):
            groups[key].append(case)
    product_surfaces = [
        rows for key, rows in groups.items() if key.startswith("surface:")
    ]
    if product_surfaces:
        groups["macro_surface"] = [case for rows in product_surfaces for case in rows]
    aggregates = arm["aggregates"]
    require(
        isinstance(aggregates, dict) and set(aggregates) == set(groups),
        f"{context}: raw aggregate group set differs",
    )
    for key, cases_in_group in groups.items():
        aggregate = aggregates[key]
        require(
            isinstance(aggregate, dict) and set(aggregate) == RAW_AGGREGATE_KEYS,
            f"{context}/{key}: raw aggregate schema differs",
        )
        for dimension in DIMENSION_KEYS:
            stored = aggregate[dimension]
            expected = dimension_aggregate(
                [case["dimensions"][dimension] for case in cases_in_group]
            )
            require(
                isinstance(stored, dict)
                and set(stored) == DIMENSION_AGGREGATE_KEYS
                and stored == expected,
                f"{context}/{key}: {dimension} aggregate differs from case records",
            )


def validate_raw_paired_comparison(
    report: dict[str, Any], arms: dict[str, dict[str, Any]], context: str
) -> None:
    stored = report.get("pairedComparison")
    require(
        isinstance(stored, dict)
        and set(stored) == {"cases", "aggregates"}
        and isinstance(stored["cases"], list)
        and isinstance(stored["aggregates"], list),
        f"{context}: raw paired comparison schema differs",
    )
    reference = {
        case["caseId"]: case
        for case in arms[SOL]["cases"]
        if case["comparisonScope"] == "product_path"
    }
    expected_pairs: dict[tuple[str, str], tuple[dict[str, Any], dict[str, Any]]] = {}
    for arm_id in (QWEN4, QWEN1):
        for local in arms[arm_id]["cases"]:
            if local["comparisonScope"] != "product_path":
                continue
            ref = reference.get(local["caseId"])
            if ref is not None:
                expected_pairs[(arm_id, local["caseId"])] = (local, ref)
    require(
        len(stored["cases"]) == len(expected_pairs),
        f"{context}: raw paired case count differs",
    )
    seen: set[tuple[str, str]] = set()
    for pair in stored["cases"]:
        require(
            isinstance(pair, dict) and set(pair) == RAW_PAIRED_CASE_KEYS,
            f"{context}: raw paired case schema differs",
        )
        key = (pair["localArm"], pair["caseId"])
        require(
            key in expected_pairs and key not in seen,
            f"{context}: raw paired case identity differs",
        )
        seen.add(key)
        local, ref = expected_pairs[key]
        kind = pair_comparison_kind(local["surface"])
        expected = {
            "caseId": local["caseId"],
            "casePayloadSha256": local["casePayloadSha256"],
            "surface": local["surface"],
            "comparisonKind": kind,
            "localArm": key[0],
            "referenceArm": SOL,
            "holdout": local["holdout"],
            "comparisonScope": "product_path",
            "localRouteInputSha256": local["routeInputSha256"],
            "referenceRouteInputSha256": ref["routeInputSha256"],
            "localGenerationProfile": local["generationProfile"],
            "referenceGenerationProfile": ref["generationProfile"],
            "localCasePass": local["score"]["casePass"],
            "referenceCasePass": ref["score"]["casePass"],
            "localCallSuccess": local["error"] is None,
            "referenceCallSuccess": ref["error"] is None,
            "localCriticalFailure": local["score"]["criticalFailure"],
            "referenceCriticalFailure": ref["score"]["criticalFailure"],
            "localDiagnosticScore": local["score"]["diagnosticScore"],
            "referenceDiagnosticScore": ref["score"]["diagnosticScore"],
            "referenceMinusLocal": rust_round(
                float(ref["score"]["diagnosticScore"])
                - float(local["score"]["diagnosticScore"]),
                1,
            ),
        }
        require(pair == expected, f"{context}: raw paired case does not replay")
    require(seen == set(expected_pairs), f"{context}: raw paired case set differs")

    def aggregate_for(
        local_arm: str, comparison_kind: str, cohort: str
    ) -> dict[str, Any] | None:
        subset = [
            pair
            for pair in stored["cases"]
            if (local_arm == "qwen-local-composite" or pair["localArm"] == local_arm)
            and pair["comparisonKind"] == comparison_kind
            and (
                cohort == "all"
                or (cohort == "calibration" and not pair["holdout"])
                or (cohort == "holdout" and pair["holdout"])
            )
        ]
        if not subset:
            return None

        def surface_macro(value_key: str) -> float:
            by_surface: dict[str, list[dict[str, Any]]] = defaultdict(list)
            for pair in subset:
                by_surface[pair["surface"]].append(pair)
            return rust_round(
                sum(
                    sum(bool(pair[value_key]) for pair in pairs) / len(pairs)
                    for pairs in by_surface.values()
                )
                / len(by_surface)
                * 100.0,
                1,
            )

        return {
            "localArm": local_arm,
            "referenceArm": SOL,
            "comparisonKind": comparison_kind,
            "cohort": cohort,
            "matchedCases": len(subset),
            "localCasePassRate": percent(
                sum(bool(pair["localCasePass"]) for pair in subset), len(subset)
            ),
            "referenceCasePassRate": percent(
                sum(bool(pair["referenceCasePass"]) for pair in subset), len(subset)
            ),
            "localCallSuccessRate": percent(
                sum(bool(pair["localCallSuccess"]) for pair in subset), len(subset)
            ),
            "referenceCallSuccessRate": percent(
                sum(bool(pair["referenceCallSuccess"]) for pair in subset), len(subset)
            ),
            "localSurfaceMacroPassRate": surface_macro("localCasePass"),
            "referenceSurfaceMacroPassRate": surface_macro("referenceCasePass"),
            "localCriticalFailureCases": sum(
                bool(pair["localCriticalFailure"]) for pair in subset
            ),
            "referenceCriticalFailureCases": sum(
                bool(pair["referenceCriticalFailure"]) for pair in subset
            ),
            "referenceMinusLocalMean": mean(
                [float(pair["referenceMinusLocal"]) for pair in subset]
            ),
        }

    expected_aggregates = {}
    comparison_kind = "route_specific_product_system"
    for local_arm in (QWEN4, QWEN1, "qwen-local-composite"):
        for cohort in ("all", "calibration", "holdout"):
            expected = aggregate_for(local_arm, comparison_kind, cohort)
            if expected is not None:
                expected_aggregates[(local_arm, comparison_kind, cohort)] = expected
    actual_aggregates = {}
    for aggregate in stored["aggregates"]:
        require(
            isinstance(aggregate, dict)
            and set(aggregate) == RAW_PAIRED_AGGREGATE_KEYS,
            f"{context}: raw paired aggregate schema differs",
        )
        key = (
            aggregate["localArm"],
            aggregate["comparisonKind"],
            aggregate["cohort"],
        )
        require(
            key not in actual_aggregates,
            f"{context}: duplicate raw paired aggregate",
        )
        actual_aggregates[key] = aggregate
    require(
        actual_aggregates == expected_aggregates,
        f"{context}: raw paired aggregates do not replay from paired cases",
    )


def arm_map(report: dict[str, Any]) -> dict[str, dict[str, Any]]:
    arms = report["arms"]
    mapped = {arm["metadata"]["armId"]: arm for arm in arms}
    require(len(mapped) == len(arms), "duplicate arm IDs in quality report")
    return mapped


def stable_arm_identity(arm: dict[str, Any]) -> dict[str, Any]:
    metadata = arm["metadata"]
    return {
        key: metadata.get(key)
        for key in (
            "armId",
            "modelRequested",
            "effort",
            "effortTransport",
            "effortEffectiveAttested",
            "modelClass",
            "modelFilename",
            "modelBytes",
            "modelSha256",
            "runtimeVersion",
            "runtimeSha256",
            "sidecarIdleSecs",
            "sidecarReadySecs",
            "sidecarHardCapSecs",
        )
    }


def percent(numerator: int, denominator: int) -> float:
    return rust_round(numerator / denominator * 100.0, 1) if denominator else 0.0


def mean(values: list[float]) -> float:
    return rust_round(sum(values) / len(values), 1) if values else 0.0


def rust_round(value: float, decimals: int) -> float:
    factor = 10**decimals
    scaled = value * factor
    rounded = math.floor(scaled + 0.5) if scaled >= 0 else math.ceil(scaled - 0.5)
    return rounded / factor


def summarize_cases(cases: list[dict[str, Any]]) -> dict[str, Any]:
    by_surface: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for case in cases:
        by_surface[case["surface"]].append(case)
    surface_pass_rates = {
        surface: percent(
            sum(bool(case["score"]["casePass"]) for case in rows), len(rows)
        )
        for surface, rows in sorted(by_surface.items())
    }
    dimensions = {
        dimension: dimension_aggregate(
            [case["dimensions"][dimension] for case in cases]
        )
        for dimension in sorted(DIMENSION_KEYS)
    }
    return {
        "observations": len(cases),
        "callSuccessRate": percent(
            sum(case.get("error") is None for case in cases), len(cases)
        ),
        "casePassRate": percent(
            sum(bool(case["score"]["casePass"]) for case in cases), len(cases)
        ),
        "surfaceMacroPassRate": mean(list(surface_pass_rates.values())),
        "criticalFailureObservations": sum(
            bool(case["score"]["criticalFailure"]) for case in cases
        ),
        "diagnosticScoreMean": mean(
            [float(case["score"]["diagnosticScore"]) for case in cases]
        ),
        "surfacePassRates": surface_pass_rates,
        "dimensions": dimensions,
    }


def summarize_languages(cases: list[dict[str, Any]]) -> dict[str, Any]:
    return {
        language: summarize_cases(
            [case for case in cases if case["language"] == language]
        )
        for language in sorted({case["language"] for case in cases})
    }


def combined_arm(arm_id: str, cases: list[dict[str, Any]]) -> dict[str, Any]:
    product = [case for case in cases if case["comparisonScope"] == "product_path"]
    return {
        "armId": arm_id,
        "cohorts": {
            "all": summarize_cases(product),
            "calibration": summarize_cases(
                [case for case in product if not case["holdout"]]
            ),
            "holdout": summarize_cases([case for case in product if case["holdout"]]),
        },
        "languages": summarize_languages(product),
    }


def pair_comparison_kind(surface: str) -> str:
    require(
        surface
        in {
            "summary",
            "meeting_chat",
            "note_assist",
            "ask_vault",
            "live_current",
            "live_bullets",
            "light_extraction",
        },
        f"unknown comparison surface {surface}",
    )
    return "route_specific_product_system"


def paired_summary(
    local_arm: str, local_cases: list[dict[str, Any]], reference_cases: list[dict[str, Any]]
) -> dict[str, Any]:
    reference = {
        (case["caseId"], case["_validationRepetition"]): case
        for case in reference_cases
        if case["comparisonScope"] == "product_path"
    }
    pairs = [
        (
            case,
            reference[(case["caseId"], case["_validationRepetition"])],
        )
        for case in local_cases
        if case["comparisonScope"] == "product_path"
        and (case["caseId"], case["_validationRepetition"]) in reference
    ]
    for local_case, reference_case in pairs:
        require(
            local_case["casePayloadSha256"]
            == reference_case["casePayloadSha256"],
            f"paired payload differs for {local_arm}/{local_case['caseId']}/"
            f"repetition {local_case['_validationRepetition']}",
        )

    def cohort(rows: list[tuple[dict[str, Any], dict[str, Any]]]) -> dict[str, Any]:
        local = [row[0] for row in rows]
        ref = [row[1] for row in rows]
        return {
            "matchedObservations": len(rows),
            "local": summarize_cases(local),
            "reference": summarize_cases(ref),
            "referenceMinusLocalDiagnosticMean": mean(
                [
                    float(reference_case["score"]["diagnosticScore"])
                    - float(local_case["score"]["diagnosticScore"])
                    for local_case, reference_case in rows
                ]
            ),
        }

    return {
        "localArm": local_arm,
        "referenceArm": SOL,
        "comparisonType": "route_specific_product_system",
        "routeProfilePairs": [
            {
                "caseId": local_case["caseId"],
                "repetition": local_case["_validationRepetition"],
                "casePayloadSha256": local_case["casePayloadSha256"],
                "comparisonKind": pair_comparison_kind(local_case["surface"]),
                "localRouteInputSha256": local_case["routeInputSha256"],
                "referenceRouteInputSha256": reference_case["routeInputSha256"],
                "localGenerationProfile": local_case["generationProfile"],
                "referenceGenerationProfile": reference_case["generationProfile"],
                "localProductRoute": local_case["productRoute"],
                "referenceProductRoute": reference_case["productRoute"],
            }
            for local_case, reference_case in pairs
        ],
        "cohorts": {
            "all": cohort(pairs),
            "calibration": cohort([row for row in pairs if not row[0]["holdout"]]),
            "holdout": cohort([row for row in pairs if row[0]["holdout"]]),
        },
        "comparisonKinds": {
            "route_specific_product_system": {
                "cohorts": {
                    "all": cohort(pairs),
                    "calibration": cohort(
                        [row for row in pairs if not row[0]["holdout"]]
                    ),
                    "holdout": cohort(
                        [row for row in pairs if row[0]["holdout"]]
                    ),
                }
            }
        },
        "languages": {
            language: cohort(
                [row for row in pairs if row[0]["language"] == language]
            )
            for language in sorted({row[0]["language"] for row in pairs})
        },
    }


def combined_model_only_summary(
    first_lane: dict[str, Any], second_lane: dict[str, Any]
) -> dict[str, Any]:
    arm_cases: dict[str, list[dict[str, Any]]] = defaultdict(list)
    model_requested: dict[str, str] = {}
    for repetition, lane in (("1", first_lane), ("2", second_lane)):
        for arm in lane["arms"]:
            model_requested[arm["armId"]] = arm["modelRequested"]
            arm_cases[arm["armId"]].extend(
                {**case, "_validationRepetition": repetition}
                for case in arm["cases"]
            )
    arms = []
    for arm_id in (QWEN4, QWEN1, SOL):
        cases = arm_cases[arm_id]
        arms.append(
            {
                "armId": arm_id,
                "modelRequested": model_requested[arm_id],
                "cohorts": {
                    "all": composite_aggregate(cases),
                    "calibration": composite_aggregate(
                        [case for case in cases if not case["holdout"]]
                    ),
                    "holdout": composite_aggregate(
                        [case for case in cases if case["holdout"]]
                    ),
                },
                "languages": {
                    language: composite_aggregate(
                        [case for case in cases if case["language"] == language]
                    )
                    for language in sorted({case["language"] for case in cases})
                },
            }
        )
    pairs = [
        {**pair, "_validationRepetition": repetition}
        for repetition, lane in (("1", first_lane), ("2", second_lane))
        for pair in lane["pairs"]
    ]

    def pair_summary(rows: list[dict[str, Any]]) -> dict[str, Any]:
        by_surface: dict[str, list[dict[str, Any]]] = defaultdict(list)
        for row in rows:
            by_surface[row["surface"]].append(row)

        def macro(key: str) -> float:
            return rust_round(
                sum(
                    sum(bool(row[key]) for row in values) / len(values)
                    for values in by_surface.values()
                )
                / len(by_surface)
                * 100.0,
                1,
            )

        return {
            "matchedObservations": len(rows),
            "localCasePassRate": percent(
                sum(bool(row["localCasePass"]) for row in rows), len(rows)
            ),
            "referenceCasePassRate": percent(
                sum(bool(row["referenceCasePass"]) for row in rows), len(rows)
            ),
            "localCallSuccessRate": percent(
                sum(bool(row["localCallSuccess"]) for row in rows), len(rows)
            ),
            "referenceCallSuccessRate": percent(
                sum(bool(row["referenceCallSuccess"]) for row in rows), len(rows)
            ),
            "localSurfaceMacroPassRate": macro("localCasePass"),
            "referenceSurfaceMacroPassRate": macro("referenceCasePass"),
            "localCriticalFailureObservations": sum(
                bool(row["localCriticalFailure"]) for row in rows
            ),
            "referenceCriticalFailureObservations": sum(
                bool(row["referenceCriticalFailure"]) for row in rows
            ),
            "referenceMinusLocalDiagnosticMean": mean(
                [float(row["referenceMinusLocal"]) for row in rows]
            ),
        }

    paired = []
    for local_arm in (QWEN4, QWEN1, "qwen-local-composite"):
        local_pairs = (
            pairs
            if local_arm == "qwen-local-composite"
            else [row for row in pairs if row["localArm"] == local_arm]
        )
        paired.append(
            {
                "localArm": local_arm,
                "referenceArm": SOL,
                "cohorts": {
                    "all": pair_summary(local_pairs),
                    "calibration": pair_summary(
                        [row for row in local_pairs if not row["holdout"]]
                    ),
                    "holdout": pair_summary(
                        [row for row in local_pairs if row["holdout"]]
                    ),
                },
            }
        )
    return {
        "laneId": first_lane["laneId"],
        "entrypoint": first_lane["entrypoint"],
        "equalityBoundary": first_lane["equalityBoundary"],
        "providerRenderedPromptsByteIdentical": False,
        "effectiveModelInputsAttestedIdentical": False,
        "interpretation": (
            "same evaluator-owned caller envelope and one provider-trait call per candidate; "
            "provider-rendered and effective model inputs are not attested identical"
        ),
        "arms": arms,
        "paired": paired,
    }


def validate_and_combine(
    first: dict[str, Any],
    second: dict[str, Any],
    paths: list[Path],
    *,
    producer_snapshot: dict[str, Any] | None = None,
    runtime_identities: dict[str, Any] | None = None,
    input_hashes_by_repetition: dict[str, str] | None = None,
) -> dict[str, Any]:
    require(
        canonical_json_hash(
            ["Zażółć", None, ["x", ""], 1.5, True, {"escaped": "line\nquote\""}]
        )
        == "f46b4ad1428b63de91b91de4897faaaa9d7c426e63236a4d812b42e0a092bb0e",
        "Python canonical JSON encoding differs from the Rust known vector",
    )
    reports = [first, second]
    by_repetition: dict[str, dict[str, Any]] = {}
    paths_by_repetition: dict[str, Path] = {}
    egress_rows_by_repetition: dict[str, list[dict[str, Any]]] = {}
    retrieval_by_repetition: dict[str, dict[str, Any]] = {}
    measurement_hashes = current_measurement_hashes()
    source_fingerprint = current_source_fingerprint()
    if producer_snapshot is None:
        current_commit, current_tracked_diff = current_git_identity()
    else:
        require(
            set(producer_snapshot) == SNAPSHOT_KEYS,
            "evidence producer snapshot schema differs",
        )
        current_commit = producer_snapshot["repositoryCommit"]
        current_tracked_diff = producer_snapshot["trackedDiffSha256"]
        require(
            source_fingerprint == producer_snapshot["sourceFingerprintSha256"],
            "current quality source fingerprint differs from committed evidence",
        )
        for key, current_hash in measurement_hashes.items():
            require(
                producer_snapshot[key] == current_hash,
                f"current {key} differs from committed evidence producer source",
            )
    fixture_path = MEASUREMENT_FILES["fixtureFileSha256"]
    fixture = load(fixture_path)
    fixture_cases = validate_fixture(fixture, str(fixture_path))
    current_manifest_hash = prompt_hash([fixture_path.read_text(encoding="utf-8")])
    if producer_snapshot is not None:
        require(
            current_manifest_hash == producer_snapshot["manifestSha256"],
            "current quality manifest differs from committed evidence",
        )
    for path, report in zip(paths, reports):
        validate_artifact_privacy(report, str(path))
        require(
            set(report) == REPORT_KEYS,
            f"{path}: quality report root schema differs",
        )
        require(report.get("schemaVersion") == 9, f"{path}: expected schemaVersion 9")
        require(
            report.get("syntheticOnly") is True
            and set(report["environment"]) == ENVIRONMENT_KEYS,
            f"{path}: report environment/synthetic contract differs",
        )
        require(
            report["environment"]["nameRedactorMode"]
            == "forced_noop_for_deterministic_synthetic_benchmark"
            and all(
                report["environment"][key] is None
                or (
                    isinstance(report["environment"][key], str)
                    and bool(report["environment"][key])
                )
                for key in ("osVersion", "osBuild")
            ),
            f"{path}: OS or deterministic name-redactor provenance differs",
        )
        require(
            report["holdoutInterpretation"]
            == "legacy_pre_remediation_tag_not_untouched_generalization",
            f"{path}: legacy holdout interpretation differs",
        )
        repetition = str(report["environment"]["repetition"])
        require(repetition in EXPECTED_ORDERS, f"{path}: unexpected repetition {repetition}")
        require(repetition not in by_repetition, f"duplicate repetition {repetition}")
        require(
            report["environment"]["armOrder"] == EXPECTED_ORDERS[repetition],
            f"{path}: arm order is not preregistered for repetition {repetition}",
        )
        require(
            report["snapshotStart"] == report["snapshotEnd"],
            f"{path}: source/runtime worktree changed during the run",
        )
        snapshot = report["snapshotStart"]
        if producer_snapshot is not None:
            require(
                snapshot == producer_snapshot,
                f"{path}: run snapshot differs from committed evidence manifest",
            )
        require(
            report["repositoryCommit"] == snapshot["repositoryCommit"] == current_commit,
            f"{path}: top-level, snapshot, or current repository commit differs",
        )
        require(
            report["sourceFingerprintSha256"]
            == snapshot["sourceFingerprintSha256"]
            == source_fingerprint,
            f"{path}: top-level, snapshot, or current source fingerprint differs",
        )
        require(
            report["manifestSha256"]
            == snapshot["manifestSha256"]
            == current_manifest_hash,
            f"{path}: top-level, snapshot, or current manifest hashes differ",
        )
        require(
            report["environment"]["trackedDiffSha256"]
            == snapshot["trackedDiffSha256"]
            == current_tracked_diff,
            f"{path}: environment, snapshot, or current tracked diff differs",
        )
        require(
            report["environment"]["workingTreeDirty"] == snapshot["workingTreeDirty"],
            f"{path}: environment and snapshot dirty-state fields differ",
        )
        for key, current_hash in measurement_hashes.items():
            require(
                report["snapshotStart"].get(key) == current_hash,
                f"{path}: current {key} differs from the run-bound measurement file",
            )
        egress_rows_by_repetition[repetition] = validate_egress_ledger(
            report, str(path)
        )
        retrieval_by_repetition[repetition] = validate_retrieval_quality(
            report, str(path)
        )
        by_repetition[repetition] = report
        paths_by_repetition[repetition] = path

    require(set(by_repetition) == {"1", "2"}, "need exactly repetitions 1 and 2")
    first = by_repetition["1"]
    second = by_repetition["2"]
    for key in (
        "repositoryCommit",
        "sourceFingerprintSha256",
        "manifestSha256",
        "promptVersion",
        "syntheticOnly",
    ):
        require(first.get(key) == second.get(key), f"repeat mismatch: {key}")
    require(
        first["snapshotStart"] == second["snapshotStart"],
        "repeat source/fixture/diff snapshots differ",
    )
    require(
        retrieval_by_repetition["1"] == retrieval_by_repetition["2"],
        "independent real-embedder retrieval evidence differs across repetitions",
    )

    first_arms = arm_map(first)
    second_arms = arm_map(second)
    require(len(first["arms"]) == 3, "repetition 1 must contain exactly three arms")
    require(len(second["arms"]) == 3, "repetition 2 must contain exactly three arms")
    require(set(first_arms) == {QWEN4, QWEN1, SOL}, "repetition 1 arm set differs")
    require(set(second_arms) == set(first_arms), "repeat arm sets differ")

    model_only_by_repetition: dict[str, dict[str, Any]] = {}
    model_only_receipts: dict[str, set[int]] = {}
    for repetition, report in (("1", first), ("2", second)):
        lane, ordinals = validate_same_envelope_model_only(
            report,
            fixture_cases,
            egress_rows_by_repetition[repetition],
            f"repetition {repetition}/same-envelope-model-only",
        )
        model_only_by_repetition[repetition] = lane
        model_only_receipts[repetition] = ordinals
    for arm_id in (QWEN4, QWEN1, SOL):
        first_cases = {
            case["caseId"]: case
            for arm in model_only_by_repetition["1"]["arms"]
            if arm["armId"] == arm_id
            for case in arm["cases"]
        }
        second_cases = {
            case["caseId"]: case
            for arm in model_only_by_repetition["2"]["arms"]
            if arm["armId"] == arm_id
            for case in arm["cases"]
        }
        for case_id in first_cases:
            for key in (
                "casePayloadSha256",
                "surface",
                "language",
                "modelClass",
                "holdout",
                "armId",
                "modelRequested",
                "systemSha256",
                "userSha256",
                "envelopeSha256",
                "systemBytes",
                "userBytes",
                "systemChars",
                "userChars",
                "projection",
                "outputContract",
                "opaqueSubstitutionCount",
                "opaqueSubstitutionsSha256",
                "callCount",
            ):
                require(
                    first_cases[case_id][key] == second_cases[case_id][key],
                    f"repeat same-envelope field differs for {arm_id}/{case_id}/{key}",
                )

    if runtime_identities is None:
        sidecar_env = os.environ.get("MURMUR_BRAIN_SIDECAR", "").strip()
        sidecar_path = (
            Path(sidecar_env)
            if sidecar_env
            else REPO_ROOT / "target/debug/murmur-brain"
        )
        require(sidecar_path.is_file(), f"current local runtime is missing: {sidecar_path}")
        current_local_runtime_version = LOCAL_RUNTIME_VERSION
        current_local_runtime_sha = sha256_file(sidecar_path)
        require(CODEX_BINARY.is_file(), f"current Codex runtime is missing: {CODEX_BINARY}")
        current_codex_runtime_sha = sha256_file(CODEX_BINARY)
        codex_version_result = subprocess.run(
            [str(CODEX_BINARY), "--version"],
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        current_codex_runtime_version = codex_version_result.stdout.strip()
        require(
            codex_version_result.returncode == 0 and bool(current_codex_runtime_version),
            "cannot re-identify the current Codex runtime",
        )
    else:
        require(
            set(runtime_identities) == {"local", "codex"}
            and set(runtime_identities["local"]) == {"version", "sha256"}
            and set(runtime_identities["codex"]) == {"version", "sha256"},
            "evidence runtime identity schema differs",
        )
        current_local_runtime_version = runtime_identities["local"]["version"]
        current_local_runtime_sha = runtime_identities["local"]["sha256"]
        current_codex_runtime_version = runtime_identities["codex"]["version"]
        current_codex_runtime_sha = runtime_identities["codex"]["sha256"]
        require(
            current_local_runtime_version == LOCAL_RUNTIME_VERSION
            and valid_sha256(current_local_runtime_sha)
            and isinstance(current_codex_runtime_version, str)
            and bool(current_codex_runtime_version)
            and valid_sha256(current_codex_runtime_sha),
            "evidence runtime identities are invalid",
        )
    for repetition, arms in (("1", first_arms), ("2", second_arms)):
        qwen_runtime_identities = {
            (
                arms[arm_id]["metadata"]["runtimeVersion"],
                arms[arm_id]["metadata"]["runtimeSha256"],
            )
            for arm_id in (QWEN4, QWEN1)
        }
        require(
            qwen_runtime_identities
            == {(current_local_runtime_version, current_local_runtime_sha)},
            f"repetition {repetition}: Qwen arms do not share the current local runtime",
        )
        require(
            (
                arms[SOL]["metadata"]["runtimeVersion"],
                arms[SOL]["metadata"]["runtimeSha256"],
            )
            == (current_codex_runtime_version, current_codex_runtime_sha),
            f"repetition {repetition}: Sol runtime differs from the current Codex binary",
        )
    combined: list[dict[str, Any]] = []
    combined_cases: dict[str, list[dict[str, Any]]] = {}
    receipt_ordinals: dict[str, set[int]] = {"1": set(), "2": set()}
    for arm_id in (QWEN4, QWEN1, SOL):
        left = first_arms[arm_id]
        right = second_arms[arm_id]
        for repetition, arm in (("1", left), ("2", right)):
            require(
                set(arm) == {"metadata", "aggregates", "cases"}
                and isinstance(arm["aggregates"], dict),
                f"repetition {repetition}/{arm_id}: arm report schema differs",
            )
        validate_arm_metadata(arm_id, left["metadata"], f"repetition 1/{arm_id}")
        validate_arm_metadata(arm_id, right["metadata"], f"repetition 2/{arm_id}")
        require(
            stable_arm_identity(left) == stable_arm_identity(right),
            f"repeat runtime/model identity differs for {arm_id}",
        )
        for repetition, arm in (("1", left), ("2", right)):
            case_ids = [case["caseId"] for case in arm["cases"]]
            require(
                len(case_ids) == len(set(case_ids)),
                f"repetition {repetition}: duplicate case IDs for {arm_id}",
            )
            require(
                set(case_ids) == EXPECTED_CASE_IDS[arm_id],
                f"repetition {repetition}: exact case set differs for {arm_id}",
            )
            for case in arm["cases"]:
                fixture_case = fixture_cases[case["caseId"]]
                validate_case_content(
                    case,
                    fixture_case,
                    arm_id,
                    f"repetition {repetition}/{arm_id}/{case['caseId']}",
                )
                case_receipt_ordinals = validate_case_receipt(
                    case,
                    arm_id,
                    egress_rows_by_repetition[repetition],
                    f"repetition {repetition}/{arm_id}/{case['caseId']}",
                )
                require(
                    receipt_ordinals[repetition].isdisjoint(case_receipt_ordinals),
                    f"repetition {repetition}: overlapping per-case egress receipts",
                )
                receipt_ordinals[repetition].update(case_receipt_ordinals)
                require(
                    case["surface"] == fixture_case["surface"]
                    and case["language"] == fixture_case["language"]
                    and case["modelClass"] == fixture_case["modelClass"]
                    and case["holdout"] == bool(fixture_case.get("holdout", False)),
                    f"repetition {repetition}: case metadata differs from fixture for "
                    f"{arm_id}/{case['caseId']}",
                )
                expected_scope = (
                    "offline_reference_ceiling"
                    if arm_id == SOL and case["caseId"] == "live-bullets-pl-polaris"
                    else "product_path"
                )
                require(
                    case["comparisonScope"] == expected_scope,
                    f"repetition {repetition}: wrong comparison scope for "
                    f"{arm_id}/{case['caseId']}",
                )
            validate_raw_dimension_aggregates(
                arm, arm_id, f"repetition {repetition}/{arm_id}"
            )
        left_inputs = {
            (
                case["caseId"],
                case["casePayloadSha256"],
                case["routeInputSha256"],
                case["generationProfile"],
            )
            for case in left["cases"]
        }
        right_inputs = {
            (
                case["caseId"],
                case["casePayloadSha256"],
                case["routeInputSha256"],
                case["generationProfile"],
            )
            for case in right["cases"]
        }
        require(left_inputs == right_inputs, f"repeat route inputs differ for {arm_id}")
        tagged_cases = [
            {**case, "_validationRepetition": repetition}
            for repetition, arm in (("1", left), ("2", right))
            for case in arm["cases"]
        ]
        combined_cases[arm_id] = tagged_cases
        combined.append(combined_arm(arm_id, tagged_cases))

    for repetition in ("1", "2"):
        validate_receipt_partition(
            receipt_ordinals[repetition],
            model_only_receipts[repetition],
            egress_rows_by_repetition[repetition],
            f"repetition {repetition}",
        )
    validate_raw_paired_comparison(first, first_arms, "repetition 1")
    validate_raw_paired_comparison(second, second_arms, "repetition 2")

    paired = [
        paired_summary(local_arm, combined_cases[local_arm], combined_cases[SOL])
        for local_arm in (QWEN4, QWEN1)
    ]
    paired.append(
        paired_summary(
            "qwen-local-composite",
            combined_cases[QWEN4] + combined_cases[QWEN1],
            combined_cases[SOL],
        )
    )
    require(
        paired[0]["cohorts"]["all"]["matchedObservations"] == 30,
        "Qwen 4B/Sol paired observation count differs",
    )
    require(
        paired[1]["cohorts"]["all"]["matchedObservations"] == 4,
        "Qwen 1.7B/Sol paired observation count differs",
    )
    require(
        paired[2]["cohorts"]["all"]["matchedObservations"] == 34,
        "local composite/Sol common-product observation count differs",
    )
    local_product_cases = combined_cases[QWEN4] + combined_cases[QWEN1]

    combined_report = {
        "schemaVersion": 5,
        "design": (
            "two serial repetitions with pairwise-reversed local/reference order; "
            "candidate-independent fixture payloads compared through route-specific product "
            "systems in a bound candidate source snapshot, not a released build or full UI; "
            "targeted synthetic probes, not a state-of-the-art, general factuality, prose, or "
            "generalization benchmark; holdout is a legacy pre-remediation tag; the Sol arm "
            "requested gpt-5.6-sol with high effort but effective served model/effort are unattested"
        ),
        "comparisonType": "separate_product_route_and_same_caller_envelope_lanes",
        "holdoutInterpretation": (
            "legacy_pre_remediation_tag_not_untouched_generalization"
        ),
        "dimensionAttribution": {
            "retrievalQuality": (
                "generation cases are not_measured for Ask because results are injected; the "
                "separate top-level retrievalQuality lane measures the production reader "
                "implementations in the bound source snapshot from content-free hashed rankings"
            ),
            "toolAgentExecution": (
                "measured only for the Sol cloud Ask/Live agent loops; deterministic floors and "
                "single-completion routes are not_applicable"
            ),
            "finalProductOutputContract": (
                "deterministic postprocessed output/state/schema/provenance contract"
            ),
        },
        "rawAggregatePolicy": (
            "raw per-run arm aggregates are retained for audit but not trusted here; every combined, "
            "paired, cohort, language, and surface value below is recomputed from fully committed "
            "case records"
        ),
        "repositoryCommit": first["repositoryCommit"],
        "sourceFingerprintSha256": first["sourceFingerprintSha256"],
        "manifestSha256": first["manifestSha256"],
        "repeatFilesByRepetition": {
            repetition: str(paths_by_repetition[repetition])
            for repetition in ("1", "2")
        },
        "inputResultSha256ByRepetition": {
            repetition: (
                input_hashes_by_repetition[repetition]
                if input_hashes_by_repetition is not None
                else sha256_file(paths_by_repetition[repetition])
            )
            for repetition in ("1", "2")
        },
        "measurementFileSha256": measurement_hashes,
        "retrievalQuality": retrieval_by_repetition["1"],
        "sameCallerEnvelopeModelStack": combined_model_only_summary(
            model_only_by_repetition["1"], model_only_by_repetition["2"]
        ),
        "arms": combined,
        "paired": paired,
        "localComposite": {
            "cohorts": {
                "all": summarize_cases(local_product_cases),
                "calibration": summarize_cases(
                    [case for case in local_product_cases if not case["holdout"]]
                ),
                "holdout": summarize_cases(
                    [case for case in local_product_cases if case["holdout"]]
                ),
            },
            "languages": summarize_languages(local_product_cases),
        },
    }
    validate_artifact_privacy(combined_report, "combined report")
    return combined_report


def verify_committed_evidence(manifest_path: Path) -> None:
    evidence = load(manifest_path)
    require(set(evidence) == EVIDENCE_MANIFEST_KEYS, "evidence manifest schema differs")
    require(evidence["schemaVersion"] == 1, "unexpected evidence schemaVersion")
    require(
        evidence["kind"] == "murmur_local_cloud_quality_evidence"
        and evidence["evidenceMethod"]
        == "deterministic_code_owned_oracles_no_model_judge",
        "evidence method differs or permits a model judge",
    )
    repetitions = evidence["repetitions"]
    require(
        isinstance(repetitions, dict) and set(repetitions) == {"1", "2"},
        "evidence must bind exactly repetitions 1 and 2",
    )
    reports: list[dict[str, Any]] = []
    logical_paths: list[Path] = []
    logical_hashes: dict[str, str] = {}
    for repetition in ("1", "2"):
        entry = repetitions[repetition]
        require(
            isinstance(entry, dict)
            and set(entry)
            == {"archivePath", "archiveSha256", "logicalPath", "logicalSha256"},
            f"repetition {repetition}: evidence entry schema differs",
        )
        archive_path = resolve_evidence_path(
            entry["archivePath"], f"repetition {repetition} archive"
        )
        require(archive_path.suffix == ".gz", f"{archive_path}: expected .gz archive")
        gzip_header = archive_path.read_bytes()[:10]
        require(
            len(gzip_header) == 10
            and gzip_header[:3] == b"\x1f\x8b\x08"
            and gzip_header[3] == 0
            and gzip_header[4:8] == b"\x00\x00\x00\x00",
            f"{archive_path}: gzip header must omit filename/flags and use mtime=0",
        )
        require(
            sha256_file(archive_path) == entry["archiveSha256"],
            f"{archive_path}: archive SHA-256 differs",
        )
        logical_bytes = read_gzip_capped(archive_path)
        require(
            sha256_bytes(logical_bytes) == entry["logicalSha256"],
            f"{archive_path}: decompressed JSON SHA-256 differs",
        )
        logical_path = Path(entry["logicalPath"])
        require(
            not logical_path.is_absolute()
            and ".." not in logical_path.parts
            and logical_path.suffix == ".json",
            f"repetition {repetition}: logical path must be repository-relative JSON",
        )
        require(
            logical_path == Path(entry["archivePath"]).with_suffix("")
            and logical_path not in logical_paths,
            f"repetition {repetition}: logical path must uniquely name the decompressed archive",
        )
        reports.append(load_bytes(logical_bytes, str(logical_path)))
        logical_paths.append(logical_path)
        logical_hashes[repetition] = entry["logicalSha256"]

    combined_entry = evidence["combined"]
    require(
        isinstance(combined_entry, dict)
        and set(combined_entry) == {"path", "sha256"},
        "combined evidence entry schema differs",
    )
    combined_path = resolve_evidence_path(combined_entry["path"], "combined evidence")
    require(
        sha256_file(combined_path) == combined_entry["sha256"],
        f"{combined_path}: combined SHA-256 differs",
    )
    committed_combined = load(combined_path)
    recomputed = validate_and_combine(
        reports[0],
        reports[1],
        logical_paths,
        producer_snapshot=evidence["producerSnapshot"],
        runtime_identities=evidence["runtimeIdentities"],
        input_hashes_by_repetition=logical_hashes,
    )
    require(
        recomputed == committed_combined,
        "committed combined report differs from full case-record recomputation",
    )
    print(
        "committed quality structural evidence verified: archives, logical hashes, producer "
        "snapshot, model/runtime provenance, stored-record commitments, reversed schedule, and "
        "combined report; the calling Rust test must independently recompute every score"
    )


def run_selftests() -> None:
    require(
        REQUIRED_SOURCE_FILES.issubset(SOURCE_FILES),
        "selftest: a required source dependency is absent from the fingerprint",
    )
    require(
        len(SOURCE_FILES) == len(set(SOURCE_FILES)),
        "selftest: source fingerprint contains duplicate dependencies",
    )
    require(
        rust_source_fingerprint_files() == SOURCE_FILES,
        "selftest: Rust and Python source-fingerprint dependency lists differ",
    )
    require(
        all((REPO_ROOT / "src-tauri" / relative).is_file() for relative in SOURCE_FILES),
        "selftest: a source fingerprint dependency does not exist",
    )
    fixture = load(MEASUREMENT_FILES["fixtureFileSha256"])
    cases = validate_fixture(fixture, "selftest fixture")
    original = cases["fact-extract-en-helix"]
    identity_only_mutation = json.loads(json.dumps(original))
    identity_only_mutation["modelClass"] = "heavy"
    identity_only_mutation["holdout"] = True
    identity_only_mutation["expected"]["structuredFacts"][0]["object"] = "mutated"
    require(
        case_payload_sha256(identity_only_mutation) == case_payload_sha256(original),
        "selftest: candidate/cohort/oracle metadata leaked into case payload commitment",
    )
    input_mutation = json.loads(json.dumps(original))
    input_mutation["transcript"] += " changed"
    require(
        case_payload_sha256(input_mutation) != case_payload_sha256(original),
        "selftest: input mutation did not change case payload commitment",
    )
    require(
        dimension_aggregate(["not_measured", "not_applicable"])
        == {
            "observations": 2,
            "applicableObservations": 1,
            "measuredObservations": 0,
            "passedObservations": 0,
            "failedObservations": 0,
            "notMeasuredObservations": 1,
            "notApplicableObservations": 1,
            "coverageRate": 0.0,
            "passRate": None,
        },
        "selftest: unmeasured dimension aggregate acquired a pass rate",
    )

    score = {
        "diagnosticScore": 100.0,
        "casePass": True,
        "criticalFailure": False,
        "requiredGroupsHit": 1,
        "requiredGroupsTotal": 1,
        "formatPass": True,
        "sectionPass": True,
        "languagePass": True,
        "forbiddenPass": True,
        "constraintPass": True,
        "provenancePass": True,
        "toolPolicyPass": True,
        "relationPass": True,
        "stateApplicationPass": True,
        "branchConvergencePass": True,
        "closedWorldPass": True,
        "structuredLabelsPass": True,
        "criticalErrors": [],
    }
    model_only_case = {
        "caseId": original["id"],
        "casePayloadSha256": case_payload_sha256(original),
        "surface": original["surface"],
        "language": original["language"],
        "modelClass": original["modelClass"],
        "holdout": False,
        "armId": QWEN1,
        "modelRequested": QWEN1_FILENAME,
        "systemSha256": "1" * 64,
        "userSha256": "2" * 64,
        "envelopeSha256": "3" * 64,
        "systemBytes": 120,
        "userBytes": 80,
        "systemChars": 110,
        "userChars": 75,
        "projection": "structured_facts",
        "outputContract": "shared_parse_first_json_exact_fact_projection_v1",
        "opaqueSubstitutionCount": 0,
        "opaqueSubstitutionsSha256": canonical_json_hash([]),
        "callCount": 1,
        "rawOutputChars": 2,
        "rawOutputSha256": prompt_hash(["{}"]),
        "outputChars": 2,
        "outputSha256": prompt_hash(["{}"]),
        "output": "{}",
        "provenance": [],
        "provenanceSha256": prompt_hash([]),
        "stateApplicationPass": None,
        "durationMs": 1,
        "error": None,
        "egressReceiptStartOrdinal": None,
        "egressReceiptEndOrdinal": None,
        "egressReceiptCount": 0,
        "egressReceiptSha256": canonical_json_hash([]),
        "redactionsEmail": 0,
        "redactionsCard": 0,
        "redactionsPhone": 0,
        "redactionsName": 0,
        "score": score,
        "caseRecordSha256": "",
    }
    base_model_only_hash = canonical_json_hash(model_only_case_values(model_only_case))
    envelope_mutation = json.loads(json.dumps(model_only_case))
    envelope_mutation["envelopeSha256"] = "4" * 64
    require(
        model_only_envelope_signature(envelope_mutation)
        != model_only_envelope_signature(model_only_case)
        and canonical_json_hash(model_only_case_values(envelope_mutation))
        != base_model_only_hash,
        "selftest: model-only envelope mutation was not detected",
    )
    call_mutation = json.loads(json.dumps(model_only_case))
    call_mutation["callCount"] = 2
    require(
        call_mutation["callCount"] != 1
        and canonical_json_hash(model_only_case_values(call_mutation))
        != base_model_only_hash,
        "selftest: model-only call-count mutation was not detected",
    )
    validate_model_only_receipt(model_only_case, [], "selftest/model-only-receipt")
    receipt_mutation = json.loads(json.dumps(model_only_case))
    receipt_mutation["egressReceiptCount"] = 1
    try:
        validate_model_only_receipt(receipt_mutation, [], "selftest/mutated-receipt")
    except ValueError:
        pass
    else:
        raise ValueError("selftest: model-only receipt mutation was accepted")
    validate_receipt_partition({2}, {1}, [{"ordinal": 1}, {"ordinal": 2}], "selftest")
    try:
        validate_receipt_partition(
            {1, 2}, {1}, [{"ordinal": 1}, {"ordinal": 2}], "selftest/mutated"
        )
    except ValueError:
        pass
    else:
        raise ValueError("selftest: overlapping receipt partition was accepted")
    aggregate = composite_aggregate([model_only_case])
    aggregate_mutation = dict(aggregate)
    aggregate_mutation["casePassRate"] = 0.0
    require(
        aggregate_mutation != composite_aggregate([model_only_case]),
        "selftest: model-only aggregate mutation was not detected",
    )

    retrieval_fixture_path = (
        REPO_ROOT / "src-tauri/src/eval/fixtures/rag-bakeoff-synthetic.json"
    )
    retrieval_text = retrieval_fixture_path.read_text(encoding="utf-8")
    retrieval_fixture = json.loads(retrieval_text)
    retrieval_modes = (
        "fts_product",
        "semantic_product_floor",
        "hybrid_product",
    )
    retrieval_cases = []
    for index, query in enumerate(retrieval_fixture, start=1):
        expected_ids = query["expected_meeting_ids"]
        expected_hashes = [
            prompt_hash(["murmur-retrieval-meeting-id-v1", value])
            for value in expected_ids
        ]
        ranking = expected_hashes[:5]
        metrics = retrieval_metric_from_hashes(ranking, expected_hashes, 5)
        retrieval_cases.append(
            {
                "caseId": f"retrieval-{index:02}",
                "language": query.get("lang", ""),
                "queryPayloadSha256": prompt_hash(
                    [
                        "murmur-retrieval-case-payload-v2",
                        query.get("lang", ""),
                        query["query"],
                        *expected_ids,
                    ]
                ),
                "expectedMeetings": len(expected_ids),
                "expectedIdHashes": expected_hashes,
                "rankings": {mode: list(ranking) for mode in retrieval_modes},
                "metrics": {mode: dict(metrics) for mode in retrieval_modes},
            }
        )

    def retrieval_aggregates() -> dict[str, Any]:
        subsets = {
            "all": retrieval_cases,
            "language:pl": [
                case for case in retrieval_cases if case["language"] == "pl"
            ],
            "language:en": [
                case for case in retrieval_cases if case["language"] == "en"
            ],
        }
        output = {}
        for cohort, rows in subsets.items():
            output[cohort] = {}
            for mode in retrieval_modes:
                output[cohort][mode] = {
                    "recallAtK": sum(
                        row["metrics"][mode]["recallAtK"] for row in rows
                    )
                    / len(rows),
                    "ndcgAtK": sum(
                        row["metrics"][mode]["ndcgAtK"] for row in rows
                    )
                    / len(rows),
                    "mrr": sum(
                        row["metrics"][mode]["reciprocalRank"] for row in rows
                    )
                    / len(rows),
                    "queries": len(rows),
                }
        return output

    retrieval_evidence = {
        "required": True,
        "surface": "ask_vault_retrieval",
        "attribution": "independent_synthetic_retrieval_lane_not_generation_quality",
        "fixtureSha256": prompt_hash([retrieval_text]),
        "corpusSourceSha256": sha256_file(REPO_ROOT / "src-tauri/src/eval/corpus.rs"),
        "embedderId": "selftest-real-embedder",
        "realEmbedder": True,
        "modelFiles": [{"filename": "model.bin", "bytes": 1, "sha256": "5" * 64}],
        "anchorDate": "2026-06-29",
        "k": 5,
        "candidateLimit": 20,
        "cosineFloor": 0.78,
        "cases": retrieval_cases,
        "aggregates": retrieval_aggregates(),
        "visibilityGate": "Db::search_visible_in_range + Db::search_semantic_visible + Db::search_hybrid_visible with empty session-unlock set",
        "temporaryDatabaseCleaned": True,
    }
    validate_retrieval_quality(
        {"retrievalQuality": retrieval_evidence}, "selftest/retrieval"
    )
    retrieval_mutation = json.loads(json.dumps(retrieval_evidence))
    retrieval_mutation["cases"][0]["rankings"]["fts_product"].insert(0, "6" * 64)
    try:
        validate_retrieval_quality(
            {"retrievalQuality": retrieval_mutation}, "selftest/mutated-retrieval"
        )
    except ValueError:
        pass
    else:
        raise ValueError("selftest: retrieval ranking mutation was accepted")

    canaries = {
        "email": "person@example.test",
        "phone": "+48 600 700 800",
        "macos_user_path": "/Users/private-user/Documents/note.md",
        "unix_user_path": "/home/private-user/note.md",
        "windows_user_path": "C:\\Users\\private-user\\note.md",
        "external_url": "https://example.test/private",
        "file_url": "file:///Users/private-user/note.md",
        "pem_private_key": "-----BEGIN PRIVATE KEY-----",
        "bearer_token": "Bearer abcdefghijklmnopqrstuvwxyz",
        "jwt": "eyJabcdefgh.eyJijklmnop.abcdefghijkl",
        "api_token": "sk-abcdefghijklmnopqrstuvwxyz",
    }
    for expected_rule, canary in canaries.items():
        violation = privacy_violation({"nested": [canary]})
        require(
            violation is not None and violation[0] == expected_rule,
            f"selftest: privacy canary was not rejected by {expected_rule}",
        )
    require(
        privacy_violation(
            {
                "date": "2026-12-03",
                "hash": "2fde00ce69dd4899c70d020845e2638353015bba0fdf161b3eb965f2bca4464e",
                "route": "ask-vault-cloud-agentic",
            }
        )
        is None,
        "selftest: ordinary synthetic metadata triggered the privacy scanner",
    )
    print(
        "quality validator selftests passed: schema v9 fixture, model-stack envelope/call/receipt/"
        "aggregate mutations, retrieval ranking replay mutation, dimension null semantics, and "
        "artifact privacy canaries"
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("repetition_one", nargs="?", type=Path)
    parser.add_argument("repetition_two", nargs="?", type=Path)
    parser.add_argument("--out", type=Path)
    parser.add_argument("--verify-evidence", type=Path)
    parser.add_argument("--selftest", action="store_true")
    args = parser.parse_args()
    if args.selftest:
        try:
            require(
                args.repetition_one is None
                and args.repetition_two is None
                and args.out is None
                and args.verify_evidence is None,
                "--selftest cannot be combined with inputs or evidence verification",
            )
            run_selftests()
        except (KeyError, TypeError, ValueError, OSError, json.JSONDecodeError) as error:
            print(f"quality validator selftest failed: {error}", file=sys.stderr)
            return 1
        return 0
    if args.verify_evidence is not None:
        try:
            require(
                args.repetition_one is None
                and args.repetition_two is None
                and args.out is None,
                "--verify-evidence cannot be combined with repetition inputs or --out",
            )
            verify_committed_evidence(args.verify_evidence)
        except (KeyError, TypeError, ValueError, OSError, json.JSONDecodeError) as error:
            print(f"quality evidence validation failed: {error}", file=sys.stderr)
            return 1
        return 0
    if args.repetition_one is None or args.repetition_two is None:
        parser.error("repetition_one and repetition_two are required")
    paths = [args.repetition_one, args.repetition_two]
    try:
        combined = validate_and_combine(load(paths[0]), load(paths[1]), paths)
        rendered = json.dumps(combined, ensure_ascii=False, indent=2) + "\n"
        if args.out:
            require(not args.out.exists(), f"refusing to overwrite {args.out}")
            args.out.parent.mkdir(parents=True, exist_ok=True)
            args.out.write_text(rendered, encoding="utf-8")
        else:
            sys.stdout.write(rendered)
    except (KeyError, TypeError, ValueError, OSError, json.JSONDecodeError) as error:
        print(f"quality repeat validation failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
