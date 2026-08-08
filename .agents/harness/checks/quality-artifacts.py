#!/usr/bin/env python3
"""Protected oracle for the atomically admitted canonical quality bundles.

This checker is implemented independently from the product-side artifact
validator and never delegates verdict authority to it.  Each admitted bundle
atomically binds the producer, runtimes, model identities, schemas, validator,
and exact artifact commitments; a new run fails closed until this protected
allowlist is reviewed.  The archive stage validates only the evidence manifest,
R1/R2, their all-string inventory, and the committed synthetic fixtures.  The
final stage first completes its own combined/projection verdict, then executes
the admitted copy of the mutable validator in a resource-bounded temporary tree
as a non-authoritative subject-under-test (valid and corrupted bundle).  Its
exit status can fail this gate but can never make artifacts pass.

The staged B0 bundle intentionally precedes the C1 evaluator/repeat-validator
source commit.  Therefore archive-stage can byte-bind the two fixed fixture
copies, while evaluatorFileSha256 and repeatValidatorFileSha256 are bound
across evidence, clean report snapshots, combined, and projection; C1's Rust
replay is the oracle that binds those two digests to the subsequently committed
source.  Hashing the pre-C1 checkout's mutable source files here would make the
designed B0 stage impossible rather than safer.
"""

from __future__ import annotations

import argparse
import base64
import copy
from dataclasses import dataclass
import hashlib
import json
import math
import os
from pathlib import Path
import re
import resource
import struct
import subprocess
import sys
import tempfile
from typing import Any, Mapping, Protocol, Sequence
import zlib


REPO_ROOT = Path(__file__).resolve().parents[3]

EVIDENCE_PATH = "eval/results/2026-08-05-qwen-vs-gpt-sol-evidence.json"
R1_ARCHIVE_PATH = (
    "eval/results/2026-08-05-qwen-vs-gpt-sol-final-r1-verified.json.gz"
)
R2_ARCHIVE_PATH = (
    "eval/results/2026-08-05-qwen-vs-gpt-sol-final-r2-verified.json.gz"
)
R1_LOGICAL_PATH = (
    "eval/results/2026-08-05-qwen-vs-gpt-sol-final-r1-verified.json"
)
R2_LOGICAL_PATH = (
    "eval/results/2026-08-05-qwen-vs-gpt-sol-final-r2-verified.json"
)
INVENTORY_PATH = (
    "eval/results/"
    "2026-08-05-qwen-vs-gpt-sol-final-content-inventory-verified.json.gz"
)
COMBINED_PATH = (
    "eval/results/2026-08-05-qwen-vs-gpt-sol-final-combined-verified.json"
)
PROJECTION_PATH = (
    "eval/results/2026-08-05-qwen-vs-gpt-sol-final-review-projection.json"
)
FIXTURE_SNAPSHOT_PATH = "eval/results/2026-08-05-local-cloud-quality-fixture.json"
FIXTURE_SOURCE_PATH = "src-tauri/src/eval/fixtures/local-cloud-quality.json"
RETRIEVAL_FIXTURE_PATH = "src-tauri/src/eval/fixtures/rag-bakeoff-synthetic.json"
RETRIEVAL_CORPUS_PATH = "src-tauri/src/eval/corpus.rs"
PRODUCT_VALIDATOR_PATH = "eval/results/verify_local_cloud_quality_artifacts.py"

EXPECTED_REPETITION_PATHS = {
    "1": {
        "archivePath": R1_ARCHIVE_PATH,
        "logicalPath": R1_LOGICAL_PATH,
    },
    "2": {
        "archivePath": R2_ARCHIVE_PATH,
        "logicalPath": R2_LOGICAL_PATH,
    },
}

ARM_4B = "qwen3-4b-instruct-2507-q4-k-m"
ARM_17B = "qwen3-1.7b-q4-k-m"
ARM_SOL = "gpt-5.6-sol-requested-high"
ARM_ORDER = {
    "1": [ARM_4B, ARM_17B, ARM_SOL],
    "2": [ARM_SOL, ARM_17B, ARM_4B],
}

# These are fixed synthetic inputs, not commitments to any candidate output.
GENERATION_CASES = {
    "ask-vault-en-quartz-holdout": ("en", "ask_vault", True),
    "ask-vault-pl-orchid": ("pl", "ask_vault", False),
    "fact-extract-en-helix": ("en", "light_extraction", False),
    "fact-extract-pl-zuraw": ("pl", "light_extraction", False),
    "live-bullets-pl-polaris": ("pl", "live_bullets", False),
    "live-current-en-nimbus": ("en", "live_current", False),
    "live-current-pl-ember-holdout": ("pl", "live_current", True),
    "meeting-chat-en-fjord-holdout": ("en", "meeting_chat", True),
    "meeting-chat-pl-delta": ("pl", "meeting_chat", False),
    "note-popup-actions-en-holdout": ("en", "note_assist", True),
    "note-popup-actions-pl": ("pl", "note_assist", False),
    "note-popup-decisions-en": ("en", "note_assist", False),
    "note-popup-fact-check-pl": ("pl", "note_assist", False),
    "note-popup-refine-pl": ("pl", "note_assist", False),
    "note-popup-shorten-en": ("en", "note_assist", False),
    "summary-en-cedar": ("en", "summary", False),
    "summary-pl-kestrel": ("pl", "summary", False),
    "summary-pl-lumen-holdout": ("pl", "summary", True),
}
RETRIEVAL_LANGUAGES = {
    "retrieval-01": "pl",
    "retrieval-02": "pl",
    "retrieval-03": "pl",
    "retrieval-04": "en",
    "retrieval-05": "pl",
    "retrieval-06": "pl",
    "retrieval-07": "pl",
    "retrieval-08": "en",
    "retrieval-09": "pl",
    "retrieval-10": "pl",
    "retrieval-11": "en",
    "retrieval-12": "en",
    "retrieval-13": "pl",
    "retrieval-14": "pl",
    "retrieval-15": "pl",
    "retrieval-16": "en",
    "retrieval-17": "pl",
    "retrieval-18": "en",
    "retrieval-19": "pl",
    "retrieval-20": "en",
}
CASE_LANGUAGES = {
    **{case_id: values[0] for case_id, values in GENERATION_CASES.items()},
    **RETRIEVAL_LANGUAGES,
}
ALL_CASE_IDS = frozenset(CASE_LANGUAGES)
COMBINED_CASE_IDS = ALL_CASE_IDS - {"live-bullets-pl-polaris"}

MAX_ARCHIVE_BYTES = 2 * 1024 * 1024
MAX_LOGICAL_BYTES = 8 * 1024 * 1024
MAX_SMALL_JSON_BYTES = 2 * 1024 * 1024
MAX_SUBJECT_OUTPUT_BYTES = 256 * 1024
DETERMINISTIC_GZIP_HEADER = b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x02\x03"
EMPTY_SHA256 = hashlib.sha256(b"").hexdigest()

# These two bundle variants are atomic.  A producer, runtime, report schema,
# artifact commitment, and validator from different rows must never compose.
# A future rerun is admitted only through a protected control-plane review.
ADMITTED_BUNDLES: tuple[dict[str, Any], ...] = (
    {
        "id": "committed-current",
        "producer": {
            "repositoryCommit": "e139cbeefed98fbb3c1da20c74da6a9d4c2dd3e6",
            "sourceFingerprintSha256": "e88bd005e9b45f714c10e289d8b7d977c113addb5c1038e823b0d86fd53fc538",
            "manifestSha256": "21ea3cc236b8c4058f18043b538bac87e93e933c86bd8b0c3696f5b67d45f01d",
            "evaluatorFileSha256": "b0feaaff5cb533fa2767a60ecd70844d3af3ee2f79be2b09b401eebf7b5f9363",
            "fixtureFileSha256": "b5f63efbc135a8629366614444bdba8d9501e28209d054e967b8e9debeddd9b2",
            "repeatValidatorFileSha256": "48d66559c713cc0da346d5d92fc0051be4b67ebb1b6b6f4422e3024101312198",
            "trackedDiffSha256": EMPTY_SHA256,
            "workingTreeDirty": False,
        },
        "runtimeIdentities": {
            "local": {
                "version": "murmur-brain-workspace-build",
                "sha256": "a47b8ec18d8597f59caad79aedf9aa9c7d86a5e2444ff036785ddea4fd2d4c37",
            },
            "codex": {
                "version": "codex-cli 0.146.0",
                "sha256": "ae1d3ffe6d48aec6a4dc3f50e7eb8e0d11962485a6a9406c5a7012139383da02",
            },
        },
        "evidenceSha256": "ed4b4ccbce0e76d42681e33b05f431a5306409abb348b9cb19d582c9c388cd2b",
        "repetitions": {
            "1": {
                "archivePath": R1_ARCHIVE_PATH,
                "archiveSha256": "8fa8e7c4fbc02c0f64ce3d149b8ffa2881e65d5aa8b3c307fd55df72d7955cbb",
                "logicalPath": R1_LOGICAL_PATH,
                "logicalSha256": "5b469273bd07cdb7dbc6cba14812a2dcbc1f068aa795e29d732f9237c57cb913",
            },
            "2": {
                "archivePath": R2_ARCHIVE_PATH,
                "archiveSha256": "b622d9ddbe3f7d8a4cbb191f9b3009185024768d48cecad564fb84561db5ba07",
                "logicalPath": R2_LOGICAL_PATH,
                "logicalSha256": "b83faa807cf093421a51021769ef638cc96ea2a8323794838bdf09f6b05307e9",
            },
        },
        "inventorySha256": "69ba4e3d507ca2de74fb26afdbbfdf9c1810d738ffa18d6b2ece71a7d8c6ab0c",
        "combined": {
            "path": COMBINED_PATH,
            "sha256": "8f9070220eef6e09ab0cb44cfb94ae4a6b077d18f1d39707c043918ff07d24f7",
        },
        "projectionSha256": "4d01dbaad9b489896742551112ed7d843987f92578fe905ceb7ceb50190a1136",
        "productValidatorSha256": "e0d9dbbab62cbdb348c954d30bf804fa444cbe4d745e7a7cc9233820f92cd947",
        "reportSchemaSha256": {
            "1": "263283c5f27f1edc99ed199c291fa12b93e75eccd30a7fa5f4849e0ccad56047",
            "2": "4a38645c75a77671a7f4d1c2b4557ba8312ca529b19b548a3ab6b45ff6d808f5",
        },
        "combinedSchemaSha256": "8e38f627ea5fb2a38d1accb354415c2d9d0e3f20828e66cbafb7bece5493580b",
    },
    {
        "id": "bootstrap-b0",
        "producer": {
            "repositoryCommit": "d672583e3181a33631b6930b28feef0a2fdacf2f",
            "sourceFingerprintSha256": "3201e12357f49442a259e131ba27192316def7cf7980c79f8006258bbfdad442",
            "manifestSha256": "21ea3cc236b8c4058f18043b538bac87e93e933c86bd8b0c3696f5b67d45f01d",
            "evaluatorFileSha256": "41e828e449382fb9df672b20ebd833d962b68aa3455e671477ff936bd20a89f9",
            "fixtureFileSha256": "b5f63efbc135a8629366614444bdba8d9501e28209d054e967b8e9debeddd9b2",
            "repeatValidatorFileSha256": "2ebe73a29054f68a3c93a3682778f0ff32ec2d40e3ce8c6a0e156be9379a1431",
            "trackedDiffSha256": EMPTY_SHA256,
            "workingTreeDirty": False,
        },
        "runtimeIdentities": {
            "local": {
                "version": "murmur-brain-workspace-build",
                "sha256": "1fa8425a068784b4659bafe48fed6cb7b737902dfeb85e04de4a2b220f0758ab",
            },
            "codex": {
                "version": "codex-cli 0.146.0",
                "sha256": "ae1d3ffe6d48aec6a4dc3f50e7eb8e0d11962485a6a9406c5a7012139383da02",
            },
        },
        "evidenceSha256": "68a933943a9bb942e276ec2d3eb1e5a9b916f1074b1bc9100ea1bd3e85d41fe4",
        "repetitions": {
            "1": {
                "archivePath": R1_ARCHIVE_PATH,
                "archiveSha256": "c044a7f5cde805e9c949df45aac180d91840579a73f19e13aec2aa7f91164c3f",
                "logicalPath": R1_LOGICAL_PATH,
                "logicalSha256": "beee7f543eb38e9283dcbea27ed16946e011d4b41e9e453120645679f22c1488",
            },
            "2": {
                "archivePath": R2_ARCHIVE_PATH,
                "archiveSha256": "640d3a775ff09e6efae88ab0bac727cddb82b8b1c25a334b60e474b468aa755b",
                "logicalPath": R2_LOGICAL_PATH,
                "logicalSha256": "2344f13087c36ff059116f2d55e448502b50056e45d0bd65eaed84807ff959b0",
            },
        },
        "inventorySha256": "54027418a91b6c47c53e357724cf424b13aa344e969032aaa824e786341c40fc",
        "combined": {
            "path": COMBINED_PATH,
            "sha256": "420d3cbb7495aba364e5d45c34261a7bd0d76f36e75fbdf37ac77ff126c69749",
        },
        "projectionSha256": "add2b40cabbc76fd87b67f623cf614f60cc2d93e77090aad55ea50899170caac",
        "productValidatorSha256": "a4f8ab66b461e36e522dcef1ec4cbc8b4a9e90602b2034ad33d9b09917a2b8c4",
        "reportSchemaSha256": {
            "1": "ffe45c3b9bcadb568f0d57b5321995270a11d8546709061eae92af29c350b6c7",
            "2": "c61f33c9c8d9eee6ef91efd943d8b9f9ee332dcf521e4af5af903b397bf7ae36",
        },
        "combinedSchemaSha256": "8e38f627ea5fb2a38d1accb354415c2d9d0e3f20828e66cbafb7bece5493580b",
    },
)

EXPECTED_MODEL_IDENTITIES = {
    ARM_4B: {
        "modelRequested": "Qwen_Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
        "modelFilename": "Qwen_Qwen3-4B-Instruct-2507-Q4_K_M.gguf",
        "modelBytes": 2497280736,
        "modelSha256": "2fde00ce69dd4899c70d020845e2638353015bba0fdf161b3eb965f2bca4464e",
        "modelClass": "heavy",
    },
    ARM_17B: {
        "modelRequested": "Qwen_Qwen3-1.7B-Q4_K_M.gguf",
        "modelFilename": "Qwen_Qwen3-1.7B-Q4_K_M.gguf",
        "modelBytes": 1282439584,
        "modelSha256": "72c5c3cb38fa32d5256e2fe30d03e7a64c6c79e668ad84057e3bd66e250b24fb",
        "modelClass": "light",
    },
}

EVIDENCE_KEYS = {
    "schemaVersion",
    "kind",
    "evidenceMethod",
    "repetitions",
    "combined",
    "producerSnapshot",
    "runtimeIdentities",
}
PRODUCER_KEYS = {
    "repositoryCommit",
    "sourceFingerprintSha256",
    "manifestSha256",
    "evaluatorFileSha256",
    "fixtureFileSha256",
    "repeatValidatorFileSha256",
    "trackedDiffSha256",
    "workingTreeDirty",
}
REPORT_KEYS = {
    "arms",
    "benchmarkDesign",
    "egressLedger",
    "environment",
    "evidenceLimits",
    "evidenceScope",
    "generatedAt",
    "holdoutInterpretation",
    "localComposite",
    "manifestSha256",
    "pairedComparison",
    "promptVersion",
    "repositoryCommit",
    "retrievalLane",
    "retrievalQuality",
    "runLabel",
    "sameCallerEnvelopeModelStack",
    "schemaVersion",
    "snapshotEnd",
    "snapshotStart",
    "sourceFingerprintSha256",
    "syntheticOnly",
}
ARM_METADATA_KEYS = {
    "armId",
    "effort",
    "effortEffectiveAttested",
    "effortTransport",
    "modelBytes",
    "modelClass",
    "modelFilename",
    "modelRequested",
    "modelSha256",
    "runtimeSha256",
    "runtimeVersion",
    "sidecarHardCapSecs",
    "sidecarIdleSecs",
    "sidecarReadySecs",
}
COMBINED_KEYS = {
    "arms",
    "comparisonType",
    "design",
    "dimensionAttribution",
    "holdoutInterpretation",
    "inputResultSha256ByRepetition",
    "localComposite",
    "manifestSha256",
    "measurementFileSha256",
    "paired",
    "rawAggregatePolicy",
    "repeatFilesByRepetition",
    "repositoryCommit",
    "retrievalQuality",
    "sameCallerEnvelopeModelStack",
    "schemaVersion",
    "sourceFingerprintSha256",
}
PROJECTION_KEYS = {
    "artifactBindings",
    "artifactBindingsCanonicalSha256",
    "contentInventory",
    "contentPolicy",
    "evidenceMethod",
    "kind",
    "measurementIntegrity",
    "producerSnapshot",
    "productRoute",
    "repetitions",
    "retrievalQuality",
    "runtimeIdentities",
    "sameCallerEnvelopeModelStack",
    "schemaVersion",
    "scope",
    "syntheticOnly",
}
PROJECTION_REPETITION_KEYS = {
    "armOrder",
    "arms",
    "cleanAndUnchanged",
    "egress",
    "environment",
    "generatedAt",
    "repetition",
    "runLabel",
    "snapshotEnd",
    "snapshotStart",
    "sourceBindingsMatchProducer",
}

PRODUCT_CASE_KEYS = {
    "branchConverged",
    "caseId",
    "casePayloadSha256",
    "caseRecordSha256",
    "comparisonScope",
    "dimensions",
    "durationMs",
    "egressReceiptCount",
    "egressReceiptEndOrdinal",
    "egressReceiptSha256",
    "egressReceiptStartOrdinal",
    "error",
    "generationProfile",
    "holdout",
    "language",
    "modelClass",
    "output",
    "outputChars",
    "outputSha256",
    "productRoute",
    "provenance",
    "provenanceSha256",
    "rawModelFormatPass",
    "rawModelOutput",
    "rawModelOutputSha256",
    "routeInputChars",
    "routeInputSha256",
    "score",
    "stateApplicationPass",
    "structuredEnvelopePass",
    "structuredLabelsPass",
    "structuredSchemaPass",
    "surface",
    "surfaceOutput",
    "surfaceOutputSha256",
    "toolPolicyPass",
    "toolPolicyScore",
    "toolSteps",
    "toolStepsSha256",
}
SAME_CALLER_CASE_KEYS = {
    "armId",
    "callCount",
    "caseId",
    "casePayloadSha256",
    "caseRecordSha256",
    "durationMs",
    "egressReceiptCount",
    "egressReceiptEndOrdinal",
    "egressReceiptSha256",
    "egressReceiptStartOrdinal",
    "envelopeSha256",
    "error",
    "holdout",
    "language",
    "modelClass",
    "modelRequested",
    "opaqueSubstitutionCount",
    "opaqueSubstitutionsSha256",
    "output",
    "outputChars",
    "outputContract",
    "outputSha256",
    "projection",
    "provenance",
    "provenanceSha256",
    "rawOutputChars",
    "rawOutputSha256",
    "redactionsCard",
    "redactionsEmail",
    "redactionsName",
    "redactionsPhone",
    "score",
    "stateApplicationPass",
    "surface",
    "systemBytes",
    "systemChars",
    "systemSha256",
    "userBytes",
    "userChars",
    "userSha256",
}
SCORE_KEYS = {
    "branchConvergencePass",
    "casePass",
    "closedWorldPass",
    "constraintPass",
    "criticalErrors",
    "criticalFailure",
    "diagnosticScore",
    "forbiddenPass",
    "formatPass",
    "languagePass",
    "provenancePass",
    "relationPass",
    "requiredGroupsHit",
    "requiredGroupsTotal",
    "sectionPass",
    "stateApplicationPass",
    "structuredLabelsPass",
    "toolPolicyPass",
}
DIMENSION_KEYS = {
    "finalProductOutputContract",
    "retrievalQuality",
    "toolAgentExecution",
}
RETRIEVAL_CASE_KEYS = {
    "caseId",
    "expectedIdHashes",
    "expectedMeetings",
    "language",
    "metrics",
    "queryPayloadSha256",
    "rankings",
}
RETRIEVAL_METHODS = {
    "fts_product",
    "hybrid_product",
    "semantic_product_floor",
}
RETRIEVAL_METRIC_KEYS = {"ndcgAtK", "recallAtK", "reciprocalRank"}

EMAIL_RE = re.compile(r"(?i)\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b")
AWS_ACCESS_KEY_RE = re.compile(r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b")
JWT_RE = re.compile(
    r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b"
)
PHONE_RES = (
    re.compile(r"(?<!\w)\+\d{1,3}(?:[ .()-]*\d){7,14}(?!\d)"),
    re.compile(r"(?<!\d)\(\d{3}\)[ .-]*\d{3}[ .-]*\d{4}(?!\d)"),
    re.compile(r"(?<!\d)\d{3}[ .-]\d{3}[ .-]\d{4}(?!\d)"),
)
CARD_RE = re.compile(r"(?<!\d)(?:\d[ -]?){12,18}\d(?!\d)")
BASE64_RE = re.compile(r"^[A-Za-z0-9+/]+={0,2}$")
BINARY_MAGIC = (
    b"RIFF",
    b"FORM",
    b"fLaC",
    b"OggS",
    b"ID3",
    b"\xff\xfb",
    b"\xff\xf3",
    b"\xff\xf2",
    b"\x89PNG\r\n\x1a\n",
    b"\xff\xd8\xff",
    b"PK\x03\x04",
    b"\x1f\x8b\x08",
    b"\xfd7zXZ\x00",
)
FORBIDDEN_TEXT_MARKERS = (
    "/users/",
    "/home/",
    "/private/var/",
    "/tmp/",
    "file://",
    "authorization:",
    "bearer ",
    "api_key",
    "api-key",
    "-----begin ",
    "id_rsa",
    "id_ed25519",
)
SENSITIVE_KEYS = {
    "authorization",
    "apikey",
    "accesstoken",
    "refreshtoken",
    "clientsecret",
    "password",
    "passwd",
    "credential",
    "credentials",
    "privatekey",
    "audiopath",
    "audiofile",
    "audiodata",
    "audiobytes",
    "audioblob",
    "recordingpath",
    "cardnumber",
    "paymentcard",
}
PROJECTION_FORBIDDEN_KEYS = {
    "criticalErrors",
    "output",
    "projection",
    "provenance",
    "rawModelOutput",
    "rawOutput",
    "rows",
    "surfaceOutput",
    "toolSteps",
    "uniqueStrings",
}


class ArtifactError(RuntimeError):
    """A content-free validation error."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ArtifactError(message)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def is_sha256(value: Any) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 64
        and all(character in "0123456789abcdef" for character in value)
        and len(set(value)) > 1
    )


def is_git_sha(value: Any) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 40
        and all(character in "0123456789abcdef" for character in value)
        and len(set(value)) > 1
    )


def canonical_sha256(value: Any) -> str:
    encoded = json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
    return sha256(encoded)


def framed_hash(parts: Sequence[str]) -> str:
    digest = hashlib.sha256()
    for part in parts:
        encoded = part.encode("utf-8")
        digest.update(len(encoded).to_bytes(8, "little"))
        digest.update(encoded)
    return digest.hexdigest()


def _reject_duplicate_keys(pairs: Sequence[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ArtifactError("duplicate JSON key")
        result[key] = value
    return result


def _reject_constant(_: str) -> None:
    raise ArtifactError("non-finite JSON number")


def load_json(data: bytes, label: str) -> Any:
    try:
        value = json.loads(
            data.decode("utf-8"),
            object_pairs_hook=_reject_duplicate_keys,
            parse_constant=_reject_constant,
        )
    except (UnicodeDecodeError, json.JSONDecodeError, ArtifactError) as exc:
        raise ArtifactError(f"{label}: invalid strict UTF-8 JSON") from exc
    for _, child in walk_values(value):
        if isinstance(child, float):
            require(math.isfinite(child), f"{label}: non-finite number")
    return value


def load_object(data: bytes, label: str) -> dict[str, Any]:
    value = load_json(data, label)
    require(isinstance(value, dict), f"{label}: root must be an object")
    return value


class Reader(Protocol):
    def read(self, relative: str, limit: int = MAX_SMALL_JSON_BYTES) -> bytes:
        ...


class DiskReader:
    def __init__(self) -> None:
        self.cache: dict[str, bytes] = {}

    def read(self, relative: str, limit: int = MAX_SMALL_JSON_BYTES) -> bytes:
        require(relative and not relative.startswith("/"), "artifact path is not relative")
        parts = Path(relative).parts
        require(".." not in parts, "artifact path traversal forbidden")
        path = REPO_ROOT.joinpath(*parts)
        require(not path.is_symlink(), f"{relative}: symlink forbidden")
        try:
            data = path.read_bytes()
        except OSError as exc:
            raise ArtifactError(f"{relative}: missing or unreadable") from exc
        require(len(data) <= limit, f"{relative}: file exceeds size limit")
        self.cache[relative] = data
        return data


class MemoryReader:
    def __init__(self, files: Mapping[str, bytes]) -> None:
        self.files = dict(files)
        self.reads: list[str] = []

    def read(self, relative: str, limit: int = MAX_SMALL_JSON_BYTES) -> bytes:
        self.reads.append(relative)
        try:
            data = self.files[relative]
        except KeyError as exc:
            raise ArtifactError(f"{relative}: missing or unreadable") from exc
        require(len(data) <= limit, f"{relative}: file exceeds size limit")
        return data


def walk_values(value: Any, path: tuple[str, ...] = ()):
    if isinstance(value, dict):
        for key, child in value.items():
            yield from walk_values(child, (*path, key))
    elif isinstance(value, list):
        for index, child in enumerate(value):
            yield from walk_values(child, (*path, str(index)))
    else:
        yield path, value


def walk_objects(value: Any):
    if isinstance(value, dict):
        yield value
        for child in value.values():
            yield from walk_objects(child)
    elif isinstance(value, list):
        for child in value:
            yield from walk_objects(child)


def walk_keys(value: Any):
    if isinstance(value, dict):
        for key, child in value.items():
            yield key
            yield from walk_keys(child)
    elif isinstance(value, list):
        for child in value:
            yield from walk_keys(child)


def text_inventory(value: Any) -> list[list[str]]:
    return [
        ["/".join(path), child]
        for path, child in walk_values(value)
        if isinstance(child, str)
    ]


def schema_inventory(value: Any, path: tuple[str, ...] = ()) -> list[list[Any]]:
    """Path-aware closed schema, including array lengths and JSON leaf types."""

    entries: list[list[Any]] = []
    rendered_path = "/".join(path)
    if isinstance(value, dict):
        entries.append([rendered_path, "object", sorted(value)])
        for key in sorted(value):
            entries.extend(schema_inventory(value[key], (*path, key)))
    elif isinstance(value, list):
        entries.append([rendered_path, "array", len(value)])
        for index, child in enumerate(value):
            entries.extend(schema_inventory(child, (*path, str(index))))
    elif value is None:
        entries.append([rendered_path, "null"])
    elif isinstance(value, bool):
        entries.append([rendered_path, "boolean"])
    elif isinstance(value, int):
        entries.append([rendered_path, "integer"])
    elif isinstance(value, float):
        entries.append([rendered_path, "number"])
    elif isinstance(value, str):
        entries.append([rendered_path, "string"])
    else:
        raise ArtifactError("closed schema: unsupported JSON value")
    return entries


def _luhn_valid(digits: str) -> bool:
    total = 0
    parity = len(digits) % 2
    for index, character in enumerate(digits):
        number = int(character)
        if index % 2 == parity:
            number *= 2
            if number > 9:
                number -= 9
        total += number
    return total % 10 == 0


def _contains_card(value: str) -> bool:
    # Content-addresses are ubiquitous in these artifacts.  A run of decimal
    # nibbles inside a complete SHA can accidentally satisfy Luhn; the digest is
    # already type- and length-validated at every binding site and is not PII.
    if is_sha256(value) or is_git_sha(value):
        return False
    for match in CARD_RE.finditer(value):
        digits = "".join(character for character in match.group(0) if character.isdigit())
        if 13 <= len(digits) <= 19 and _luhn_valid(digits):
            return True
    return False


def _contains_encoded_binary(value: str) -> bool:
    candidate = "".join(value.split())
    lowered = candidate.lower()
    if lowered.startswith(("data:audio/", "data:application/octet-stream")):
        return True
    if "audio/" in lowered and ";base64," in lowered:
        return True
    if len(candidate) < 8 or len(candidate) % 4 != 0:
        return False
    if BASE64_RE.fullmatch(candidate) is None:
        return False
    if is_sha256(candidate) or is_git_sha(candidate):
        return False
    try:
        decoded = base64.b64decode(candidate, validate=True)
    except (ValueError, UnicodeEncodeError):
        return False
    if any(decoded.startswith(magic) for magic in BINARY_MAGIC):
        return True
    if len(decoded) < 64:
        return False
    non_text = sum(
        byte not in b"\t\n\r" and not 32 <= byte <= 126
        for byte in decoded
    )
    return non_text / len(decoded) > 0.25


def _scan_text(child: str, label: str) -> None:
    lowered = child.lower()
    require(
        not any(marker in lowered for marker in FORBIDDEN_TEXT_MARKERS),
        f"{label}: private path or credential marker forbidden",
    )
    require(EMAIL_RE.search(child) is None, f"{label}: email address forbidden")
    require(
        AWS_ACCESS_KEY_RE.search(child) is None,
        f"{label}: AWS access key forbidden",
    )
    require(JWT_RE.search(child) is None, f"{label}: JWT-like token forbidden")
    require(
        all(pattern.search(child) is None for pattern in PHONE_RES),
        f"{label}: phone number forbidden",
    )
    require(not _contains_card(child), f"{label}: payment card number forbidden")
    require(
        not _contains_encoded_binary(child),
        f"{label}: encoded binary or audio payload forbidden",
    )


def scan_privacy(value: Any, label: str) -> None:
    # JSON object keys are decoded strings too, so they receive the same PII
    # and payload scan as string leaves in addition to the sensitive-name gate.
    for key in walk_keys(value):
        normalized = re.sub(r"[^a-z0-9]", "", key.lower())
        require(normalized not in SENSITIVE_KEYS, f"{label}: sensitive field forbidden")
        _scan_text(key, label)
    for _, child in walk_values(value):
        if isinstance(child, str):
            _scan_text(child, label)


def decode_gzip(archive: bytes, label: str) -> bytes:
    require(len(archive) <= MAX_ARCHIVE_BYTES, f"{label}: archive exceeds size limit")
    require(len(archive) >= 18, f"{label}: truncated gzip")
    require(
        archive[:10] == DETERMINISTIC_GZIP_HEADER,
        f"{label}: gzip metadata is not deterministic",
    )
    decompressor = zlib.decompressobj(16 + zlib.MAX_WBITS)
    try:
        logical = decompressor.decompress(archive, MAX_LOGICAL_BYTES + 1)
        require(len(logical) <= MAX_LOGICAL_BYTES, f"{label}: logical data exceeds limit")
        logical += decompressor.flush(MAX_LOGICAL_BYTES - len(logical) + 1)
    except zlib.error as exc:
        raise ArtifactError(f"{label}: invalid gzip stream") from exc
    require(len(logical) <= MAX_LOGICAL_BYTES, f"{label}: logical data exceeds limit")
    require(decompressor.eof, f"{label}: incomplete gzip member")
    require(not decompressor.unused_data, f"{label}: trailing or concatenated gzip data")
    require(not decompressor.unconsumed_tail, f"{label}: unconsumed gzip data")
    return logical


def deterministic_gzip(logical: bytes, *, mtime: int = 0) -> bytes:
    compressor = zlib.compressobj(level=9, wbits=-zlib.MAX_WBITS)
    payload = compressor.compress(logical) + compressor.flush()
    header = b"\x1f\x8b\x08\x00" + struct.pack("<I", mtime) + b"\x02\x03"
    trailer = struct.pack("<II", zlib.crc32(logical) & 0xFFFFFFFF, len(logical) & 0xFFFFFFFF)
    return header + payload + trailer


def _validate_producer(evidence: Mapping[str, Any]) -> dict[str, Any]:
    producer = evidence.get("producerSnapshot")
    require(isinstance(producer, dict), "evidence: producer snapshot missing")
    require(set(producer) == PRODUCER_KEYS, "evidence: producer snapshot schema differs")
    require(is_git_sha(producer.get("repositoryCommit")), "evidence: invalid producer commit")
    for field in PRODUCER_KEYS - {"repositoryCommit", "workingTreeDirty"}:
        require(is_sha256(producer.get(field)), f"evidence: invalid producer {field}")
    require(producer.get("workingTreeDirty") is False, "evidence: producer is dirty")
    require(
        producer.get("trackedDiffSha256") == EMPTY_SHA256,
        "evidence: producer tracked diff is not empty",
    )
    return producer


def _validate_evidence(value: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any]]:
    require(set(value) == EVIDENCE_KEYS, "evidence: root schema differs")
    require(value.get("schemaVersion") == 1, "evidence: schema version differs")
    require(
        value.get("kind") == "murmur_local_cloud_quality_evidence",
        "evidence: kind differs",
    )
    require(
        value.get("evidenceMethod") == "deterministic_code_owned_oracles_no_model_judge",
        "evidence: method differs",
    )
    repetitions = value.get("repetitions")
    require(
        isinstance(repetitions, dict) and set(repetitions) == {"1", "2"},
        "evidence: repetition inventory differs",
    )
    for repetition in ("1", "2"):
        entry = repetitions[repetition]
        require(isinstance(entry, dict), f"evidence: R{repetition} entry missing")
        require(
            set(entry) == {"archivePath", "archiveSha256", "logicalPath", "logicalSha256"},
            f"evidence: R{repetition} schema differs",
        )
        for field, expected in EXPECTED_REPETITION_PATHS[repetition].items():
            require(entry.get(field) == expected, f"evidence: R{repetition} {field} differs")
        require(is_sha256(entry.get("archiveSha256")), f"evidence: R{repetition} archive hash invalid")
        require(is_sha256(entry.get("logicalSha256")), f"evidence: R{repetition} logical hash invalid")
    combined = value.get("combined")
    require(
        isinstance(combined, dict) and set(combined) == {"path", "sha256"},
        "evidence: combined entry differs",
    )
    require(combined.get("path") == COMBINED_PATH, "evidence: combined path differs")
    require(is_sha256(combined.get("sha256")), "evidence: combined hash invalid")
    producer = _validate_producer(value)
    identities = value.get("runtimeIdentities")
    require(
        isinstance(identities, dict) and set(identities) == {"local", "codex"},
        "evidence: runtime identity inventory differs",
    )
    for name, identity in identities.items():
        require(
            isinstance(identity, dict) and set(identity) == {"version", "sha256"},
            f"evidence: {name} runtime identity schema differs",
        )
        require(
            isinstance(identity.get("version"), str) and identity["version"],
            f"evidence: {name} runtime version missing",
        )
        require(is_sha256(identity.get("sha256")), f"evidence: {name} runtime hash invalid")
    scan_privacy(value, "evidence")
    return producer, identities


def _resolve_admitted_bundle(
    producer: Mapping[str, Any],
    identities: Mapping[str, Any],
) -> dict[str, Any]:
    matches = [
        bundle
        for bundle in ADMITTED_BUNDLES
        if producer == bundle["producer"]
        and identities == bundle["runtimeIdentities"]
    ]
    require(
        len(matches) == 1,
        "evidence: producer/runtime combination is not atomically admitted",
    )
    return matches[0]


def _generation_fixture_commitments(
    reader: Reader,
    producer: Mapping[str, Any],
) -> tuple[dict[str, str], bytes]:
    snapshot_bytes = reader.read(FIXTURE_SNAPSHOT_PATH)
    source_bytes = reader.read(FIXTURE_SOURCE_PATH)
    require(snapshot_bytes == source_bytes, "generation fixture: source snapshot differs")
    require(
        sha256(snapshot_bytes) == producer.get("fixtureFileSha256"),
        "generation fixture: producer hash differs",
    )
    fixture = load_object(snapshot_bytes, "generation fixture")
    require(
        set(fixture) == {"schemaVersion", "syntheticOnly", "cases"},
        "generation fixture: root schema differs",
    )
    require(fixture.get("schemaVersion") == 9, "generation fixture: schema differs")
    require(fixture.get("syntheticOnly") is True, "generation fixture: not synthetic")
    cases = fixture.get("cases")
    require(isinstance(cases, list), "generation fixture: cases missing")
    empty_defaults: dict[str, Any] = {
        "transcript": "",
        "question": "",
        "dateIso": "",
        "titleHint": "",
        "vaultTitles": [],
        "labeled": False,
        "diarizedOthers": False,
        "durationS": 0,
        "action": "",
        "selection": "",
        "before": "",
        "previousBullets": "",
        "toolResult": "",
        "searchResult": "",
        "searchTerms": [],
        "floorCorpus": "",
        "syntheticRedactionEntities": [],
    }
    payload_fields = (
        "id",
        "surface",
        "language",
        "transcript",
        "question",
        "dateIso",
        "titleHint",
        "vaultTitles",
        "labeled",
        "diarizedOthers",
        "durationS",
        "action",
        "selection",
        "before",
        "previousBullets",
        "toolResult",
        "searchResult",
        "searchTerms",
        "floorCorpus",
        "syntheticRedactionEntities",
    )
    commitments: dict[str, str] = {}
    for case in cases:
        require(isinstance(case, dict), "generation fixture: case is not an object")
        case_id = case.get("id")
        require(case_id in GENERATION_CASES, "generation fixture: unknown case")
        require(case_id not in commitments, "generation fixture: duplicate case")
        language, surface, holdout = GENERATION_CASES[case_id]
        require(case.get("language") == language, "generation fixture: language differs")
        require(case.get("surface") == surface, "generation fixture: surface differs")
        require(case.get("holdout", False) is holdout, "generation fixture: holdout differs")
        payload = ["murmur-quality-case-payload-v2"] + [
            case.get(field, empty_defaults.get(field)) for field in payload_fields
        ]
        canonical = json.dumps(payload, ensure_ascii=False, separators=(",", ":"))
        commitments[case_id] = framed_hash([canonical])
    require(set(commitments) == set(GENERATION_CASES), "generation fixture: case inventory differs")
    scan_privacy(fixture, "generation fixture")
    return commitments, snapshot_bytes


def _retrieval_fixture_commitments(
    reader: Reader,
) -> tuple[dict[str, str], str, str]:
    fixture_bytes = reader.read(RETRIEVAL_FIXTURE_PATH)
    fixture = load_json(fixture_bytes, "retrieval fixture")
    require(isinstance(fixture, list), "retrieval fixture: root must be an array")
    require(len(fixture) == len(RETRIEVAL_LANGUAGES), "retrieval fixture: count differs")
    commitments: dict[str, str] = {}
    for index, query in enumerate(fixture, start=1):
        require(isinstance(query, dict), "retrieval fixture: query is not an object")
        require(
            set(query) == {"_comment", "query", "lang", "expected_meeting_ids"},
            "retrieval fixture: query schema differs",
        )
        case_id = f"retrieval-{index:02d}"
        language = query.get("lang")
        text = query.get("query")
        expected_ids = query.get("expected_meeting_ids")
        require(language == RETRIEVAL_LANGUAGES[case_id], "retrieval fixture: language differs")
        require(isinstance(text, str), "retrieval fixture: query text missing")
        require(
            isinstance(expected_ids, list)
            and expected_ids
            and all(isinstance(item, str) for item in expected_ids),
            "retrieval fixture: expected IDs differ",
        )
        commitments[case_id] = framed_hash(
            ["murmur-retrieval-case-payload-v2", language, text, *expected_ids]
        )
    require(set(commitments) == set(RETRIEVAL_LANGUAGES), "retrieval fixture: case inventory differs")
    scan_privacy(fixture, "retrieval fixture")
    try:
        fixture_text = fixture_bytes.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise ArtifactError("retrieval fixture: invalid UTF-8") from exc
    corpus_bytes = reader.read(RETRIEVAL_CORPUS_PATH)
    return commitments, framed_hash([fixture_text]), sha256(corpus_bytes)


def _validate_case_inventory(
    value: Any,
    label: str,
    generation_commitments: Mapping[str, str],
    retrieval_commitments: Mapping[str, str],
    expected_case_ids: frozenset[str],
) -> None:
    observed: set[str] = set()
    languages: set[str] = set()
    for record in walk_objects(value):
        language_value = record.get("language")
        if language_value is not None:
            require(language_value in {"en", "pl"}, f"{label}: unsupported language")
            languages.add(language_value)
        if "caseId" not in record:
            continue
        case_id = record.get("caseId")
        require(isinstance(case_id, str), f"{label}: case ID is not text")
        require(case_id in ALL_CASE_IDS, f"{label}: unknown case ID")
        observed.add(case_id)
        if language_value is not None:
            require(language_value == CASE_LANGUAGES[case_id], f"{label}: case language differs")
        if "casePayloadSha256" in record:
            require(case_id in generation_commitments, f"{label}: generation commitment on retrieval case")
            require(
                record.get("casePayloadSha256") == generation_commitments[case_id],
                f"{label}: generation fixture commitment differs",
            )
        if "queryPayloadSha256" in record:
            require(case_id in retrieval_commitments, f"{label}: retrieval commitment on generation case")
            require(
                record.get("queryPayloadSha256") == retrieval_commitments[case_id],
                f"{label}: retrieval fixture commitment differs",
            )
    require(observed == set(expected_case_ids), f"{label}: case ID inventory differs")
    require(languages == {"en", "pl"}, f"{label}: language inventory differs")


def _is_number(value: Any) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool)


def _validate_numeric_semantics(value: Any, label: str) -> None:
    """Reject impossible quality claims without pinning any observed score."""

    percentage_fields = {
        "callSuccessRate",
        "casePassRate",
        "coverageRate",
        "diagnosticScore",
        "diagnosticScoreMean",
        "localCallSuccessRate",
        "localCasePassRate",
        "localDiagnosticScore",
        "localSurfaceMacroPassRate",
        "passRate",
        "referenceCallSuccessRate",
        "referenceCasePassRate",
        "referenceDiagnosticScore",
        "referenceSurfaceMacroPassRate",
        "surfaceMacroPassRate",
        "toolPolicyMean",
        "toolPolicyScore",
    }
    unit_interval_fields = {
        "cosineFloor",
        "mrr",
        "ndcgAtK",
        "recallAtK",
        "reciprocalRank",
    }
    signed_percentage_fields = {
        "referenceMinusLocal",
        "referenceMinusLocalDiagnosticMean",
        "referenceMinusLocalMean",
    }

    def visit(child: Any, path: tuple[str, ...]) -> None:
        if isinstance(child, dict):
            for key, nested in child.items():
                visit(nested, (*path, key))
            return
        if isinstance(child, list):
            for index, nested in enumerate(child):
                visit(nested, (*path, str(index)))
            return
        if not _is_number(child):
            return
        key = path[-1] if path else ""
        if key in signed_percentage_fields:
            require(-100 <= child <= 100, f"{label}: signed quality value out of range")
            return
        require(child >= 0, f"{label}: negative count or measurement")
        if key in percentage_fields or "surfacePassRates" in path:
            require(child <= 100, f"{label}: percentage quality value out of range")
        if key in unit_interval_fields:
            require(child <= 1, f"{label}: normalized quality value out of range")

    visit(value, ())


def _validate_hash_fields(value: Any, label: str) -> None:
    """Fail closed on zero/placeholder commits and digest-shaped bindings."""

    if isinstance(value, dict):
        for key, child in value.items():
            if key == "repositoryCommit" and child is not None:
                require(is_git_sha(child), f"{label}: invalid repository commit")
            if key.lower().endswith("sha256") and child is not None:
                require(
                    isinstance(child, (dict, list)) or is_sha256(child),
                    f"{label}: invalid SHA-256 binding",
                )
            _validate_hash_fields(child, label)
    elif isinstance(value, list):
        for child in value:
            _validate_hash_fields(child, label)


def _validate_score(score: Any, label: str) -> None:
    require(isinstance(score, dict), f"{label}: score missing")
    require(set(score) == SCORE_KEYS, f"{label}: score schema differs")
    for key in SCORE_KEYS:
        value = score[key]
        if key.endswith("Pass") or key == "criticalFailure":
            require(isinstance(value, bool), f"{label}: {key} is not boolean")
    require(
        _is_number(score["diagnosticScore"])
        and 0 <= score["diagnosticScore"] <= 100,
        f"{label}: diagnostic score out of range",
    )
    require(
        isinstance(score["requiredGroupsHit"], int)
        and not isinstance(score["requiredGroupsHit"], bool)
        and isinstance(score["requiredGroupsTotal"], int)
        and not isinstance(score["requiredGroupsTotal"], bool)
        and 0 <= score["requiredGroupsHit"] <= score["requiredGroupsTotal"],
        f"{label}: required-group counts differ",
    )
    require(
        isinstance(score["criticalErrors"], list)
        and all(isinstance(item, str) for item in score["criticalErrors"]),
        f"{label}: critical error inventory differs",
    )


def _ordered_json_hash(value: Any) -> str:
    canonical = json.dumps(value, ensure_ascii=False, separators=(",", ":"))
    return framed_hash([canonical])


def _validate_score_relations(case: Mapping[str, Any], label: str) -> None:
    score = case["score"]
    errors = score["criticalErrors"]
    total = score["requiredGroupsTotal"]
    hit = score["requiredGroupsHit"]
    require(total > 0, f"{label}: required-group total is empty")
    require(
        score["criticalFailure"] is bool(errors),
        f"{label}: critical verdict differs from critical errors",
    )
    pass_fields = (
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
    )
    expected_pass = (
        case.get("error") is None
        and not errors
        and hit == total
        and all(score[field] for field in pass_fields)
    )
    require(
        score["casePass"] is expected_pass,
        f"{label}: case-pass verdict does not replay from score evidence",
    )
    raw = (
        hit / total * 50.0
        + (10.0 if score["formatPass"] else 0.0)
        + (10.0 if score["sectionPass"] else 0.0)
        + (10.0 if score["languagePass"] else 0.0)
        + (10.0 if score["forbiddenPass"] else 0.0)
        + (5.0 if score["constraintPass"] else 0.0)
        + (5.0 if score["provenancePass"] else 0.0)
    )
    uncapped = math.floor(raw * 10.0 + 0.5) / 10.0
    expected_diagnostic = min(uncapped, 49.0) if errors else uncapped
    require(
        math.isclose(
            float(score["diagnosticScore"]),
            expected_diagnostic,
            rel_tol=0.0,
            abs_tol=1e-9,
        ),
        f"{label}: diagnostic score does not replay from score evidence",
    )


def _expected_product_dimensions(
    case: Mapping[str, Any],
    arm_id: str,
    label: str,
) -> dict[str, str]:
    score = case["score"]
    retrieval = "not_measured" if case["surface"] == "ask_vault" else "not_applicable"
    is_agent_loop = arm_id == ARM_SOL and case["surface"] in {"ask_vault", "live_current"}
    if is_agent_loop:
        require(
            isinstance(case["branchConverged"], bool)
            and isinstance(case["toolPolicyPass"], bool),
            f"{label}: cloud loop receipts are missing",
        )
        tool_agent = (
            "pass"
            if case["branchConverged"] and case["toolPolicyPass"]
            else "fail"
        )
    else:
        require(
            case["branchConverged"] is None,
            f"{label}: branch receipt applies only to cloud Ask/Live loops",
        )
        tool_agent = "not_applicable"
    final_pass = (
        case["error"] is None
        and score["requiredGroupsHit"] == score["requiredGroupsTotal"]
        and all(
            score[field]
            for field in (
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


def _validate_case_receipt(
    case: Mapping[str, Any],
    arm_id: str,
    rows: list[Any],
    label: str,
) -> None:
    count = case["egressReceiptCount"]
    start = case["egressReceiptStartOrdinal"]
    end = case["egressReceiptEndOrdinal"]
    require(type(count) is int and count >= 0, f"{label}: receipt count differs")
    if arm_id != ARM_SOL:
        require(
            count == 0
            and start is None
            and end is None
            and case["egressReceiptSha256"] == _ordered_json_hash([]),
            f"{label}: local receipt is not empty",
        )
        return
    require(
        count > 0
        and type(start) is int
        and type(end) is int
        and start > 0
        and end >= start
        and end - start + 1 == count,
        f"{label}: cloud receipt range differs",
    )
    selected = [
        row
        for row in rows
        if isinstance(row, dict)
        and isinstance(row.get("ordinal"), int)
        and start <= row["ordinal"] <= end
    ]
    require(
        len(selected) == count
        and [row["ordinal"] for row in selected] == list(range(start, end + 1))
        and case["egressReceiptSha256"] == _ordered_json_hash(selected),
        f"{label}: cloud receipt commitment differs",
    )


def _product_case_record_values(case: Mapping[str, Any]) -> list[Any]:
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


def _same_caller_case_record_values(case: Mapping[str, Any]) -> list[Any]:
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


def _validate_product_case_bindings(
    case: Mapping[str, Any],
    arm_id: str,
    rows: list[Any],
    label: str,
) -> None:
    output = case["output"]
    require(isinstance(output, str), f"{label}: output must be text")
    require(
        case["outputChars"] == len(output)
        and case["outputSha256"] == framed_hash([output]),
        f"{label}: output length/hash differs from output",
    )
    for value_key, hash_key in (
        ("rawModelOutput", "rawModelOutputSha256"),
        ("surfaceOutput", "surfaceOutputSha256"),
    ):
        value = case[value_key]
        require(
            (value is None and case[hash_key] is None)
            or (isinstance(value, str) and case[hash_key] == framed_hash([value])),
            f"{label}: {hash_key} differs from {value_key}",
        )
    for value_key, hash_key in (
        ("provenance", "provenanceSha256"),
        ("toolSteps", "toolStepsSha256"),
    ):
        values = case[value_key]
        require(
            isinstance(values, list) and all(isinstance(value, str) for value in values),
            f"{label}: {value_key} is not a string array",
        )
        require(
            case[hash_key] == framed_hash(values),
            f"{label}: {hash_key} differs from {value_key}",
        )
    score = case["score"]
    require(
        score["toolPolicyPass"]
        == (case["toolPolicyPass"] if case["toolPolicyPass"] is not None else True)
        and score["stateApplicationPass"]
        == (
            case["stateApplicationPass"]
            if case["stateApplicationPass"] is not None
            else True
        )
        and score["branchConvergencePass"]
        == (case["branchConverged"] if case["branchConverged"] is not None else True),
        f"{label}: top-level verdict receipts differ from score",
    )
    if case["surface"] == "light_extraction":
        require(
            all(
                isinstance(case[field], bool)
                for field in (
                    "structuredSchemaPass",
                    "structuredLabelsPass",
                    "structuredEnvelopePass",
                    "rawModelFormatPass",
                )
            )
            and case["structuredSchemaPass"] == score["formatPass"]
            and case["structuredLabelsPass"] == score["structuredLabelsPass"]
            and case["structuredEnvelopePass"] == case["rawModelFormatPass"],
            f"{label}: structured-output evidence differs",
        )
    else:
        require(
            all(
                case[field] is None
                for field in (
                    "structuredSchemaPass",
                    "structuredLabelsPass",
                    "structuredEnvelopePass",
                )
            ),
            f"{label}: structured evidence applies only to extraction",
        )
    _validate_score_relations(case, label)
    require(
        case["dimensions"] == _expected_product_dimensions(case, arm_id, label),
        f"{label}: dimensions do not replay from case evidence",
    )
    _validate_case_receipt(case, arm_id, rows, label)
    require(
        case["caseRecordSha256"] == _ordered_json_hash(_product_case_record_values(case)),
        f"{label}: case-record commitment differs",
    )


def _validate_same_caller_case_bindings(
    case: Mapping[str, Any],
    arm_id: str,
    rows: list[Any],
    label: str,
) -> None:
    output = case["output"]
    provenance = case["provenance"]
    require(isinstance(output, str), f"{label}: output must be text")
    require(
        case["outputChars"] == len(output)
        and case["outputSha256"] == framed_hash([output]),
        f"{label}: output length/hash differs from output",
    )
    require(
        isinstance(provenance, list)
        and all(isinstance(value, str) for value in provenance)
        and case["provenanceSha256"] == framed_hash(provenance),
        f"{label}: provenance commitment differs",
    )
    require(
        case["score"]["stateApplicationPass"]
        == (
            case["stateApplicationPass"]
            if case["stateApplicationPass"] is not None
            else True
        ),
        f"{label}: state verdict differs from score",
    )
    _validate_score_relations(case, label)
    _validate_case_receipt(case, arm_id, rows, label)
    require(
        case["caseRecordSha256"]
        == _ordered_json_hash(_same_caller_case_record_values(case)),
        f"{label}: case-record commitment differs",
    )


def _validate_report_case_schemas(report: Mapping[str, Any], label: str) -> None:
    expected_by_arm = {
        ARM_4B: set(GENERATION_CASES)
        - {
            "live-bullets-pl-polaris",
            "live-current-en-nimbus",
            "live-current-pl-ember-holdout",
        },
        ARM_17B: {
            "live-bullets-pl-polaris",
            "live-current-en-nimbus",
            "live-current-pl-ember-holdout",
        },
        ARM_SOL: set(GENERATION_CASES),
    }
    ledger = report.get("egressLedger")
    require(isinstance(ledger, dict), f"{label}: egress ledger missing")
    rows = ledger.get("rows")
    require(isinstance(rows, list), f"{label}: egress rows missing")
    require(
        ledger.get("attemptedRows") == ledger.get("persistedRows") == len(rows)
        and ledger.get("persistenceFailures") == 0
        and ledger.get("contentFreeRowsSha256") == _ordered_json_hash(rows),
        f"{label}: egress ledger commitment differs",
    )
    arms = report.get("arms")
    require(isinstance(arms, list), f"{label}: product arms missing")
    for arm in arms:
        require(isinstance(arm, dict), f"{label}: product arm missing")
        metadata = arm.get("metadata")
        cases = arm.get("cases")
        require(isinstance(metadata, dict), f"{label}: product metadata missing")
        arm_id = metadata.get("armId")
        require(arm_id in expected_by_arm, f"{label}: product arm ID differs")
        require(isinstance(cases, list), f"{label}: product cases missing")
        observed: list[str] = []
        for case in cases:
            require(isinstance(case, dict), f"{label}: product case is not an object")
            require(set(case) == PRODUCT_CASE_KEYS, f"{label}: product case schema differs")
            case_id = case.get("caseId")
            require(isinstance(case_id, str), f"{label}: product case ID missing")
            observed.append(case_id)
            _validate_score(case.get("score"), f"{label}: product {arm_id}/{case_id}")
            dimensions = case.get("dimensions")
            require(isinstance(dimensions, dict), f"{label}: product dimensions missing")
            require(set(dimensions) == DIMENSION_KEYS, f"{label}: product dimensions schema differs")
            require(
                dimensions["finalProductOutputContract"] in {"pass", "fail"}
                and dimensions["retrievalQuality"] in {"not_measured", "not_applicable"}
                and dimensions["toolAgentExecution"]
                in {"pass", "fail", "not_applicable"},
                f"{label}: product dimension value differs",
            )
            _validate_product_case_bindings(
                case,
                arm_id,
                rows,
                f"{label}: product {arm_id}/{case_id}",
            )
        require(len(observed) == len(set(observed)), f"{label}: duplicate product case")
        require(set(observed) == expected_by_arm[arm_id], f"{label}: product arm case inventory differs")

    same_caller = report.get("sameCallerEnvelopeModelStack")
    require(isinstance(same_caller, dict), f"{label}: same-caller lane missing")
    same_arms = same_caller.get("arms")
    require(isinstance(same_arms, list), f"{label}: same-caller arms missing")
    for arm in same_arms:
        require(isinstance(arm, dict), f"{label}: same-caller arm missing")
        arm_id = arm.get("armId")
        cases = arm.get("cases")
        require(arm_id in expected_by_arm, f"{label}: same-caller arm ID differs")
        require(isinstance(cases, list), f"{label}: same-caller cases missing")
        observed = []
        for case in cases:
            require(isinstance(case, dict), f"{label}: same-caller case is not an object")
            require(
                set(case) == SAME_CALLER_CASE_KEYS,
                f"{label}: same-caller case schema differs",
            )
            case_id = case.get("caseId")
            require(isinstance(case_id, str), f"{label}: same-caller case ID missing")
            require(case.get("armId") == arm_id, f"{label}: same-caller case arm differs")
            observed.append(case_id)
            _validate_score(case.get("score"), f"{label}: same-caller {arm_id}/{case_id}")
            _validate_same_caller_case_bindings(
                case,
                arm_id,
                rows,
                f"{label}: same-caller {arm_id}/{case_id}",
            )
        require(len(observed) == len(set(observed)), f"{label}: duplicate same-caller case")
        require(set(observed) == expected_by_arm[arm_id], f"{label}: same-caller case inventory differs")

    retrieval = report.get("retrievalQuality")
    require(isinstance(retrieval, dict), f"{label}: retrieval lane missing")
    cases = retrieval.get("cases")
    require(isinstance(cases, list), f"{label}: retrieval cases missing")
    observed = []
    for case in cases:
        require(isinstance(case, dict), f"{label}: retrieval case is not an object")
        require(set(case) == RETRIEVAL_CASE_KEYS, f"{label}: retrieval case schema differs")
        case_id = case.get("caseId")
        require(isinstance(case_id, str), f"{label}: retrieval case ID missing")
        observed.append(case_id)
        expected_hashes = case.get("expectedIdHashes")
        require(
            isinstance(expected_hashes, list)
            and expected_hashes
            and all(is_sha256(item) for item in expected_hashes)
            and len(expected_hashes) == len(set(expected_hashes)),
            f"{label}: retrieval expected-ID hashes differ",
        )
        require(
            isinstance(case.get("expectedMeetings"), int)
            and not isinstance(case.get("expectedMeetings"), bool)
            and case["expectedMeetings"] == len(expected_hashes),
            f"{label}: retrieval expected-meeting count differs",
        )
        metrics = case.get("metrics")
        rankings = case.get("rankings")
        require(
            isinstance(metrics, dict) and set(metrics) == RETRIEVAL_METHODS,
            f"{label}: retrieval method inventory differs",
        )
        require(
            isinstance(rankings, dict) and set(rankings) == RETRIEVAL_METHODS,
            f"{label}: retrieval ranking inventory differs",
        )
        for method in RETRIEVAL_METHODS:
            values = metrics[method]
            require(
                isinstance(values, dict) and set(values) == RETRIEVAL_METRIC_KEYS,
                f"{label}: retrieval metric schema differs",
            )
            require(
                all(_is_number(metric) and 0 <= metric <= 1 for metric in values.values()),
                f"{label}: retrieval metric out of range",
            )
            require(
                isinstance(rankings[method], list)
                and all(is_sha256(item) for item in rankings[method])
                and len(rankings[method]) == len(set(rankings[method])),
                f"{label}: retrieval ranking differs",
            )
    require(len(observed) == len(set(observed)), f"{label}: duplicate retrieval case")
    require(set(observed) == set(RETRIEVAL_LANGUAGES), f"{label}: retrieval case inventory differs")


def _validate_arm_metadata(
    report: Mapping[str, Any],
    repetition: str,
    identities: Mapping[str, Any],
) -> dict[str, dict[str, Any]]:
    arms = report.get("arms")
    require(isinstance(arms, list) and len(arms) == 3, f"R{repetition}: arm count differs")
    metadata: list[dict[str, Any]] = []
    for arm in arms:
        require(
            isinstance(arm, dict) and set(arm) == {"metadata", "cases", "aggregates"},
            f"R{repetition}: arm schema differs",
        )
        item = arm.get("metadata")
        require(isinstance(item, dict), f"R{repetition}: arm metadata missing")
        require(set(item) == ARM_METADATA_KEYS, f"R{repetition}: arm metadata schema differs")
        metadata.append(item)
    observed_order = [item.get("armId") for item in metadata]
    require(observed_order == ARM_ORDER[repetition], f"R{repetition}: arm order differs")
    by_id = {item["armId"]: item for item in metadata}
    require(len(by_id) == 3, f"R{repetition}: duplicate arm ID")
    for arm_id, item in by_id.items():
        identity_name = "codex" if arm_id == ARM_SOL else "local"
        identity = identities[identity_name]
        require(item.get("runtimeSha256") == identity["sha256"], f"R{repetition}: runtime hash differs")
        require(item.get("runtimeVersion") == identity["version"], f"R{repetition}: runtime version differs")
    for arm_id in (ARM_4B, ARM_17B):
        expected_model = EXPECTED_MODEL_IDENTITIES[arm_id]
        require(
            all(by_id[arm_id].get(field) == expected for field, expected in expected_model.items())
            and by_id[arm_id].get("effort") is None
            and by_id[arm_id].get("effortTransport") is None
            and by_id[arm_id].get("effortEffectiveAttested") is None
            and (
                by_id[arm_id].get("sidecarIdleSecs"),
                by_id[arm_id].get("sidecarReadySecs"),
                by_id[arm_id].get("sidecarHardCapSecs"),
            )
            == (300, 90, 180),
            f"R{repetition}: local model identity differs",
        )
    require(
        by_id[ARM_SOL].get("modelClass") == "reference"
        and by_id[ARM_SOL].get("modelRequested") == "gpt-5.6-sol"
        and by_id[ARM_SOL].get("effort") == "high"
        and by_id[ARM_SOL].get("effortTransport")
        == '--config model_reasoning_effort="high"'
        and by_id[ARM_SOL].get("effortEffectiveAttested") is False
        and all(
            by_id[ARM_SOL].get(field) is None
            for field in (
                "modelFilename",
                "modelBytes",
                "modelSha256",
                "sidecarIdleSecs",
                "sidecarReadySecs",
                "sidecarHardCapSecs",
            )
        ),
        f"R{repetition}: Sol model/effort identity differs",
    )
    return by_id


def _validate_report(
    report: dict[str, Any],
    repetition: str,
    bundle: Mapping[str, Any],
    producer: Mapping[str, Any],
    identities: Mapping[str, Any],
    generation_commitments: Mapping[str, str],
    retrieval_commitments: Mapping[str, str],
    retrieval_fixture_sha256: str,
    retrieval_corpus_sha256: str,
) -> dict[str, dict[str, Any]]:
    require(set(report) == REPORT_KEYS, f"R{repetition}: root schema differs")
    require(report.get("schemaVersion") == 9, f"R{repetition}: schema version differs")
    require(report.get("syntheticOnly") is True, f"R{repetition}: not synthetic")
    require(
        report.get("snapshotStart") == report.get("snapshotEnd"),
        f"R{repetition}: start/end snapshots differ",
    )
    require(
        report.get("snapshotStart") == producer,
        f"R{repetition}: snapshot differs from producer",
    )
    for field in ("repositoryCommit", "sourceFingerprintSha256", "manifestSha256"):
        require(report.get(field) == producer.get(field), f"R{repetition}: {field} binding differs")
    environment = report.get("environment")
    require(isinstance(environment, dict), f"R{repetition}: environment missing")
    require(environment.get("armOrder") == ARM_ORDER[repetition], f"R{repetition}: environment arm order differs")
    require(environment.get("repetition") == repetition, f"R{repetition}: environment repetition differs")
    require(environment.get("workingTreeDirty") is False, f"R{repetition}: environment is dirty")
    require(
        environment.get("trackedDiffSha256") == EMPTY_SHA256,
        f"R{repetition}: environment tracked diff is not empty",
    )
    require(
        isinstance(report.get("promptVersion"), str) and report["promptVersion"],
        f"R{repetition}: prompt version missing",
    )
    metadata = _validate_arm_metadata(report, repetition, identities)
    _validate_hash_fields(report, f"R{repetition}")
    _validate_report_case_schemas(report, f"R{repetition}")
    _validate_numeric_semantics(report, f"R{repetition}")
    require(
        canonical_sha256(schema_inventory(report))
        == bundle["reportSchemaSha256"][repetition],
        f"R{repetition}: closed nested schema commitment differs",
    )
    same_caller = report.get("sameCallerEnvelopeModelStack")
    require(isinstance(same_caller, dict), f"R{repetition}: same-caller lane missing")
    same_caller_arms = same_caller.get("arms")
    require(
        isinstance(same_caller_arms, list)
        and [arm.get("armId") if isinstance(arm, dict) else None for arm in same_caller_arms]
        == ARM_ORDER[repetition],
        f"R{repetition}: same-caller arm order differs",
    )
    retrieval = report.get("retrievalQuality")
    require(isinstance(retrieval, dict), f"R{repetition}: retrieval binding missing")
    require(
        retrieval.get("fixtureSha256") == retrieval_fixture_sha256,
        f"R{repetition}: retrieval fixture source hash differs",
    )
    require(
        retrieval.get("corpusSourceSha256") == retrieval_corpus_sha256,
        f"R{repetition}: retrieval corpus source hash differs",
    )
    _validate_case_inventory(
        report,
        f"R{repetition}",
        generation_commitments,
        retrieval_commitments,
        ALL_CASE_IDS,
    )
    scan_privacy(report, f"R{repetition}")
    return metadata


def _expected_inventory(
    reports: Mapping[str, dict[str, Any]],
    evidence: Mapping[str, Any],
) -> dict[str, Any]:
    values: list[str] = []
    counts: dict[str, int] = {}
    commitments: dict[str, str] = {}
    for repetition in ("1", "2"):
        inventory = text_inventory(reports[repetition])
        counts[repetition] = len(inventory)
        commitments[repetition] = canonical_sha256(inventory)
        values.extend(value for _, value in inventory)
    return {
        "schemaVersion": 2,
        "kind": "murmur_synthetic_quality_all_string_inventory",
        "syntheticOnly": True,
        "logicalSha256ByRepetition": {
            repetition: evidence["repetitions"][repetition]["logicalSha256"]
            for repetition in ("1", "2")
        },
        "pathAndOccurrenceCommitmentSha256ByRepetition": commitments,
        "stringLeafCountByRepetition": counts,
        "uniqueStringCount": len(set(values)),
        "uniqueStrings": sorted(set(values)),
    }


@dataclass
class ArchiveState:
    bundle: dict[str, Any]
    evidence_bytes: bytes
    evidence: dict[str, Any]
    producer: dict[str, Any]
    identities: dict[str, Any]
    logical_bytes: dict[str, bytes]
    reports: dict[str, dict[str, Any]]
    arm_metadata: dict[str, dict[str, dict[str, Any]]]
    inventory_archive: bytes
    inventory: dict[str, Any]
    fixture_snapshot_bytes: bytes
    generation_commitments: dict[str, str]
    retrieval_commitments: dict[str, str]
    retrieval_fixture_sha256: str
    retrieval_corpus_sha256: str


def validate_archive_stage(reader: Reader) -> ArchiveState:
    evidence_bytes = reader.read(EVIDENCE_PATH)
    evidence = load_object(evidence_bytes, "evidence")
    producer, identities = _validate_evidence(evidence)
    bundle = _resolve_admitted_bundle(producer, identities)
    generation_commitments, fixture_snapshot_bytes = _generation_fixture_commitments(
        reader, producer
    )
    (
        retrieval_commitments,
        retrieval_fixture_sha256,
        retrieval_corpus_sha256,
    ) = _retrieval_fixture_commitments(reader)
    reports: dict[str, dict[str, Any]] = {}
    logical_bytes: dict[str, bytes] = {}
    arm_metadata: dict[str, dict[str, dict[str, Any]]] = {}
    for repetition, archive_path in (("1", R1_ARCHIVE_PATH), ("2", R2_ARCHIVE_PATH)):
        archive = reader.read(archive_path, MAX_ARCHIVE_BYTES)
        entry = evidence["repetitions"][repetition]
        require(sha256(archive) == entry["archiveSha256"], f"R{repetition}: archive hash differs")
        logical = decode_gzip(archive, f"R{repetition}")
        require(sha256(logical) == entry["logicalSha256"], f"R{repetition}: logical hash differs")
        report = load_object(logical, f"R{repetition}")
        arm_metadata[repetition] = _validate_report(
            report,
            repetition,
            bundle,
            producer,
            identities,
            generation_commitments,
            retrieval_commitments,
            retrieval_fixture_sha256,
            retrieval_corpus_sha256,
        )
        logical_bytes[repetition] = logical
        reports[repetition] = report
    require(
        arm_metadata["1"] == arm_metadata["2"],
        "repetitions: arm metadata differs beyond order",
    )
    require(
        reports["1"].get("promptVersion") == reports["2"].get("promptVersion"),
        "repetitions: prompt version differs",
    )
    inventory_archive = reader.read(INVENTORY_PATH, MAX_ARCHIVE_BYTES)
    inventory_bytes = decode_gzip(inventory_archive, "content inventory")
    inventory = load_object(inventory_bytes, "content inventory")
    expected_inventory = _expected_inventory(reports, evidence)
    require(inventory == expected_inventory, "content inventory: decoded content differs")
    expected_inventory_bytes = (
        json.dumps(expected_inventory, ensure_ascii=False, indent=2) + "\n"
    ).encode("utf-8")
    require(
        inventory_bytes == expected_inventory_bytes,
        "content inventory: logical encoding is not canonical",
    )
    scan_privacy(inventory, "content inventory")
    require(
        sha256(evidence_bytes) == bundle["evidenceSha256"]
        and evidence["repetitions"] == bundle["repetitions"]
        and evidence["combined"] == bundle["combined"],
        "archive bundle: evidence/artifact commitments are not atomically admitted",
    )
    require(
        sha256(inventory_archive) == bundle["inventorySha256"],
        "archive bundle: content inventory is not atomically admitted",
    )
    return ArchiveState(
        bundle=bundle,
        evidence_bytes=evidence_bytes,
        evidence=evidence,
        producer=producer,
        identities=identities,
        logical_bytes=logical_bytes,
        reports=reports,
        arm_metadata=arm_metadata,
        inventory_archive=inventory_archive,
        inventory=inventory,
        fixture_snapshot_bytes=fixture_snapshot_bytes,
        generation_commitments=generation_commitments,
        retrieval_commitments=retrieval_commitments,
        retrieval_fixture_sha256=retrieval_fixture_sha256,
        retrieval_corpus_sha256=retrieval_corpus_sha256,
    )


@dataclass
class FinalState:
    archive: ArchiveState
    combined_bytes: bytes
    combined: dict[str, Any]
    projection_bytes: bytes
    projection: dict[str, Any]


def _validate_combined(
    combined: dict[str, Any],
    archive: ArchiveState,
) -> None:
    require(set(combined) == COMBINED_KEYS, "combined: root schema differs")
    require(combined.get("schemaVersion") == 5, "combined: schema version differs")
    require(
        combined.get("comparisonType")
        == "separate_product_route_and_same_caller_envelope_lanes",
        "combined: comparison type differs",
    )
    require(
        isinstance(combined.get("design"), str)
        and "synthetic" in combined["design"].lower(),
        "combined: synthetic design marker missing",
    )
    _validate_hash_fields(combined, "combined")
    _validate_numeric_semantics(combined, "combined")
    require(
        canonical_sha256(schema_inventory(combined))
        == archive.bundle["combinedSchemaSha256"],
        "combined: closed nested schema commitment differs",
    )
    expected_hashes = {
        repetition: archive.evidence["repetitions"][repetition]["logicalSha256"]
        for repetition in ("1", "2")
    }
    expected_paths = {
        repetition: archive.evidence["repetitions"][repetition]["logicalPath"]
        for repetition in ("1", "2")
    }
    require(
        combined.get("inputResultSha256ByRepetition") == expected_hashes,
        "combined: repetition logical hashes differ",
    )
    require(
        combined.get("repeatFilesByRepetition") == expected_paths,
        "combined: repetition logical paths differ",
    )
    for field in ("repositoryCommit", "sourceFingerprintSha256", "manifestSha256"):
        require(combined.get(field) == archive.producer[field], f"combined: {field} differs")
    measurement = combined.get("measurementFileSha256")
    require(
        isinstance(measurement, dict)
        and set(measurement)
        == {"evaluatorFileSha256", "fixtureFileSha256", "repeatValidatorFileSha256"},
        "combined: measurement binding schema differs",
    )
    for field in measurement:
        require(measurement[field] == archive.producer[field], f"combined: {field} differs")
    retrieval = combined.get("retrievalQuality")
    require(isinstance(retrieval, dict), "combined: retrieval binding missing")
    require(
        retrieval.get("fixtureSha256") == archive.retrieval_fixture_sha256,
        "combined: retrieval fixture source hash differs",
    )
    require(
        retrieval.get("corpusSourceSha256") == archive.retrieval_corpus_sha256,
        "combined: retrieval corpus source hash differs",
    )
    require(
        archive.reports["1"].get("retrievalQuality")
        == archive.reports["2"].get("retrievalQuality")
        == retrieval,
        "combined: retrieval repetition binding differs",
    )
    _validate_case_inventory(
        combined,
        "combined",
        archive.generation_commitments,
        archive.retrieval_commitments,
        COMBINED_CASE_IDS,
    )
    scan_privacy(combined, "combined")


def _projection_inventory_summary(archive: ArchiveState) -> dict[str, Any]:
    inventory = archive.inventory
    return {
        "schemaVersion": inventory["schemaVersion"],
        "kind": inventory["kind"],
        "syntheticOnly": inventory["syntheticOnly"],
        "logicalSha256ByRepetition": copy.deepcopy(
            inventory["logicalSha256ByRepetition"]
        ),
        "pathAndOccurrenceCommitmentSha256ByRepetition": copy.deepcopy(
            inventory["pathAndOccurrenceCommitmentSha256ByRepetition"]
        ),
        "stringLeafCountByRepetition": copy.deepcopy(
            inventory["stringLeafCountByRepetition"]
        ),
        "uniqueStringCount": inventory["uniqueStringCount"],
        "uniqueStringsCommitmentSha256": canonical_sha256(inventory["uniqueStrings"]),
    }


def _derive_quality_summary(value: Any, label: str) -> dict[str, Any]:
    require(isinstance(value, dict), f"{label}: aggregate must be an object")
    result: dict[str, Any] = {}
    for key in (
        "observations",
        "cases",
        "callSuccessRate",
        "casePassRate",
        "surfaceMacroPassRate",
        "criticalFailureObservations",
        "criticalFailureCases",
        "diagnosticScoreMean",
    ):
        if key in value:
            result[key] = copy.deepcopy(value[key])
    require(
        "callSuccessRate" in result and "casePassRate" in result,
        f"{label}: aggregate summary fields missing",
    )
    return result


def _derive_arm_summary(value: Any, label: str) -> dict[str, Any]:
    require(isinstance(value, dict), f"{label}: arm must be an object")
    cohorts = value.get("cohorts")
    languages = value.get("languages")
    require(
        isinstance(cohorts, dict) and set(cohorts) == {"all", "calibration", "holdout"},
        f"{label}: cohort inventory differs",
    )
    require(
        isinstance(languages, dict) and set(languages) == {"en", "pl"},
        f"{label}: language inventory differs",
    )
    overall = cohorts["all"]
    require(isinstance(overall, dict), f"{label}: overall aggregate missing")
    surface_rates = overall.get("surfacePassRates", {})
    dimensions = overall.get("dimensions", {})
    require(isinstance(surface_rates, dict), f"{label}: surface rates differ")
    require(isinstance(dimensions, dict), f"{label}: dimensions differ")
    result: dict[str, Any] = {}
    for key in ("armId", "modelRequested"):
        if key in value:
            result[key] = copy.deepcopy(value[key])
    result.update(
        {
            "overall": _derive_quality_summary(overall, f"{label} overall"),
            "cohorts": {
                cohort: _derive_quality_summary(cohorts[cohort], f"{label} {cohort}")
                for cohort in ("calibration", "holdout")
            },
            "languages": {
                language: _derive_quality_summary(languages[language], f"{label} {language}")
                for language in ("en", "pl")
            },
            "surfacePassRates": copy.deepcopy(surface_rates),
            "dimensionCoverageAndPass": copy.deepcopy(dimensions),
        }
    )
    return result


def _derive_paired_scope(value: Any, label: str) -> dict[str, Any]:
    require(isinstance(value, dict), f"{label}: paired scope must be an object")
    local = value.get("local")
    reference = value.get("reference")
    require(isinstance(local, dict), f"{label}: local aggregate missing")
    require(isinstance(reference, dict), f"{label}: reference aggregate missing")
    return {
        "matchedObservations": value.get("matchedObservations"),
        "local": _derive_quality_summary(local, f"{label} local"),
        "reference": _derive_quality_summary(reference, f"{label} reference"),
        "referenceMinusLocalDiagnosticMean": value.get(
            "referenceMinusLocalDiagnosticMean"
        ),
    }


def _derive_product_pair(value: Any, label: str) -> dict[str, Any]:
    require(isinstance(value, dict), f"{label}: pair must be an object")
    cohorts = value.get("cohorts")
    languages = value.get("languages")
    route_pairs = value.get("routeProfilePairs")
    require(
        isinstance(cohorts, dict) and set(cohorts) == {"all", "calibration", "holdout"},
        f"{label}: pair cohorts differ",
    )
    require(
        isinstance(languages, dict) and set(languages) == {"en", "pl"},
        f"{label}: pair languages differ",
    )
    require(isinstance(route_pairs, list), f"{label}: route profiles missing")
    profile_fields = (
        "comparisonKind",
        "localGenerationProfile",
        "referenceGenerationProfile",
        "localProductRoute",
        "referenceProductRoute",
    )
    distinct_profiles = sorted(
        {
            tuple(route.get(field) for field in profile_fields)
            for route in route_pairs
            if isinstance(route, dict)
        }
    )
    require(
        distinct_profiles
        and all(all(isinstance(item, str) for item in profile) for profile in distinct_profiles),
        f"{label}: route profile fields differ",
    )
    identical_inputs = sum(
        route.get("localRouteInputSha256") == route.get("referenceRouteInputSha256")
        for route in route_pairs
        if isinstance(route, dict)
    )
    return {
        "localArm": value.get("localArm"),
        "referenceArm": value.get("referenceArm"),
        "comparisonType": value.get("comparisonType"),
        "cohorts": {
            cohort: _derive_paired_scope(cohorts[cohort], f"{label} {cohort}")
            for cohort in ("all", "calibration", "holdout")
        },
        "languages": {
            language: _derive_paired_scope(languages[language], f"{label} {language}")
            for language in ("en", "pl")
        },
        "routeComparison": {
            "observations": len(route_pairs),
            "identicalRouteInputObservations": identical_inputs,
            "differentRouteInputObservations": len(route_pairs) - identical_inputs,
            "routeProfilePairsCommitmentSha256": canonical_sha256(route_pairs),
            "distinctProfiles": [
                dict(zip(profile_fields, profile)) for profile in distinct_profiles
            ],
        },
    }


def _lane_cases(
    report: Mapping[str, Any],
    lane: str,
    label: str,
) -> dict[tuple[str, str], dict[str, Any]]:
    if lane == "productRoute":
        arms = report.get("arms")
        metadata_arm = True
    else:
        stack = report.get("sameCallerEnvelopeModelStack")
        require(isinstance(stack, dict), f"{label}: same-caller stack missing")
        arms = stack.get("arms")
        metadata_arm = False
    require(isinstance(arms, list), f"{label}: arm list missing")
    result: dict[tuple[str, str], dict[str, Any]] = {}
    for arm in arms:
        require(isinstance(arm, dict), f"{label}: arm record differs")
        if metadata_arm:
            metadata = arm.get("metadata")
            require(isinstance(metadata, dict), f"{label}: arm metadata missing")
            arm_id = metadata.get("armId")
        else:
            arm_id = arm.get("armId")
        cases = arm.get("cases")
        require(isinstance(arm_id, str), f"{label}: arm ID missing")
        require(isinstance(cases, list), f"{label}: cases missing")
        for case in cases:
            require(isinstance(case, dict), f"{label}: case record differs")
            case_id = case.get("caseId")
            require(isinstance(case_id, str), f"{label}: case ID missing")
            key = (arm_id, case_id)
            require(key not in result, f"{label}: duplicate arm/case observation")
            result[key] = case
    return result


def _failed_checks(score: Any, label: str) -> list[str]:
    require(isinstance(score, dict), f"{label}: score missing")
    return sorted(
        key
        for key, value in score.items()
        if key != "casePass" and key.endswith("Pass") and value is False
    )


def _derive_failures(
    reports: Mapping[str, dict[str, Any]],
    lane: str,
) -> list[dict[str, Any]]:
    failures: dict[tuple[str, str], dict[str, Any]] = {}
    for repetition in ("1", "2"):
        cases = _lane_cases(reports[repetition], lane, f"{lane} R{repetition}")
        for (arm_id, case_id), case in cases.items():
            score = case.get("score")
            require(isinstance(score, dict), f"{lane}: score missing")
            if score.get("casePass") is not False:
                continue
            record = failures.setdefault(
                (arm_id, case_id),
                {
                    "armId": arm_id,
                    "caseId": case_id,
                    "surface": case.get("surface"),
                    "language": case.get("language"),
                    "casePayloadSha256": case.get("casePayloadSha256"),
                    "failingRepetitions": [],
                    "criticalFailureRepetitions": [],
                    "diagnosticScoreByRepetition": {},
                    "failedChecksByRepetition": {},
                    "errorPresentByRepetition": {},
                },
            )
            record["failingRepetitions"].append(repetition)
            if score.get("criticalFailure") is True:
                record["criticalFailureRepetitions"].append(repetition)
            record["diagnosticScoreByRepetition"][repetition] = score.get(
                "diagnosticScore"
            )
            record["failedChecksByRepetition"][repetition] = _failed_checks(
                score, f"{lane} {arm_id}/{case_id} R{repetition}"
            )
            record["errorPresentByRepetition"][repetition] = case.get("error") is not None
    return [failures[key] for key in sorted(failures)]


def _new_stability_bucket(
    include_surface_output: bool,
    include_envelope: bool,
) -> dict[str, Any]:
    bucket: dict[str, Any] = {
        "comparableObservations": 0,
        "identicalFinalOutputHashes": 0,
        "changedFinalOutputHashes": 0,
        "identicalRawOutputHashes": 0,
        "changedRawOutputHashes": 0,
        "stablePassOutcomes": 0,
        "passFlips": 0,
        "stableCriticalFailureOutcomes": 0,
        "criticalFailureFlips": 0,
        "changedFinalOutputCaseIds": [],
        "changedRawOutputCaseIds": [],
        "passFlipCaseIds": [],
        "criticalFailureFlipCaseIds": [],
    }
    if include_surface_output:
        bucket.update(
            {
                "comparableSurfaceOutputHashes": 0,
                "identicalSurfaceOutputHashes": 0,
                "changedSurfaceOutputHashes": 0,
                "changedSurfaceOutputCaseIds": [],
            }
        )
    if include_envelope:
        bucket.update(
            {
                "identicalEnvelopeHashes": 0,
                "changedEnvelopeHashes": 0,
                "changedEnvelopeCaseIds": [],
            }
        )
    return bucket


def _update_stability_bucket(
    bucket: dict[str, Any],
    case_id: str,
    first: Mapping[str, Any],
    second: Mapping[str, Any],
    raw_hash_field: str,
    surface_hash_field: str | None,
    envelope_hash_field: str | None,
) -> None:
    bucket["comparableObservations"] += 1
    for source_field, identical_key, changed_key, changed_ids_key in (
        (
            "outputSha256",
            "identicalFinalOutputHashes",
            "changedFinalOutputHashes",
            "changedFinalOutputCaseIds",
        ),
        (
            raw_hash_field,
            "identicalRawOutputHashes",
            "changedRawOutputHashes",
            "changedRawOutputCaseIds",
        ),
    ):
        if first.get(source_field) == second.get(source_field):
            bucket[identical_key] += 1
        else:
            bucket[changed_key] += 1
            bucket[changed_ids_key].append(case_id)
    if surface_hash_field is not None and not (
        first.get(surface_hash_field) is None and second.get(surface_hash_field) is None
    ):
        bucket["comparableSurfaceOutputHashes"] += 1
        if first.get(surface_hash_field) == second.get(surface_hash_field):
            bucket["identicalSurfaceOutputHashes"] += 1
        else:
            bucket["changedSurfaceOutputHashes"] += 1
            bucket["changedSurfaceOutputCaseIds"].append(case_id)
    if envelope_hash_field is not None:
        if first.get(envelope_hash_field) == second.get(envelope_hash_field):
            bucket["identicalEnvelopeHashes"] += 1
        else:
            bucket["changedEnvelopeHashes"] += 1
            bucket["changedEnvelopeCaseIds"].append(case_id)

    first_score = first.get("score")
    second_score = second.get("score")
    require(isinstance(first_score, dict), "output stability: R1 score missing")
    require(isinstance(second_score, dict), "output stability: R2 score missing")
    if first_score.get("casePass") == second_score.get("casePass"):
        bucket["stablePassOutcomes"] += 1
    else:
        bucket["passFlips"] += 1
        bucket["passFlipCaseIds"].append(case_id)
    if first_score.get("criticalFailure") == second_score.get("criticalFailure"):
        bucket["stableCriticalFailureOutcomes"] += 1
    else:
        bucket["criticalFailureFlips"] += 1
        bucket["criticalFailureFlipCaseIds"].append(case_id)


def _derive_output_stability(
    reports: Mapping[str, dict[str, Any]],
    lane: str,
) -> dict[str, Any]:
    first = _lane_cases(reports["1"], lane, f"{lane} R1 stability")
    second = _lane_cases(reports["2"], lane, f"{lane} R2 stability")
    require(set(first) == set(second), f"{lane}: repetition case inventory differs")
    raw_hash_field = "rawModelOutputSha256" if lane == "productRoute" else "rawOutputSha256"
    surface_hash_field = "surfaceOutputSha256" if lane == "productRoute" else None
    envelope_hash_field = "envelopeSha256" if lane == "sameCaller" else None
    overall = _new_stability_bucket(
        surface_hash_field is not None, envelope_hash_field is not None
    )
    by_arm: dict[str, dict[str, Any]] = {}
    pass_flips: list[dict[str, Any]] = []
    critical_flips: list[dict[str, Any]] = []
    for arm_id, case_id in sorted(first):
        first_case = first[(arm_id, case_id)]
        second_case = second[(arm_id, case_id)]
        arm_bucket = by_arm.setdefault(
            arm_id,
            _new_stability_bucket(
                surface_hash_field is not None,
                envelope_hash_field is not None,
            ),
        )
        _update_stability_bucket(
            overall,
            f"{arm_id}:{case_id}",
            first_case,
            second_case,
            raw_hash_field,
            surface_hash_field,
            envelope_hash_field,
        )
        _update_stability_bucket(
            arm_bucket,
            case_id,
            first_case,
            second_case,
            raw_hash_field,
            surface_hash_field,
            envelope_hash_field,
        )
        first_score = first_case["score"]
        second_score = second_case["score"]
        base_flip = {
            "armId": arm_id,
            "caseId": case_id,
            "surface": first_case.get("surface"),
            "language": first_case.get("language"),
        }
        if first_score.get("casePass") != second_score.get("casePass"):
            pass_flips.append(
                {
                    **base_flip,
                    "r1CasePass": first_score.get("casePass"),
                    "r2CasePass": second_score.get("casePass"),
                }
            )
        if first_score.get("criticalFailure") != second_score.get("criticalFailure"):
            critical_flips.append(
                {
                    **base_flip,
                    "r1CriticalFailure": first_score.get("criticalFailure"),
                    "r2CriticalFailure": second_score.get("criticalFailure"),
                }
            )
    return {
        "overall": overall,
        "byArm": [{"armId": arm_id, **by_arm[arm_id]} for arm_id in sorted(by_arm)],
        "passFlips": pass_flips,
        "criticalFailureFlips": critical_flips,
    }


def _percentage(numerator: int, denominator: int) -> float | None:
    if denominator == 0:
        return None
    return round(100.0 * numerator / denominator, 1)


def _derive_surface_summaries(
    reports: Mapping[str, dict[str, Any]],
    lane: str,
) -> list[dict[str, Any]]:
    grouped: dict[tuple[str, str], list[dict[str, Any]]] = {}
    for repetition in ("1", "2"):
        for (arm_id, _), case in _lane_cases(
            reports[repetition], lane, f"{lane} R{repetition} surfaces"
        ).items():
            surface = case.get("surface")
            require(isinstance(surface, str), f"{lane}: surface missing")
            grouped.setdefault((arm_id, surface), []).append(case)
    result: dict[str, dict[str, Any]] = {}
    for (arm_id, surface), cases in sorted(grouped.items()):
        scores = [case.get("score") for case in cases]
        require(
            all(isinstance(score, dict) for score in scores),
            f"{lane}: surface score missing",
        )
        diagnostic_scores = [score.get("diagnosticScore") for score in scores]
        require(
            all(_is_number(score) for score in diagnostic_scores),
            f"{lane}: diagnostic score differs",
        )
        result.setdefault(arm_id, {})[surface] = {
            "observations": len(cases),
            "callSuccessRate": _percentage(
                sum(case.get("error") is None for case in cases), len(cases)
            ),
            "casePassRate": _percentage(
                sum(score.get("casePass") is True for score in scores), len(cases)
            ),
            "criticalFailureObservations": sum(
                score.get("criticalFailure") is True for score in scores
            ),
            "diagnosticScoreMean": round(sum(diagnostic_scores) / len(cases), 1),
        }
    return [
        {"armId": arm_id, "surfaces": result[arm_id]} for arm_id in sorted(result)
    ]


def _count_string_field(rows: list[Any], field: str, label: str) -> dict[str, int]:
    counts: dict[str, int] = {}
    for row in rows:
        require(isinstance(row, dict), f"{label}: ledger row differs")
        value = row.get(field)
        require(isinstance(value, str), f"{label}: {field} missing")
        counts[value] = counts.get(value, 0) + 1
    return {key: counts[key] for key in sorted(counts)}


def _derive_egress(ledger: Any, label: str) -> dict[str, Any]:
    require(isinstance(ledger, dict), f"{label}: egress ledger missing")
    rows = ledger.get("rows")
    require(isinstance(rows, list), f"{label}: egress rows missing")
    redaction_fields = {
        "email": "redactionsEmail",
        "card": "redactionsCard",
        "phone": "redactionsPhone",
        "name": "redactionsName",
    }
    redactions: dict[str, int] = {}
    for output_name, source_name in redaction_fields.items():
        values = [row.get(source_name) for row in rows if isinstance(row, dict)]
        require(
            len(values) == len(rows)
            and all(isinstance(value, int) and value >= 0 for value in values),
            f"{label}: redaction count differs",
        )
        redactions[output_name] = sum(values)
    redactions["total"] = sum(redactions.values())
    redactions["rowsWithAnyRedaction"] = sum(
        any(row.get(field, 0) > 0 for field in redaction_fields.values())
        for row in rows
        if isinstance(row, dict)
    )
    system_bytes = [row.get("systemBytes") for row in rows if isinstance(row, dict)]
    user_bytes = [row.get("userBytes") for row in rows if isinstance(row, dict)]
    require(
        len(system_bytes) == len(rows)
        and all(isinstance(value, int) and value >= 0 for value in system_bytes),
        f"{label}: system byte count differs",
    )
    require(
        len(user_bytes) == len(rows)
        and all(isinstance(value, int) and value >= 0 for value in user_bytes),
        f"{label}: user byte count differs",
    )
    served_models = sorted(
        {
            row.get("modelServed")
            for row in rows
            if isinstance(row, dict) and isinstance(row.get("modelServed"), str)
        }
    )
    return {
        "required": ledger.get("required"),
        "sqlitePersistenceVerified": ledger.get("sqlitePersistenceVerified"),
        "temporaryDatabaseCleaned": ledger.get("temporaryDatabaseCleaned"),
        "attemptedRows": ledger.get("attemptedRows"),
        "persistedRows": ledger.get("persistedRows"),
        "persistenceFailures": ledger.get("persistenceFailures"),
        "projectedRowCount": len(rows),
        "contentFreeRowsSha256": ledger.get("contentFreeRowsSha256"),
        "providerIds": copy.deepcopy(ledger.get("providerIds")),
        "callKinds": copy.deepcopy(ledger.get("callKinds")),
        "providerRowCounts": _count_string_field(rows, "providerId", label),
        "destinationRowCounts": _count_string_field(rows, "destination", label),
        "callKindRowCounts": _count_string_field(rows, "callKind", label),
        "requestedModelRowCounts": _count_string_field(rows, "modelRequested", label),
        "servedModelsObserved": served_models,
        "rowsWithoutServedModelAttestation": sum(
            row.get("modelServed") is None for row in rows if isinstance(row, dict)
        ),
        "systemBytes": sum(system_bytes),
        "userBytes": sum(user_bytes),
        "redactions": redactions,
    }


def _derive_repetition(
    repetition: str,
    report: Mapping[str, Any],
    producer: Mapping[str, Any],
) -> dict[str, Any]:
    environment = report.get("environment")
    start = report.get("snapshotStart")
    end = report.get("snapshotEnd")
    arms = report.get("arms")
    require(isinstance(environment, dict), f"R{repetition}: environment missing")
    require(isinstance(start, dict), f"R{repetition}: start snapshot missing")
    require(isinstance(end, dict), f"R{repetition}: end snapshot missing")
    require(isinstance(arms, list), f"R{repetition}: arms missing")
    clean_and_unchanged = (
        start == end
        and start.get("workingTreeDirty") is False
        and start.get("trackedDiffSha256") == EMPTY_SHA256
        and environment.get("workingTreeDirty") is False
        and environment.get("trackedDiffSha256") == EMPTY_SHA256
    )
    producer_fields = (
        "repositoryCommit",
        "sourceFingerprintSha256",
        "manifestSha256",
        "evaluatorFileSha256",
        "fixtureFileSha256",
        "repeatValidatorFileSha256",
        "trackedDiffSha256",
        "workingTreeDirty",
    )
    source_matches_producer = all(
        start.get(field) == producer.get(field) and end.get(field) == producer.get(field)
        for field in producer_fields
    )
    environment_projection = {
        key: copy.deepcopy(value)
        for key, value in environment.items()
        if key
        not in {
            "armOrder",
            "repetition",
            "trackedDiffSha256",
            "workingTreeDirty",
        }
    }
    return {
        "repetition": repetition,
        "runLabel": report.get("runLabel"),
        "generatedAt": report.get("generatedAt"),
        "armOrder": copy.deepcopy(environment.get("armOrder")),
        "environment": environment_projection,
        "snapshotStart": copy.deepcopy(start),
        "snapshotEnd": copy.deepcopy(end),
        "cleanAndUnchanged": clean_and_unchanged,
        "sourceBindingsMatchProducer": source_matches_producer,
        "arms": [copy.deepcopy(arm.get("metadata")) for arm in arms],
        "egress": _derive_egress(report.get("egressLedger"), f"R{repetition}"),
    }


def _derive_retrieval(
    reports: Mapping[str, dict[str, Any]],
    combined: Mapping[str, Any],
) -> dict[str, Any]:
    retrieval = combined.get("retrievalQuality")
    require(isinstance(retrieval, dict), "projection derivation: retrieval missing")
    repeated = {
        repetition: reports[repetition].get("retrievalQuality")
        for repetition in ("1", "2")
    }
    require(
        all(isinstance(value, dict) for value in repeated.values()),
        "projection derivation: repeated retrieval missing",
    )
    case_metrics = retrieval.get("cases")
    require(isinstance(case_metrics, list), "projection derivation: retrieval cases missing")
    projected_cases: list[dict[str, Any]] = []
    for case in case_metrics:
        require(isinstance(case, dict), "projection derivation: retrieval case differs")
        projected_cases.append(
            {
                "caseId": case.get("caseId"),
                "language": case.get("language"),
                "queryPayloadSha256": case.get("queryPayloadSha256"),
                "expectedMeetings": case.get("expectedMeetings"),
                "metrics": copy.deepcopy(case.get("metrics")),
            }
        )
    metadata = {
        key: copy.deepcopy(value)
        for key, value in retrieval.items()
        if key not in {"aggregates", "cases"}
    }
    return {
        "metadata": metadata,
        "aggregates": copy.deepcopy(retrieval.get("aggregates")),
        "caseMetrics": projected_cases,
        "repetitionBindings": {
            "canonicalSha256ByRepetition": {
                repetition: canonical_sha256(repeated[repetition])
                for repetition in ("1", "2")
            },
            "combinedCanonicalSha256": canonical_sha256(retrieval),
            "allRepetitionsEqualCombined": repeated["1"] == repeated["2"] == retrieval,
        },
    }


def _derive_final_projection(
    combined: Mapping[str, Any],
    combined_bytes: bytes,
    archive: ArchiveState,
) -> dict[str, Any]:
    reports = archive.reports
    for field in (
        "benchmarkDesign",
        "evidenceScope",
        "evidenceLimits",
        "holdoutInterpretation",
        "promptVersion",
    ):
        require(
            reports["1"].get(field) == reports["2"].get(field),
            f"projection derivation: repetition {field} differs",
        )
    artifact_bindings = {
        "evidence": {"path": EVIDENCE_PATH, "sha256": sha256(archive.evidence_bytes)},
        "fixtureSnapshot": {
            "path": FIXTURE_SNAPSHOT_PATH,
            "sha256": sha256(archive.fixture_snapshot_bytes),
        },
        "repetitions": copy.deepcopy(archive.evidence["repetitions"]),
        "combined": {"path": COMBINED_PATH, "sha256": sha256(combined_bytes)},
        "contentInventory": {
            "path": INVENTORY_PATH,
            "sha256": sha256(archive.inventory_archive),
        },
    }
    repetitions = [
        _derive_repetition(repetition, reports[repetition], archive.producer)
        for repetition in ("1", "2")
    ]
    product_arms = combined.get("arms")
    product_pairs = combined.get("paired")
    same_caller = combined.get("sameCallerEnvelopeModelStack")
    local_composite = combined.get("localComposite")
    require(isinstance(product_arms, list), "projection derivation: product arms missing")
    require(isinstance(product_pairs, list), "projection derivation: product pairs missing")
    require(isinstance(same_caller, dict), "projection derivation: same-caller missing")
    require(isinstance(local_composite, dict), "projection derivation: local composite missing")
    same_caller_arms = same_caller.get("arms")
    require(isinstance(same_caller_arms, list), "projection derivation: same-caller arms missing")
    measurement = {
        "pairwiseReversedArmOrder": repetitions[1]["armOrder"]
        == list(reversed(repetitions[0]["armOrder"])),
        "allSnapshotsCleanAndUnchanged": all(
            repetition["cleanAndUnchanged"] for repetition in repetitions
        ),
        "allSourceBindingsMatchProducer": all(
            repetition["sourceBindingsMatchProducer"] for repetition in repetitions
        ),
    }
    require(all(measurement.values()), "projection derivation: measurement integrity differs")
    return {
        "schemaVersion": 1,
        "kind": "murmur_local_cloud_quality_final_review_projection",
        "syntheticOnly": True,
        "contentPolicy": {
            "containsRawOutputs": False,
            "omittedSourceFields": sorted(PROJECTION_FORBIDDEN_KEYS),
        },
        "evidenceMethod": archive.evidence.get("evidenceMethod"),
        "scope": {
            "benchmarkDesign": reports["1"].get("benchmarkDesign"),
            "evidenceScope": reports["1"].get("evidenceScope"),
            "evidenceLimits": copy.deepcopy(reports["1"].get("evidenceLimits")),
            "combinedDesign": combined.get("design"),
            "holdoutInterpretation": combined.get("holdoutInterpretation"),
            "dimensionAttribution": copy.deepcopy(combined.get("dimensionAttribution")),
        },
        "artifactBindings": artifact_bindings,
        "artifactBindingsCanonicalSha256": canonical_sha256(artifact_bindings),
        "producerSnapshot": copy.deepcopy(archive.producer),
        "runtimeIdentities": copy.deepcopy(archive.identities),
        "measurementIntegrity": measurement,
        "repetitions": repetitions,
        "productRoute": {
            "comparisonType": combined.get("comparisonType"),
            "rawAggregatePolicy": combined.get("rawAggregatePolicy"),
            "arms": [
                _derive_arm_summary(arm, f"product arm {index}")
                for index, arm in enumerate(product_arms)
            ],
            "localComposite": _derive_arm_summary(
                local_composite, "product local composite"
            ),
            "paired": [
                _derive_product_pair(pair, f"product pair {index}")
                for index, pair in enumerate(product_pairs)
            ],
            "failures": _derive_failures(reports, "productRoute"),
            "outputStability": _derive_output_stability(reports, "productRoute"),
        },
        "sameCallerEnvelopeModelStack": {
            "laneId": same_caller.get("laneId"),
            "entrypoint": same_caller.get("entrypoint"),
            "equalityBoundary": same_caller.get("equalityBoundary"),
            "providerRenderedPromptsByteIdentical": same_caller.get(
                "providerRenderedPromptsByteIdentical"
            ),
            "effectiveModelInputsAttestedIdentical": same_caller.get(
                "effectiveModelInputsAttestedIdentical"
            ),
            "interpretation": same_caller.get("interpretation"),
            "arms": [
                _derive_arm_summary(arm, f"same-caller arm {index}")
                for index, arm in enumerate(same_caller_arms)
            ],
            "paired": copy.deepcopy(same_caller.get("paired")),
            "surfaceAggregates": _derive_surface_summaries(reports, "sameCaller"),
            "failures": _derive_failures(reports, "sameCaller"),
            "outputStability": _derive_output_stability(reports, "sameCaller"),
        },
        "retrievalQuality": _derive_retrieval(reports, combined),
        "contentInventory": _projection_inventory_summary(archive),
    }


def _validate_projection_repetitions(
    projection: Mapping[str, Any],
    archive: ArchiveState,
) -> None:
    repetitions = projection.get("repetitions")
    require(isinstance(repetitions, list) and len(repetitions) == 2, "projection: repetitions differ")
    environment_fields = {
        "cpuBrand",
        "hardwareModel",
        "memoryBytes",
        "nameRedactorMode",
        "osBuild",
        "osVersion",
    }
    egress_fields = {
        "attemptedRows",
        "callKinds",
        "contentFreeRowsSha256",
        "persistedRows",
        "persistenceFailures",
        "providerIds",
        "required",
        "sqlitePersistenceVerified",
        "temporaryDatabaseCleaned",
    }
    for index, repetition in enumerate(("1", "2")):
        projected = repetitions[index]
        report = archive.reports[repetition]
        require(isinstance(projected, dict), f"projection: R{repetition} entry missing")
        require(set(projected) == PROJECTION_REPETITION_KEYS, f"projection: R{repetition} schema differs")
        require(projected.get("repetition") == repetition, f"projection: R{repetition} label differs")
        require(projected.get("armOrder") == ARM_ORDER[repetition], f"projection: R{repetition} arm order differs")
        require(
            projected.get("arms") == [arm["metadata"] for arm in report["arms"]],
            f"projection: R{repetition} arm metadata differs",
        )
        for field in ("generatedAt", "runLabel", "snapshotStart", "snapshotEnd"):
            require(projected.get(field) == report.get(field), f"projection: R{repetition} {field} differs")
        require(projected.get("cleanAndUnchanged") is True, f"projection: R{repetition} clean marker differs")
        require(
            projected.get("sourceBindingsMatchProducer") is True,
            f"projection: R{repetition} source binding marker differs",
        )
        expected_environment = {
            field: report["environment"].get(field) for field in environment_fields
        }
        require(
            projected.get("environment") == expected_environment,
            f"projection: R{repetition} environment differs",
        )
        projected_egress = projected.get("egress")
        require(isinstance(projected_egress, dict), f"projection: R{repetition} egress missing")
        for field in egress_fields:
            require(
                projected_egress.get(field) == report["egressLedger"].get(field),
                f"projection: R{repetition} egress {field} differs",
            )


def _validate_projection(
    projection: dict[str, Any],
    projection_bytes: bytes,
    combined: Mapping[str, Any],
    combined_bytes: bytes,
    archive: ArchiveState,
) -> None:
    require(set(projection) == PROJECTION_KEYS, "projection: root schema differs")
    require(projection.get("schemaVersion") == 1, "projection: schema version differs")
    require(
        projection.get("kind") == "murmur_local_cloud_quality_final_review_projection",
        "projection: kind differs",
    )
    require(projection.get("syntheticOnly") is True, "projection: not synthetic")
    canonical_bytes = (
        json.dumps(
            projection,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
        )
        + "\n"
    ).encode("utf-8")
    require(projection_bytes == canonical_bytes, "projection: encoding is not canonical compact JSON")
    scan_privacy(projection, "projection")
    for key in walk_keys(projection):
        require(key not in PROJECTION_FORBIDDEN_KEYS, "projection: content-bearing field retained")
    _validate_hash_fields(projection, "projection")
    _validate_numeric_semantics(projection, "projection")
    expected_bindings = {
        "combined": {"path": COMBINED_PATH, "sha256": sha256(combined_bytes)},
        "contentInventory": {"path": INVENTORY_PATH, "sha256": sha256(archive.inventory_archive)},
        "evidence": {"path": EVIDENCE_PATH, "sha256": sha256(archive.evidence_bytes)},
        "fixtureSnapshot": {
            "path": FIXTURE_SNAPSHOT_PATH,
            "sha256": sha256(archive.fixture_snapshot_bytes),
        },
        "repetitions": copy.deepcopy(archive.evidence["repetitions"]),
    }
    require(projection.get("artifactBindings") == expected_bindings, "projection: artifact bindings differ")
    require(
        projection.get("artifactBindingsCanonicalSha256")
        == canonical_sha256(expected_bindings),
        "projection: artifact binding commitment differs",
    )
    require(projection.get("producerSnapshot") == archive.producer, "projection: producer differs")
    require(projection.get("runtimeIdentities") == archive.identities, "projection: runtimes differ")
    require(
        projection.get("measurementIntegrity")
        == {
            "allSnapshotsCleanAndUnchanged": True,
            "allSourceBindingsMatchProducer": True,
            "pairwiseReversedArmOrder": True,
        },
        "projection: measurement integrity markers differ",
    )
    require(
        projection.get("contentInventory") == _projection_inventory_summary(archive),
        "projection: inventory binding differs",
    )
    content_policy = projection.get("contentPolicy")
    require(isinstance(content_policy, dict), "projection: content policy missing")
    require(content_policy.get("containsRawOutputs") is False, "projection: raw output marker differs")
    require(
        set(content_policy.get("omittedSourceFields", [])) == PROJECTION_FORBIDDEN_KEYS,
        "projection: omitted content field inventory differs",
    )
    require(
        projection.get("evidenceMethod") == archive.evidence.get("evidenceMethod"),
        "projection: evidence method differs",
    )
    _validate_projection_repetitions(projection, archive)
    scope = projection.get("scope")
    require(isinstance(scope, dict), "projection: scope missing")
    shared_report_fields = {
        "benchmarkDesign": "benchmarkDesign",
        "evidenceLimits": "evidenceLimits",
        "evidenceScope": "evidenceScope",
        "holdoutInterpretation": "holdoutInterpretation",
    }
    for projected_field, report_field in shared_report_fields.items():
        require(
            archive.reports["1"].get(report_field) == archive.reports["2"].get(report_field),
            f"repetitions: {report_field} differs",
        )
        require(
            scope.get(projected_field) == archive.reports["1"].get(report_field),
            f"projection: scope {projected_field} differs",
        )
    require(scope.get("combinedDesign") == combined.get("design"), "projection: combined design differs")
    require(
        scope.get("dimensionAttribution") == combined.get("dimensionAttribution"),
        "projection: dimension attribution differs",
    )
    product_route = projection.get("productRoute")
    require(isinstance(product_route, dict), "projection: product route missing")
    require(
        product_route.get("comparisonType") == combined.get("comparisonType"),
        "projection: comparison type differs",
    )
    expected_projection = _derive_final_projection(combined, combined_bytes, archive)
    require(
        projection == expected_projection,
        "projection: independently derived content differs",
    )


def validate_final(reader: Reader) -> FinalState:
    archive = validate_archive_stage(reader)
    combined_entry = archive.evidence["combined"]
    combined_bytes = reader.read(COMBINED_PATH)
    require(sha256(combined_bytes) == combined_entry["sha256"], "combined: file hash differs")
    combined = load_object(combined_bytes, "combined")
    _validate_combined(combined, archive)
    projection_bytes = reader.read(PROJECTION_PATH)
    projection = load_object(projection_bytes, "projection")
    _validate_projection(projection, projection_bytes, combined, combined_bytes, archive)
    require(
        sha256(projection_bytes) == archive.bundle["projectionSha256"],
        "projection: artifact is not atomically admitted",
    )
    return FinalState(
        archive=archive,
        combined_bytes=combined_bytes,
        combined=combined,
        projection_bytes=projection_bytes,
        projection=projection,
    )


def _subject_resource_limits() -> None:
    # The product validator is deliberately treated as untrusted test subject
    # code.  Its stdout/stderr go to a capped regular file in an isolated tree.
    resource.setrlimit(
        resource.RLIMIT_FSIZE,
        (MAX_SUBJECT_OUTPUT_BYTES, MAX_SUBJECT_OUTPUT_BYTES),
    )
    resource.setrlimit(resource.RLIMIT_CPU, (20, 20))


def _run_product_validator_subject(
    root: Path,
    arguments: Sequence[str],
    label: str,
) -> tuple[int, str]:
    log_path = root / f"{label}.log"
    command = [
        sys.executable,
        "-I",
        "-B",
        str(root / PRODUCT_VALIDATOR_PATH),
        *arguments,
    ]
    environment = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "LC_ALL": "C",
        "PYTHONDONTWRITEBYTECODE": "1",
    }
    try:
        with log_path.open("wb") as output:
            completed = subprocess.run(
                command,
                cwd=root,
                env=environment,
                stdin=subprocess.DEVNULL,
                stdout=output,
                stderr=subprocess.STDOUT,
                check=False,
                timeout=30,
                preexec_fn=_subject_resource_limits,
            )
    except (OSError, subprocess.TimeoutExpired) as exc:
        raise ArtifactError(f"product validator subject {label}: execution failed") from exc
    try:
        output_text = log_path.read_bytes()[:MAX_SUBJECT_OUTPUT_BYTES].decode(
            "utf-8", errors="replace"
        )
    except OSError as exc:
        raise ArtifactError(f"product validator subject {label}: output unreadable") from exc
    return completed.returncode, output_text


def _validate_product_validator_subject(
    reader: Reader,
    bundle: Mapping[str, Any],
) -> int:
    """Exercise, but never trust, the mutable product-side validator.

    This runs only after the protected artifact verdict and protected mutation
    selftests.  The admitted script is copied with its inputs to a temporary
    repository-shaped tree; no import or in-process call crosses the boundary.
    """

    validator_bytes = reader.read(PRODUCT_VALIDATOR_PATH, MAX_SMALL_JSON_BYTES)
    require(
        sha256(validator_bytes) == bundle["productValidatorSha256"],
        "product validator subject: source identity is not independently admitted",
    )
    subject_files = (
        PRODUCT_VALIDATOR_PATH,
        EVIDENCE_PATH,
        R1_ARCHIVE_PATH,
        R2_ARCHIVE_PATH,
        INVENTORY_PATH,
        COMBINED_PATH,
        PROJECTION_PATH,
        FIXTURE_SNAPSHOT_PATH,
        RETRIEVAL_FIXTURE_PATH,
        RETRIEVAL_CORPUS_PATH,
    )
    with tempfile.TemporaryDirectory(prefix="murmur-quality-validator-subject-") as temporary:
        root = Path(temporary)
        for relative in subject_files:
            target = root.joinpath(*Path(relative).parts)
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(reader.read(relative, MAX_LOGICAL_BYTES))

        valid_code, _ = _run_product_validator_subject(
            root,
            ("--final", "--selftest"),
            "valid",
        )
        require(valid_code == 0, "product validator subject: valid bundle was rejected")

        combined_path = root.joinpath(*Path(COMBINED_PATH).parts)
        combined_path.write_bytes(combined_path.read_bytes() + b" ")
        corrupt_code, corrupt_output = _run_product_validator_subject(
            root,
            ("--final",),
            "corrupt",
        )
        require(
            corrupt_code != 0,
            "product validator subject: corrupted combined artifact was accepted",
        )
        require(
            "final combined: SHA-256 differs" in corrupt_output,
            "product validator subject: corruption failed for the wrong reason",
        )
    return 2


def _json_pretty(value: Any) -> bytes:
    return (json.dumps(value, ensure_ascii=False, indent=2) + "\n").encode("utf-8")


def _json_compact(value: Any, *, sort_keys: bool = False) -> bytes:
    return (
        json.dumps(value, ensure_ascii=False, sort_keys=sort_keys, separators=(",", ":"))
        + "\n"
    ).encode("utf-8")


def _replace_evidence(files: dict[str, bytes], evidence: Mapping[str, Any]) -> None:
    files[EVIDENCE_PATH] = _json_pretty(evidence)


def _replace_report(
    files: dict[str, bytes],
    evidence: dict[str, Any],
    repetition: str,
    report: Mapping[str, Any],
) -> None:
    logical = _json_pretty(report)
    archive = deterministic_gzip(logical)
    archive_path = EXPECTED_REPETITION_PATHS[repetition]["archivePath"]
    files[archive_path] = archive
    evidence["repetitions"][repetition]["archiveSha256"] = sha256(archive)
    evidence["repetitions"][repetition]["logicalSha256"] = sha256(logical)
    _replace_evidence(files, evidence)


def _replace_inventory(
    files: dict[str, bytes],
    evidence: Mapping[str, Any],
    reports: Mapping[str, dict[str, Any]],
) -> None:
    logical = _json_pretty(_expected_inventory(reports, evidence))
    files[INVENTORY_PATH] = deterministic_gzip(logical)


class Selftests:
    def __init__(self) -> None:
        self.assertions = 0

    def fails(self, label: str, callback, expected: str) -> None:
        try:
            callback()
        except ArtifactError as exc:
            require(expected in str(exc), f"selftest {label}: wrong failure class")
            self.assertions += 1
            return
        raise ArtifactError(f"selftest {label}: mutation was accepted")

def _archive_selftests(base_files: Mapping[str, bytes]) -> int:
    tests = Selftests()

    files = dict(base_files)
    mutated = bytearray(files[R1_ARCHIVE_PATH])
    mutated[-1] ^= 1
    files[R1_ARCHIVE_PATH] = bytes(mutated)
    tests.fails(
        "archive hash",
        lambda: validate_archive_stage(MemoryReader(files)),
        "archive hash differs",
    )

    files = dict(base_files)
    evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
    logical = decode_gzip(files[R1_ARCHIVE_PATH], "selftest R1")
    archive = deterministic_gzip(logical, mtime=1)
    files[R1_ARCHIVE_PATH] = archive
    evidence["repetitions"]["1"]["archiveSha256"] = sha256(archive)
    _replace_evidence(files, evidence)
    tests.fails(
        "gzip mtime",
        lambda: validate_archive_stage(MemoryReader(files)),
        "not deterministic",
    )

    files = dict(base_files)
    evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
    archive = files[R1_ARCHIVE_PATH] + b"trailing"
    files[R1_ARCHIVE_PATH] = archive
    evidence["repetitions"]["1"]["archiveSha256"] = sha256(archive)
    _replace_evidence(files, evidence)
    tests.fails(
        "gzip trailing bytes",
        lambda: validate_archive_stage(MemoryReader(files)),
        "trailing or concatenated",
    )

    privacy_mutations = (
        ("email PII", "synthetic@example.com", "email address"),
        ("phone PII", "+1 (415) 555-2671", "phone number"),
        ("card PII", "4111 1111 1111 1111", "payment card"),
        ("AWS access key", "AKIAIOSFODNN7EXAMPLE", "AWS access key"),
        (
            "JWT-like token",
            "eyJabcdefgh.abcdefgh.abcdefgh",
            "JWT-like token",
        ),
        (
            "encoded audio",
            "data:audio/wav;base64,UklGRgAAAAA=",
            "encoded binary or audio",
        ),
    )
    for label, marker, expected in privacy_mutations:
        files = dict(base_files)
        evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
        report = load_object(decode_gzip(files[R1_ARCHIVE_PATH], "selftest R1"), "selftest R1")
        report["runLabel"] = marker
        _replace_report(files, evidence, "1", report)
        tests.fails(
            label,
            lambda files=files: validate_archive_stage(MemoryReader(files)),
            expected,
        )

    files = dict(base_files)
    evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
    reports = {
        repetition: load_object(
            decode_gzip(files[path], f"selftest R{repetition}"),
            f"selftest R{repetition}",
        )
        for repetition, path in (("1", R1_ARCHIVE_PATH), ("2", R2_ARCHIVE_PATH))
    }
    reports["1"]["arms"][0]["cases"][0]["score"]["diagnosticScore"] = 999999
    _replace_report(files, evidence, "1", reports["1"])
    _replace_inventory(files, evidence, reports)
    tests.fails(
        "out-of-range nested score with coherent hashes",
        lambda: validate_archive_stage(MemoryReader(files)),
        "diagnostic score out of range",
    )

    files = dict(base_files)
    evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
    reports = {
        repetition: load_object(
            decode_gzip(files[path], f"selftest R{repetition}"),
            f"selftest R{repetition}",
        )
        for repetition, path in (("1", R1_ARCHIVE_PATH), ("2", R2_ARCHIVE_PATH))
    }
    reports["1"]["arms"][0]["cases"][0]["undeclaredNestedField"] = True
    _replace_report(files, evidence, "1", reports["1"])
    _replace_inventory(files, evidence, reports)
    tests.fails(
        "undeclared nested case field with coherent hashes",
        lambda: validate_archive_stage(MemoryReader(files)),
        "product case schema differs",
    )

    files = dict(base_files)
    evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
    reports = {
        repetition: load_object(
            decode_gzip(files[path], f"selftest R{repetition}"),
            f"selftest R{repetition}",
        )
        for repetition, path in (("1", R1_ARCHIVE_PATH), ("2", R2_ARCHIVE_PATH))
    }
    reports["1"]["arms"][0]["aggregates"]["all_eligible"][
        "undeclaredNestedField"
    ] = True
    _replace_report(files, evidence, "1", reports["1"])
    _replace_inventory(files, evidence, reports)
    tests.fails(
        "undeclared nested aggregate field with coherent hashes",
        lambda: validate_archive_stage(MemoryReader(files)),
        "closed nested schema commitment differs",
    )

    files = dict(base_files)
    evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
    reports = {
        repetition: load_object(
            decode_gzip(files[path], f"selftest R{repetition}"),
            f"selftest R{repetition}",
        )
        for repetition, path in (("1", R1_ARCHIVE_PATH), ("2", R2_ARCHIVE_PATH))
    }
    evidence["producerSnapshot"]["repositoryCommit"] = "0" * 40
    for repetition in ("1", "2"):
        report = reports[repetition]
        report["repositoryCommit"] = "0" * 40
        report["snapshotStart"]["repositoryCommit"] = "0" * 40
        report["snapshotEnd"]["repositoryCommit"] = "0" * 40
        _replace_report(files, evidence, repetition, report)
    _replace_inventory(files, evidence, reports)
    tests.fails(
        "zero producer commit with coherent propagation",
        lambda: validate_archive_stage(MemoryReader(files)),
        "invalid producer commit",
    )

    files = dict(base_files)
    evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
    reports = {
        repetition: load_object(
            decode_gzip(files[path], f"selftest R{repetition}"),
            f"selftest R{repetition}",
        )
        for repetition, path in (("1", R1_ARCHIVE_PATH), ("2", R2_ARCHIVE_PATH))
    }
    zero_fields = (
        "sourceFingerprintSha256",
        "evaluatorFileSha256",
        "repeatValidatorFileSha256",
    )
    for field in zero_fields:
        evidence["producerSnapshot"][field] = "0" * 64
    for repetition in ("1", "2"):
        report = reports[repetition]
        report["sourceFingerprintSha256"] = "0" * 64
        for snapshot_name in ("snapshotStart", "snapshotEnd"):
            for field in zero_fields:
                report[snapshot_name][field] = "0" * 64
        _replace_report(files, evidence, repetition, report)
    _replace_inventory(files, evidence, reports)
    tests.fails(
        "zero producer digests with coherent propagation",
        lambda: validate_archive_stage(MemoryReader(files)),
        "invalid producer",
    )

    files = dict(base_files)
    evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
    reports = {
        repetition: load_object(
            decode_gzip(files[path], f"selftest R{repetition}"),
            f"selftest R{repetition}",
        )
        for repetition, path in (("1", R1_ARCHIVE_PATH), ("2", R2_ARCHIVE_PATH))
    }
    reports["1"]["arms"][0]["cases"][0]["caseRecordSha256"] = "0" * 64
    _replace_report(files, evidence, "1", reports["1"])
    _replace_inventory(files, evidence, reports)
    tests.fails(
        "zero nested digest with coherent hashes",
        lambda: validate_archive_stage(MemoryReader(files)),
        "invalid SHA-256 binding",
    )

    files = dict(base_files)
    evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
    reports = {
        repetition: load_object(
            decode_gzip(files[path], f"selftest R{repetition}"),
            f"selftest R{repetition}",
        )
        for repetition, path in (("1", R1_ARCHIVE_PATH), ("2", R2_ARCHIVE_PATH))
    }
    reports["1"]["arms"][0]["cases"][0]["output"] = "synthetic replacement output"
    _replace_report(files, evidence, "1", reports["1"])
    _replace_inventory(files, evidence, reports)
    tests.fails(
        "stale output hash with coherent outer hashes",
        lambda: validate_archive_stage(MemoryReader(files)),
        "output length/hash differs from output",
    )

    files = dict(base_files)
    evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
    reports = {
        repetition: load_object(
            decode_gzip(files[path], f"selftest R{repetition}"),
            f"selftest R{repetition}",
        )
        for repetition, path in (("1", R1_ARCHIVE_PATH), ("2", R2_ARCHIVE_PATH))
    }
    case = reports["1"]["arms"][0]["cases"][0]
    replacement = "synthetic replacement output"
    case["output"] = replacement
    case["outputChars"] = len(replacement)
    case["outputSha256"] = sha256(replacement.encode("utf-8"))
    case["caseRecordSha256"] = "ab" * 32
    case["score"]["diagnosticScore"] = 100.0
    case["score"]["casePass"] = True
    case["score"]["criticalFailure"] = False
    case["score"]["criticalErrors"] = []
    case["dimensions"]["finalProductOutputContract"] = "pass"
    _replace_report(files, evidence, "1", reports["1"])
    _replace_inventory(files, evidence, reports)
    tests.fails(
        "reviewer fabricated perfect case",
        lambda: validate_archive_stage(MemoryReader(files)),
        "output length/hash differs from output",
    )

    files = dict(base_files)
    evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
    reports = {
        repetition: load_object(
            decode_gzip(files[path], f"selftest R{repetition}"),
            f"selftest R{repetition}",
        )
        for repetition, path in (("1", R1_ARCHIVE_PATH), ("2", R2_ARCHIVE_PATH))
    }
    case = reports["1"]["arms"][0]["cases"][0]
    replacement = "synthetic replacement output"
    case["output"] = replacement
    case["outputChars"] = len(replacement)
    case["outputSha256"] = framed_hash([replacement])
    case["caseRecordSha256"] = "ab" * 32
    _replace_report(files, evidence, "1", reports["1"])
    _replace_inventory(files, evidence, reports)
    tests.fails(
        "arbitrary case-record commitment after valid output hash",
        lambda: validate_archive_stage(MemoryReader(files)),
        "case-record commitment differs",
    )

    files = dict(base_files)
    evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
    reports = {
        repetition: load_object(
            decode_gzip(files[path], f"selftest R{repetition}"),
            f"selftest R{repetition}",
        )
        for repetition, path in (("1", R1_ARCHIVE_PATH), ("2", R2_ARCHIVE_PATH))
    }
    reports["1"]["arms"][0]["cases"][0]["rawModelOutput"] = "stale raw output"
    _replace_report(files, evidence, "1", reports["1"])
    _replace_inventory(files, evidence, reports)
    tests.fails(
        "stale raw-model output hash",
        lambda: validate_archive_stage(MemoryReader(files)),
        "rawModelOutputSha256 differs from rawModelOutput",
    )

    files = dict(base_files)
    evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
    reports = {
        repetition: load_object(
            decode_gzip(files[path], f"selftest R{repetition}"),
            f"selftest R{repetition}",
        )
        for repetition, path in (("1", R1_ARCHIVE_PATH), ("2", R2_ARCHIVE_PATH))
    }
    case = reports["1"]["arms"][0]["cases"][0]
    case["score"]["casePass"] = False
    case["caseRecordSha256"] = _ordered_json_hash(_product_case_record_values(case))
    _replace_report(files, evidence, "1", reports["1"])
    _replace_inventory(files, evidence, reports)
    tests.fails(
        "contradictory case-pass verdict",
        lambda: validate_archive_stage(MemoryReader(files)),
        "case-pass verdict does not replay",
    )

    files = dict(base_files)
    evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
    reports = {
        repetition: load_object(
            decode_gzip(files[path], f"selftest R{repetition}"),
            f"selftest R{repetition}",
        )
        for repetition, path in (("1", R1_ARCHIVE_PATH), ("2", R2_ARCHIVE_PATH))
    }
    case = reports["1"]["arms"][0]["cases"][0]
    case["score"]["criticalFailure"] = True
    case["caseRecordSha256"] = _ordered_json_hash(_product_case_record_values(case))
    _replace_report(files, evidence, "1", reports["1"])
    _replace_inventory(files, evidence, reports)
    tests.fails(
        "contradictory critical verdict",
        lambda: validate_archive_stage(MemoryReader(files)),
        "critical verdict differs from critical errors",
    )

    files = dict(base_files)
    evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
    reports = {
        repetition: load_object(
            decode_gzip(files[path], f"selftest R{repetition}"),
            f"selftest R{repetition}",
        )
        for repetition, path in (("1", R1_ARCHIVE_PATH), ("2", R2_ARCHIVE_PATH))
    }
    case = reports["1"]["arms"][0]["cases"][0]
    case["dimensions"]["finalProductOutputContract"] = "fail"
    case["caseRecordSha256"] = _ordered_json_hash(_product_case_record_values(case))
    _replace_report(files, evidence, "1", reports["1"])
    _replace_inventory(files, evidence, reports)
    tests.fails(
        "contradictory dimension verdict",
        lambda: validate_archive_stage(MemoryReader(files)),
        "dimensions do not replay from case evidence",
    )

    files = dict(base_files)
    evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
    reports = {
        repetition: load_object(
            decode_gzip(files[path], f"selftest R{repetition}"),
            f"selftest R{repetition}",
        )
        for repetition, path in (("1", R1_ARCHIVE_PATH), ("2", R2_ARCHIVE_PATH))
    }
    case = reports["1"]["arms"][0]["cases"][0]
    replacement = "synthetic replacement output"
    case["output"] = replacement
    case["outputChars"] = len(replacement)
    case["outputSha256"] = framed_hash([replacement])
    case["caseRecordSha256"] = _ordered_json_hash(_product_case_record_values(case))
    _replace_report(files, evidence, "1", reports["1"])
    _replace_inventory(files, evidence, reports)
    tests.fails(
        "internally coherent but unadmitted output and score content",
        lambda: validate_archive_stage(MemoryReader(files)),
        "evidence/artifact commitments are not atomically admitted",
    )

    files = dict(base_files)
    evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
    reports = {
        repetition: load_object(
            decode_gzip(files[path], f"selftest R{repetition}"),
            f"selftest R{repetition}",
        )
        for repetition, path in (("1", R1_ARCHIVE_PATH), ("2", R2_ARCHIVE_PATH))
    }
    current_bundle = _resolve_admitted_bundle(
        evidence["producerSnapshot"], evidence["runtimeIdentities"]
    )
    other_bundle = next(
        bundle for bundle in ADMITTED_BUNDLES if bundle["id"] != current_bundle["id"]
    )
    swapped_local = copy.deepcopy(other_bundle["runtimeIdentities"]["local"])
    evidence["runtimeIdentities"]["local"] = swapped_local
    for repetition in ("1", "2"):
        for arm in reports[repetition]["arms"]:
            if arm["metadata"]["armId"] != ARM_SOL:
                arm["metadata"]["runtimeVersion"] = swapped_local["version"]
                arm["metadata"]["runtimeSha256"] = swapped_local["sha256"]
        _replace_report(files, evidence, repetition, reports[repetition])
    _replace_inventory(files, evidence, reports)
    tests.fails(
        "cross-product producer and runtime",
        lambda: validate_archive_stage(MemoryReader(files)),
        "producer/runtime combination is not atomically admitted",
    )

    files = dict(base_files)
    evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
    report = load_object(decode_gzip(files[R1_ARCHIVE_PATH], "selftest R1"), "selftest R1")
    report["arms"][0], report["arms"][1] = report["arms"][1], report["arms"][0]
    _replace_report(files, evidence, "1", report)
    tests.fails(
        "arm order",
        lambda: validate_archive_stage(MemoryReader(files)),
        "arm order differs",
    )

    files = dict(base_files)
    evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
    report = load_object(decode_gzip(files[R1_ARCHIVE_PATH], "selftest R1"), "selftest R1")
    report["snapshotEnd"]["sourceFingerprintSha256"] = "0" * 64
    _replace_report(files, evidence, "1", report)
    tests.fails(
        "source binding",
        lambda: validate_archive_stage(MemoryReader(files)),
        "start/end snapshots differ",
    )

    files = dict(base_files)
    inventory = load_object(decode_gzip(files[INVENTORY_PATH], "selftest inventory"), "selftest inventory")
    inventory["uniqueStringCount"] += 1
    files[INVENTORY_PATH] = deterministic_gzip(_json_pretty(inventory))
    tests.fails(
        "inventory mutation",
        lambda: validate_archive_stage(MemoryReader(files)),
        "decoded content differs",
    )

    files = dict(base_files)
    evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
    reports = {
        repetition: load_object(
            decode_gzip(files[path], f"selftest R{repetition}"),
            f"selftest R{repetition}",
        )
        for repetition, path in (("1", R1_ARCHIVE_PATH), ("2", R2_ARCHIVE_PATH))
    }
    reports["1"]["generatedAt"] = "2099-01-01T00:00:00+00:00"
    _replace_report(files, evidence, "1", reports["1"])
    _replace_inventory(files, evidence, reports)
    tests.fails(
        "unadmitted report evolution",
        lambda: validate_archive_stage(MemoryReader(files)),
        "evidence/artifact commitments are not atomically admitted",
    )

    archive_reader = MemoryReader(dict(base_files))
    validate_archive_stage(archive_reader)
    forbidden_reads = {COMBINED_PATH, PROJECTION_PATH, PRODUCT_VALIDATOR_PATH}
    require(
        not forbidden_reads.intersection(archive_reader.reads),
        "selftest archive isolation: final artifacts were read",
    )
    tests.assertions += 1
    return tests.assertions


def _final_selftests(base_files: Mapping[str, bytes]) -> int:
    tests = Selftests()

    files = dict(base_files)
    mutated = bytearray(files[COMBINED_PATH])
    mutated[-2] ^= 1
    files[COMBINED_PATH] = bytes(mutated)
    tests.fails(
        "combined hash",
        lambda: validate_final(MemoryReader(files)),
        "file hash differs",
    )

    files = dict(base_files)
    evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
    combined = load_object(files[COMBINED_PATH], "selftest combined")
    combined["inputResultSha256ByRepetition"]["1"] = "0" * 64
    combined_bytes = _json_compact(combined)
    files[COMBINED_PATH] = combined_bytes
    evidence["combined"]["sha256"] = sha256(combined_bytes)
    _replace_evidence(files, evidence)
    tests.fails(
        "combined repetition binding",
        lambda: validate_final(MemoryReader(files)),
        "evidence/artifact commitments are not atomically admitted",
    )

    files = dict(base_files)
    evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
    combined = load_object(files[COMBINED_PATH], "selftest combined")
    combined["sourceFingerprintSha256"] = "0" * 64
    combined_bytes = _json_compact(combined)
    files[COMBINED_PATH] = combined_bytes
    evidence["combined"]["sha256"] = sha256(combined_bytes)
    _replace_evidence(files, evidence)
    tests.fails(
        "combined producer binding",
        lambda: validate_final(MemoryReader(files)),
        "evidence/artifact commitments are not atomically admitted",
    )

    files = dict(base_files)
    projection = load_object(files[PROJECTION_PATH], "selftest projection")
    projection["artifactBindings"]["evidence"]["sha256"] = "0" * 64
    projection["artifactBindingsCanonicalSha256"] = canonical_sha256(
        projection["artifactBindings"]
    )
    files[PROJECTION_PATH] = _json_compact(projection, sort_keys=True)
    tests.fails(
        "projection artifact binding",
        lambda: validate_final(MemoryReader(files)),
        "invalid SHA-256 binding",
    )

    files = dict(base_files)
    projection = load_object(files[PROJECTION_PATH], "selftest projection")
    projection["repetitions"][0]["runLabel"] = "mutated-run"
    files[PROJECTION_PATH] = _json_compact(projection, sort_keys=True)
    tests.fails(
        "projection repetition binding",
        lambda: validate_final(MemoryReader(files)),
        "runLabel differs",
    )

    files = dict(base_files)
    projection = load_object(files[PROJECTION_PATH], "selftest projection")
    projection["scope"]["evidenceScope"] = "synthetic@example.com"
    files[PROJECTION_PATH] = _json_compact(projection, sort_keys=True)
    tests.fails(
        "projection privacy",
        lambda: validate_final(MemoryReader(files)),
        "email address",
    )

    files = dict(base_files)
    projection = load_object(files[PROJECTION_PATH], "selftest projection")
    files[PROJECTION_PATH] = _json_pretty(projection)
    tests.fails(
        "projection compact encoding",
        lambda: validate_final(MemoryReader(files)),
        "not canonical compact",
    )

    files = dict(base_files)
    projection = load_object(files[PROJECTION_PATH], "selftest projection")
    projection["productRoute"]["arms"][0]["overall"]["casePassRate"] = 999
    files[PROJECTION_PATH] = _json_compact(projection, sort_keys=True)
    tests.fails(
        "projection out-of-range quality claim",
        lambda: validate_final(MemoryReader(files)),
        "percentage quality value out of range",
    )

    files = dict(base_files)
    projection = load_object(files[PROJECTION_PATH], "selftest projection")
    projection["retrievalQuality"]["unboundClaim"] = "synthetic-but-unbound"
    files[PROJECTION_PATH] = _json_compact(projection, sort_keys=True)
    tests.fails(
        "projection unbound retrieval claim",
        lambda: validate_final(MemoryReader(files)),
        "independently derived content differs",
    )

    files = dict(base_files)
    evidence = copy.deepcopy(load_object(files[EVIDENCE_PATH], "selftest evidence"))
    combined = load_object(files[COMBINED_PATH], "selftest combined")
    combined["retrievalQuality"]["unboundClaim"] = "synthetic-but-unbound"
    combined_bytes = _json_compact(combined)
    files[COMBINED_PATH] = combined_bytes
    evidence["combined"]["sha256"] = sha256(combined_bytes)
    _replace_evidence(files, evidence)
    tests.fails(
        "combined unbound retrieval claim",
        lambda: validate_final(MemoryReader(files)),
        "evidence/artifact commitments are not atomically admitted",
    )

    return tests.assertions


def main(argv: Sequence[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--archive-stage", action="store_true")
    mode.add_argument("--final", action="store_true")
    parser.add_argument("--selftest", action="store_true")
    args = parser.parse_args(argv)
    try:
        reader = DiskReader()
        if args.archive_stage:
            validate_archive_stage(reader)
            assertions = _archive_selftests(reader.cache) if args.selftest else 0
            suffix = f" ({assertions} selftests)" if args.selftest else ""
            print(f"quality-artifacts archive-stage: PASS{suffix}")
            return 0

        final_state = validate_final(reader)
        assertions = 0
        if args.selftest:
            assertions += _archive_selftests(reader.cache)
            assertions += _final_selftests(reader.cache)
        subject_assertions = _validate_product_validator_subject(
            reader, final_state.archive.bundle
        )
        if args.selftest:
            assertions += subject_assertions
        suffix = f" ({assertions} selftests)" if args.selftest else ""
        print(f"quality-artifacts final: PASS{suffix}")
        return 0
    except ArtifactError as exc:
        print(f"quality-artifacts: FAIL: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
