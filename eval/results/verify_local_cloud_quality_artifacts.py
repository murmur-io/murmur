#!/usr/bin/env python3
"""Offline integrity/privacy oracle for committed synthetic quality artifacts."""

from __future__ import annotations

import argparse
import base64
import copy
import gzip
import hashlib
import json
import lzma
import re
import zlib
from pathlib import Path
from typing import Any, Callable


REPO_ROOT = Path(__file__).resolve().parents[2]
RESULTS_PREFIX = Path("eval/results")
FINAL_EVIDENCE = RESULTS_PREFIX / "2026-08-05-qwen-vs-gpt-sol-evidence.json"
FINAL_FIXTURE_SNAPSHOT = RESULTS_PREFIX / "2026-08-05-local-cloud-quality-fixture.json"
FINAL_CONTENT_INVENTORY = (
    RESULTS_PREFIX / "2026-08-05-qwen-vs-gpt-sol-final-content-inventory.json"
)
RETRIEVAL_FIXTURE_SOURCE = Path(
    "src-tauri/src/eval/fixtures/rag-bakeoff-synthetic.json"
)
RETRIEVAL_CORPUS_SOURCE = Path("src-tauri/src/eval/corpus.rs")
BASELINE_BUNDLE = RESULTS_PREFIX / "2026-08-05-qwen-vs-gpt-sol-baseline-raw.json.xz"
DECISION_BUNDLE = RESULTS_PREFIX / "2026-08-05-qwen-vs-gpt-sol-decision-raw.json.xz"
HISTORY_CONTENT_INVENTORY = (
    RESULTS_PREFIX / "2026-08-05-qwen-vs-gpt-sol-history-content-inventory.json"
)

FINAL_EVIDENCE_SHA256 = "73eb8325bf536dc0e5c47ad739502b853fb37722c82f362b080884dac9de6c44"
FINAL_FIXTURE_SHA256 = "b5f63efbc135a8629366614444bdba8d9501e28209d054e967b8e9debeddd9b2"
FINAL_CONTENT_INVENTORY_SHA256 = (
    "5ce634a6bff61e48fdbcec19b2c08bf91f3aaae29c78d8028ac79d8679afac28"
)
BASELINE_BUNDLE_SHA256 = "05f248da2a6e1104b9311d2fd21422c9dc2af10c1cf0492472812295509f2186"
DECISION_BUNDLE_SHA256 = "18521020093fe4c3f1d39027e319fd4e9dea8ee9758464c48b7e0bb81349a5ed"
DECISION_COMBINED_SHA256 = "a024c257ae79939a61879c54db02c3f845d2c46286b567fff443d925f00c72d5"
BASELINE_RESCORE_SHA256 = {
    "r1": "259b709e720183d12aaba4f02b08d817e7a42c83fa2369eb89f457726b60c672",
    "r2": "ece5e953fd3ad45fe74a713948f7504f7ec30c26f677e3bbe6671d3e3ca1a101",
}
HISTORY_MEMBER_HASHES = {
    "baseline/r1": (
        "9b1cbf49733995f3469463ee40cf03993eadb49042d5dcd748692b394d807338",
        "9a87f49f7cd00601c4b60ff005d0b91e0e810c4bf47c03fdc7454781266e405c",
    ),
    "baseline/r2": (
        "bcddce863367cdbe08e62ec9345b94700bd5a7b8ebf1c3a67a8b848c7931bf7e",
        "bd63cbb603710234ae0dc2871e6f6fd0013d8ba44ab2c191738fb480fcc93239",
    ),
    "decision/r1": (
        "451e7ccb431d4e972c4c4afa10936bd4b3b837602e4a9866d7aaf0b70e2147d3",
        "98fa05e44040844180ceeeab69435b1845571fc072667438b48b70c5957ff205",
    ),
    "decision/r2": (
        "c244bfa9705c90a26a9ebaee285c062603e60108124896566e1bd0432958c5c5",
        "be61472521be4e3eccef681b1b7f0f31fa48c8c7efe2ff5575b4a77a4c0e0203",
    ),
}
HISTORY_CONTENT_INVENTORY_SHA256 = (
    "e501da20b98123b82e2c38ee74a6d22a553b4ccc4594eccd8ae69890d071c5a2"
)
HISTORY_SCHEMA_SHA256 = {
    "baseline/r1": "8809cb885cfc6f849271464111d4258274ae537893fde109306b45bde6509e08",
    "baseline/r2": "75b86d906d5d88eecb2d9ddb8112d50471ee506bae67abb191729b4dbf00e2ce",
    "decision/r1": "799795d0cf954e961340508eb135fef770f3de1c852d4e8728db28544b858754",
    "decision/r2": "a6ba467b726477bab4200cc4844710655affe9077dbdb3797627ae04a3729b28",
}
HISTORY_TEXT_SHA256 = {
    "baseline/r1": "f01fe02944b657d98a17a414b40e9424e048482d21615e96e577cbbbb12ad7dd",
    "baseline/r2": "6eff56d30a7975851223e7431a79b4a5727186e9c7a84bd445fad6ec38d2ed61",
    "decision/r1": "a790690d380590583ee334090937dd8b25b1eca38f870c5a6c78cdaa702a42ff",
    "decision/r2": "0663940fca79b7c33ea0b7154114745519a248749a95662e7d36cf5e061d5a60",
}
MAX_GZIP_ARCHIVE_BYTES = 2 * 1024 * 1024
MAX_GZIP_LOGICAL_BYTES = 8 * 1024 * 1024
FINAL_REPETITIONS = {
    "1": {
        "archivePath": "eval/results/2026-08-05-qwen-vs-gpt-sol-final-r1.json.gz",
        "archiveSha256": "fc7c70eb07595b8ae768a9a53d2d8e6d12544b086a5d4173540acb873618fb2f",
        "logicalPath": "eval/results/2026-08-05-qwen-vs-gpt-sol-final-r1.json",
        "logicalSha256": "3308ec93608ca21a190e6bf14f0082d98f8cde545e1c9f8fe017f9c2f07bac51",
    },
    "2": {
        "archivePath": "eval/results/2026-08-05-qwen-vs-gpt-sol-final-r2.json.gz",
        "archiveSha256": "0142628e94dd3756cbad3d2d067def02b19b830e95107c7be82dbcc3344aa6cf",
        "logicalPath": "eval/results/2026-08-05-qwen-vs-gpt-sol-final-r2.json",
        "logicalSha256": "393ddee23d7bdff7604a2333f0303d3fea8c398769e1732519d4bfc479973d64",
    },
}
FINAL_COMBINED = {
    "path": "eval/results/2026-08-05-qwen-vs-gpt-sol-final-combined.json",
    "sha256": "6db53789cc979743ab6deaf74a0aba093c94df2825abef32ea93cb801effd3cc",
}
FINAL_CASE_IDS = {
    "ask-vault-en-quartz-holdout",
    "ask-vault-pl-orchid",
    "fact-extract-en-helix",
    "fact-extract-pl-zuraw",
    "live-bullets-pl-polaris",
    "live-current-en-nimbus",
    "live-current-pl-ember-holdout",
    "meeting-chat-en-fjord-holdout",
    "meeting-chat-pl-delta",
    "note-popup-actions-en-holdout",
    "note-popup-actions-pl",
    "note-popup-decisions-en",
    "note-popup-fact-check-pl",
    "note-popup-refine-pl",
    "note-popup-shorten-en",
    "summary-en-cedar",
    "summary-pl-kestrel",
    "summary-pl-lumen-holdout",
} | {f"retrieval-{index:02d}" for index in range(1, 21)}
BASELINE_CASE_LANGUAGES = {
    "ask-vault-en-quartz-holdout": "en",
    "ask-vault-pl-orchid": "pl",
    "live-bullets-pl-polaris": "pl",
    "live-current-en-nimbus": "en",
    "live-current-pl-ember-holdout": "pl",
    "meeting-chat-en-fjord-holdout": "en",
    "meeting-chat-pl-delta": "pl",
    "note-popup-actions-en-holdout": "en",
    "note-popup-actions-pl": "pl",
    "note-popup-decisions-en": "en",
    "note-popup-fact-check-pl": "pl",
    "note-popup-refine-pl": "pl",
    "note-popup-shorten-en": "en",
    "summary-en-cedar": "en",
    "summary-pl-kestrel": "pl",
    "summary-pl-lumen-holdout": "pl",
}

# These are commitments to the complete, invented fixture payloads, not hashes
# of model output.  Keeping the language and synthetic entity inventory beside
# each digest makes accidental real-data substitution reviewable without
# needing to trust a filename or syntheticOnly boolean.
GENERATION_FIXTURE_ORACLE = {
    "ask-vault-en-quartz-holdout": (
        "en", "c386d4efd9b02ce70125c7d9faf0ed7d882e3818f1eb54c61ad10df46d4cb87e", ("Theo",)
    ),
    "ask-vault-pl-orchid": (
        "pl", "2d2bbbabf56fd539aa0671c28701ee93766451a72bf71e317817b44eaa0ac54c", ("Iga",)
    ),
    "fact-extract-en-helix": (
        "en", "2413a1f7ca91c3b9af80cea77a2ebbd1af7c20b5ff2879f89fdcad8c0ffc4613", ("Mara Voss",)
    ),
    "fact-extract-pl-zuraw": (
        "pl", "53f690e0698ee90749feab99f00df6f4eb2e3c57d01fe3c301dc225dd99f57a2", ("Łucja Borek",)
    ),
    "live-bullets-pl-polaris": (
        "pl", "f2f995c70eae9a751ac9b77669f613a40426f3689dd38da9f90dc829c0ff5a70", ("Lena",)
    ),
    "live-current-en-nimbus": (
        "en", "c3f3aeab3986e2b586411384c539a9cdd953a64c546e67fac7b3c97e1a77afab", ("Omar",)
    ),
    "live-current-pl-ember-holdout": (
        "pl", "6833bca67931d28ab0a918fb290b84ae85d123a1413c922b0ecf60b0333407f4", ("Bartek",)
    ),
    "meeting-chat-en-fjord-holdout": (
        "en", "f7f48aac48a9afaeb971a4277e2992c7235e95d8a6bad2626583fb1a2c3a286d", ("Mei",)
    ),
    "meeting-chat-pl-delta": (
        "pl", "01f3fc5fa1ede4792fe55da22138d1ed62ed51758f09752f332204f5cbfac225", ("Nina",)
    ),
    "note-popup-actions-en-holdout": (
        "en", "3f041c4d7ef4d5155bfba5b3371bb689fcfbfcbcb333a26eb6e67b3b7bd306a7", ("Leah",)
    ),
    "note-popup-actions-pl": (
        "pl", "7bc6acf7f6df4a3d33759f5850f5536ca063aa2857b856dbeb8534e15e214050", ("Iga",)
    ),
    "note-popup-decisions-en": (
        "en", "a9ef9a1ceeb2351ae06840b427c243c3e650793f36cfbc9ec15e9356a639dc23", ("Morgan",)
    ),
    "note-popup-fact-check-pl": (
        "pl", "35fefc43069fa96489f5a1e54ee04a7807490a0d58d8d6c3257622ef9f012113", ()
    ),
    "note-popup-refine-pl": (
        "pl", "683b8c53f708c5333c02d6591cdd73fe407c064b67fd579196e2bcdb338a5637", ("Alicja",)
    ),
    "note-popup-shorten-en": (
        "en", "44f12c1d93908110576d7fda6956f2c19925ce8037a43e4ba30a5ec90a2276f0", ("Morgan",)
    ),
    "summary-en-cedar": (
        "en", "99d8ea16d73192e6936c1c023c9b24d6b07ab02be80298ea7a6653fdc6773dff", ("Rowan",)
    ),
    "summary-pl-kestrel": (
        "pl", "f05845b1d49b01e0b3b6b186120e6c58fc69ff756247524a6268437e86e2b402", ("Piotr", "Marta")
    ),
    "summary-pl-lumen-holdout": (
        "pl", "54a1686bdf51836b50590d3d794866ec1d8f55a07942e70a823cba526ef15e66", ("Sara",)
    ),
}
RETRIEVAL_FIXTURE_ORACLE = {
    "retrieval-01": ("pl", "d00ae16b6ba3c87b2dd3e0decbeff8ad7209fe4aeb656588b7fcec7a2c469d38"),
    "retrieval-02": ("pl", "7ee2d0863720e3c6018109824faeca84bae0cd96cab2308b96138527cce9621b"),
    "retrieval-03": ("pl", "a789381e2cdac3b0a08a0dc4fbc1a37b70b7afb5b39d2eca29d081b6e70be7e0"),
    "retrieval-04": ("en", "8389f5c322dba56a0600596560b9d6cf33cad19abc656f83ef9f2270571a382d"),
    "retrieval-05": ("pl", "4ea320689acbda3bffb8111de8f0fd528a6ffee7c81ecfa6138b0873f84d89e6"),
    "retrieval-06": ("pl", "e8a4b5c5aa6a3e43547238ee7721850c0ef61fb7bc7095d59847085b500b7981"),
    "retrieval-07": ("pl", "5d5181392fb1fcc5ca7b98efa7c2202169f45d3cd4b28fae8a15827122264557"),
    "retrieval-08": ("en", "1f5268d1bd80919a20a8da6b98f71ba799dca451ed907c22f4a5e9eb3abc8d05"),
    "retrieval-09": ("pl", "ece9a761e97e0c641ff177413495a9202832ebd6723708a800c3a17bdf4f6879"),
    "retrieval-10": ("pl", "5658233ecaa0b26078d3e2e4afcd59f514a3dd673e88c20213401acdbc439157"),
    "retrieval-11": ("en", "88d2e87d399ef12d30d58ee0c4b07ecf666dcb0aa2cc1ec2bc1d8f0a656938d8"),
    "retrieval-12": ("en", "c1f36314c8703b59404353c4596887ac838b4057963987b0dbd24f3267169115"),
    "retrieval-13": ("pl", "b28349655601f2b5a569fad0899e187ff9fd91cbb86974aa99066ef382c07604"),
    "retrieval-14": ("pl", "492d0e0bbc0f3203509a6f3a8c4a7f32bff643f66a9eb74f3eeb453d07abb03b"),
    "retrieval-15": ("pl", "fbc3c4f24a5d13f3f41ac91b95624f1b8ba9453a297a3d502615b5c24b95743a"),
    "retrieval-16": ("en", "e74de60e15f7a568de7b7de75afbe34c53307cc6fa67dfda47af06d73befef4c"),
    "retrieval-17": ("pl", "a9d494cd5158ae77fabeee28270b65ff127513d8647749fd9240001e3f72be30"),
    "retrieval-18": ("en", "102e780c10fcb1034a7a6159cdcc232aebaae4a92faa62c8d7173ba895cc7058"),
    "retrieval-19": ("pl", "71985bcd8940834bc42c20c345d88c70262ffca76ea44d697de655b4ceb3a8c7"),
    "retrieval-20": ("en", "ea623c2d48fc723bcf46d036f5b9fdf7aeb2dcdee29ca2effedb9cb00934d0ee"),
}
GENERATION_CASE_METADATA = {
    "ask-vault-en-quartz-holdout": ("ask_vault", True),
    "ask-vault-pl-orchid": ("ask_vault", False),
    "fact-extract-en-helix": ("light_extraction", False),
    "fact-extract-pl-zuraw": ("light_extraction", False),
    "live-bullets-pl-polaris": ("live_bullets", False),
    "live-current-en-nimbus": ("live_current", False),
    "live-current-pl-ember-holdout": ("live_current", True),
    "meeting-chat-en-fjord-holdout": ("meeting_chat", True),
    "meeting-chat-pl-delta": ("meeting_chat", False),
    "note-popup-actions-en-holdout": ("note_assist", True),
    "note-popup-actions-pl": ("note_assist", False),
    "note-popup-decisions-en": ("note_assist", False),
    "note-popup-fact-check-pl": ("note_assist", False),
    "note-popup-refine-pl": ("note_assist", False),
    "note-popup-shorten-en": ("note_assist", False),
    "summary-en-cedar": ("summary", False),
    "summary-pl-kestrel": ("summary", False),
    "summary-pl-lumen-holdout": ("summary", True),
}

FINAL_REPORT_TOP_LEVEL_KEYS = {
    "arms", "benchmarkDesign", "egressLedger", "environment", "evidenceLimits",
    "evidenceScope", "generatedAt", "holdoutInterpretation", "localComposite",
    "manifestSha256", "pairedComparison", "promptVersion", "repositoryCommit",
    "retrievalLane", "retrievalQuality", "runLabel", "sameCallerEnvelopeModelStack",
    "schemaVersion", "snapshotEnd", "snapshotStart", "sourceFingerprintSha256",
    "syntheticOnly",
}
FINAL_COMBINED_TOP_LEVEL_KEYS = {
    "arms", "comparisonType", "design", "dimensionAttribution", "holdoutInterpretation",
    "inputResultSha256ByRepetition", "localComposite", "manifestSha256",
    "measurementFileSha256", "paired", "rawAggregatePolicy", "repeatFilesByRepetition",
    "repositoryCommit", "retrievalQuality", "sameCallerEnvelopeModelStack",
    "schemaVersion", "sourceFingerprintSha256",
}
FINAL_EVIDENCE_TOP_LEVEL_KEYS = {
    "combined", "contentInventory", "evidenceMethod", "fixtureSnapshot", "kind",
    "producerSnapshot", "repetitions", "runtimeIdentities", "schemaVersion",
}

# Filled from canonical, path-aware inventories of the immutable artifacts.
# A schema commitment detects any added/removed/retyped field; a text
# commitment detects any substituted name, address, unsupported-language text,
# model output, path, or other string even when it uses a benign-looking key.
FINAL_REPORT_SCHEMA_SHA256 = {
    "1": "873fb4d6585db2ba4e590f618c2176885e05527e7699165e22ec84422dcda7a1",
    "2": "e7b8a009e46b9c611864163537bf82c4c1739ad559eaee76c36367534bf9a8a7",
}
FINAL_REPORT_TEXT_SHA256 = {
    "1": "51850a2cea70dfc85d7f11182c510c13c88db4a79fe4832632faaea3fd116d51",
    "2": "cf58eb74f3fdbb9fe57fd23c8859b2a4e1fafa0d9ca9ee2e083dc8dbf402c4e3",
}
FINAL_COMBINED_SCHEMA_SHA256 = "8e38f627ea5fb2a38d1accb354415c2d9d0e3f20828e66cbafb7bece5493580b"
FINAL_COMBINED_TEXT_SHA256 = "299e507ebad955b0f699acc6ae9b73bc8dec3ba488107eac08076b71e1c8212b"
FINAL_EVIDENCE_SCHEMA_SHA256 = "6b14f024761786be7cadc3d43b4ca6804321b7292f6939466d327568e612306c"
FINAL_EVIDENCE_TEXT_SHA256 = "02ed6d20692ad2d3e64fc5e3a278919523be0870e9d27859d97b94834b891fb0"

FORBIDDEN_ARTIFACT_BYTES = (
    b"/users/",
    b"/home/",
    b"/private/var/",
    b"/tmp/",
    b"file://",
    b"authorization:",
    b'"authorization"',
    b"bearer ",
    b"api_key",
    b"api-key",
    b"-----begin ",
    b"id_rsa",
    b"id_ed25519",
    b"audio_path",
    b"audiopath",
)
SENSITIVE_KEY_NAMES = {
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
    "fullname",
    "personname",
    "customername",
    "streetaddress",
    "postaladdress",
}
EMAIL_PATTERN = re.compile(rb"(?i)\b[A-Z0-9._%+-]+@[A-Z0-9.-]+\.[A-Z]{2,}\b")
PHONE_PATTERNS = (
    re.compile(rb"(?<!\w)\+\d{1,3}(?:[ .()-]*\d){7,14}(?!\d)"),
    re.compile(rb"(?<!\d)\(\d{3}\)[ .-]*\d{3}[ .-]*\d{4}(?!\d)"),
    re.compile(rb"(?<!\d)\d{3}[ .-]\d{3}[ .-]\d{4}(?!\d)"),
)
SECRET_ASSIGNMENT_PATTERN = re.compile(
    rb"(?i)\b(?:authorization|api[_-]?key|access[_-]?token|refresh[_-]?token|"
    rb"client[_-]?secret|password|passwd|credential|private[_-]?key)\b\s*[:=]"
)
JWT_PATTERN = re.compile(
    rb"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b"
)
AWS_ACCESS_KEY_PATTERN = re.compile(rb"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b")
POSTAL_ADDRESS_PATTERNS = (
    re.compile(
        rb"(?i)\b\d{1,5}\s+[A-Z][A-Za-z.-]*(?:\s+[A-Z][A-Za-z.-]*){0,3}\s+"
        rb"(?:street|st\.?|road|rd\.?|avenue|ave\.?)\b"
    ),
    re.compile(
        rb"(?i)\b(?:ulica|ul\.?|aleja|al\.?)\s+[A-Z][A-Za-z.-]*"
        rb"(?:\s+[A-Z][A-Za-z.-]*){0,3}\s+\d{1,5}\b"
    ),
)
BASE64_TEXT_PATTERN = re.compile(r"^[A-Za-z0-9+/]+={0,2}$")
BINARY_MAGIC_PREFIXES = (
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
CONTENT_INVENTORY_FIELDS = (
    "output",
    "rawModelOutput",
    "surfaceOutput",
    "provenance",
    "error",
    "toolSteps",
)
PROVENANCE_ALLOWLIST = {
    "[[Ember sync]]",
    "[[Nimbus sync]]",
    "[[Orchid launch]]",
    "[[Quartz review]]",
}


class ArtifactError(RuntimeError):
    """A content-free validation failure."""


def require(condition: bool, message: str) -> None:
    if not condition:
        raise ArtifactError(message)


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def framed_hash(parts: list[str]) -> str:
    digest = hashlib.sha256()
    for part in parts:
        encoded = part.encode("utf-8")
        digest.update(len(encoded).to_bytes(8, "little"))
        digest.update(encoded)
    return digest.hexdigest()


def is_sha256(value: Any) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 64
        and all(char in "0123456789abcdef" for char in value)
    )


def is_git_sha(value: Any) -> bool:
    return (
        isinstance(value, str)
        and len(value) == 40
        and all(char in "0123456789abcdef" for char in value)
    )


def load_object(data: bytes, label: str) -> dict[str, Any]:
    try:
        value = json.loads(data)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ArtifactError(f"{label}: invalid UTF-8 JSON") from exc
    require(isinstance(value, dict), f"{label}: root must be an object")
    return value


def repository_path(value: Any, label: str) -> Path:
    require(isinstance(value, str) and value, f"{label}: path must be non-empty")
    relative = Path(value)
    require(not relative.is_absolute(), f"{label}: absolute path forbidden")
    require(".." not in relative.parts, f"{label}: parent traversal forbidden")
    require(
        relative.parts[:2] == RESULTS_PREFIX.parts,
        f"{label}: path must stay under eval/results",
    )
    return REPO_ROOT / relative


def read_repository_file(relative: Path) -> bytes:
    path = repository_path(relative.as_posix(), relative.as_posix())
    try:
        return path.read_bytes()
    except OSError as exc:
        raise ArtifactError(f"{relative}: missing or unreadable") from exc


def read_fixed_source(relative: Path) -> bytes:
    require(not relative.is_absolute(), f"{relative}: source path must be relative")
    require(".." not in relative.parts, f"{relative}: source traversal forbidden")
    try:
        return (REPO_ROOT / relative).read_bytes()
    except OSError as exc:
        raise ArtifactError(f"{relative}: source missing or unreadable") from exc


def walk_json(value: Any):
    if isinstance(value, dict):
        for key, child in value.items():
            yield key, child
            yield from walk_json(child)
    elif isinstance(value, list):
        for child in value:
            yield from walk_json(child)


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


def canonical_sha256(value: Any) -> str:
    return sha256(
        json.dumps(
            value,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
        ).encode("utf-8")
    )


def schema_inventory(value: Any, path: tuple[str, ...] = ()) -> list[list[Any]]:
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


def text_inventory(value: Any) -> list[list[str]]:
    return [
        ["/".join(path), child]
        for path, child in walk_values(value)
        if isinstance(child, str)
    ]


def contains_encoded_binary(value: str) -> bool:
    candidate = "".join(value.split())
    if candidate.lower().startswith(("data:audio/", "data:application/octet-stream")):
        return True
    if len(candidate) < 8 or len(candidate) % 4 != 0:
        return False
    if BASE64_TEXT_PATTERN.fullmatch(candidate) is None:
        return False
    if is_sha256(candidate) or is_git_sha(candidate):
        return False
    try:
        decoded = base64.b64decode(candidate, validate=True)
    except (ValueError, UnicodeEncodeError):
        return False
    if any(decoded.startswith(magic) for magic in BINARY_MAGIC_PREFIXES):
        return True
    if len(decoded) < 64:
        return False
    non_text = sum(
        byte not in b"\t\n\r" and not 32 <= byte <= 126
        for byte in decoded
    )
    return non_text / len(decoded) > 0.25


def case_ids(value: Any) -> set[str]:
    return {
        child
        for key, child in walk_json(value)
        if key == "caseId" and isinstance(child, str)
    }


def scan_privacy(data: bytes, label: str) -> None:
    lowered = data.lower()
    for forbidden in FORBIDDEN_ARTIFACT_BYTES:
        require(forbidden not in lowered, f"{label}: forbidden privacy marker")
    require(EMAIL_PATTERN.search(data) is None, f"{label}: email address forbidden")
    require(
        all(pattern.search(data) is None for pattern in PHONE_PATTERNS),
        f"{label}: phone number forbidden",
    )
    require(
        SECRET_ASSIGNMENT_PATTERN.search(data) is None,
        f"{label}: credential assignment forbidden",
    )
    require(JWT_PATTERN.search(data) is None, f"{label}: JWT-like token forbidden")
    require(
        AWS_ACCESS_KEY_PATTERN.search(data) is None,
        f"{label}: cloud access key forbidden",
    )
    require(
        all(pattern.search(data) is None for pattern in POSTAL_ADDRESS_PATTERNS),
        f"{label}: postal address forbidden",
    )
    try:
        value = json.loads(data)
    except (UnicodeDecodeError, json.JSONDecodeError):
        return
    for key, _ in walk_json(value):
        normalized = re.sub(r"[^a-z0-9]", "", key.lower())
        require(normalized not in SENSITIVE_KEY_NAMES, f"{label}: sensitive field forbidden")
    for _, child in walk_values(value):
        if isinstance(child, str):
            require(
                not contains_encoded_binary(child),
                f"{label}: encoded binary or audio payload forbidden",
            )


def validate_closed_contract(
    value: dict[str, Any],
    label: str,
    expected_top_level_keys: set[str],
    expected_schema_sha256: str,
    expected_text_sha256: str,
    expected_languages: set[str],
) -> None:
    require(set(value) == expected_top_level_keys, f"{label}: top-level schema differs")
    require(
        canonical_sha256(schema_inventory(value)) == expected_schema_sha256,
        f"{label}: closed schema commitment differs",
    )
    require(
        canonical_sha256(text_inventory(value)) == expected_text_sha256,
        f"{label}: closed text commitment differs",
    )
    languages = {
        child
        for key, child in walk_json(value)
        if key == "language" and isinstance(child, str)
    }
    require(languages == expected_languages, f"{label}: language inventory differs")


def validate_fixture_commitments(
    value: dict[str, Any],
    label: str,
    expected_generation_cases: set[str] | None = None,
    expected_retrieval_cases: set[str] | None = None,
) -> None:
    observed_generation: set[str] = set()
    observed_retrieval: set[str] = set()
    for record in walk_objects(value):
        case_id = record.get("caseId")
        if case_id is None:
            continue
        require(isinstance(case_id, str), f"{label}: case ID must be text")
        require(case_id in FINAL_CASE_IDS, f"{label}: unexpected case ID")
        oracle = GENERATION_FIXTURE_ORACLE.get(case_id)
        retrieval_oracle = RETRIEVAL_FIXTURE_ORACLE.get(case_id)
        language = record.get("language")
        expected_language = oracle[0] if oracle is not None else retrieval_oracle[0]
        if language is not None:
            require(language == expected_language, f"{label}: case language differs")
        if "casePayloadSha256" in record:
            require(oracle is not None, f"{label}: generation payload on retrieval case")
            require(
                record["casePayloadSha256"] == oracle[1],
                f"{label}: invented generation fixture commitment differs",
            )
            observed_generation.add(case_id)
        if "queryPayloadSha256" in record:
            require(retrieval_oracle is not None, f"{label}: retrieval payload on generation case")
            require(
                record["queryPayloadSha256"] == retrieval_oracle[1],
                f"{label}: invented retrieval fixture commitment differs",
            )
            observed_retrieval.add(case_id)
    require(
        observed_generation
        == (expected_generation_cases or set(GENERATION_FIXTURE_ORACLE)),
        f"{label}: generation fixture commitment inventory differs",
    )
    require(
        observed_retrieval
        == (expected_retrieval_cases or set(RETRIEVAL_FIXTURE_ORACLE)),
        f"{label}: retrieval fixture commitment inventory differs",
    )


def validate_generation_fixture_snapshot(evidence: dict[str, Any]) -> None:
    entry = evidence.get("fixtureSnapshot")
    require(
        entry
        == {
            "path": FINAL_FIXTURE_SNAPSHOT.as_posix(),
            "sha256": FINAL_FIXTURE_SHA256,
        },
        "generation fixture snapshot: path or hash differs",
    )
    fixture_bytes = read_repository_file(FINAL_FIXTURE_SNAPSHOT)
    require(
        sha256(fixture_bytes) == FINAL_FIXTURE_SHA256,
        "generation fixture snapshot: SHA-256 differs",
    )
    producer = evidence.get("producerSnapshot")
    require(
        isinstance(producer, dict)
        and producer.get("fixtureFileSha256") == FINAL_FIXTURE_SHA256,
        "generation fixture snapshot: producer commitment differs",
    )
    fixture = load_object(fixture_bytes, "generation fixture snapshot")
    require(
        set(fixture) == {"schemaVersion", "syntheticOnly", "cases"},
        "generation fixture snapshot: root schema differs",
    )
    require(fixture.get("schemaVersion") == 9, "generation fixture snapshot: schema differs")
    require(
        fixture.get("syntheticOnly") is True,
        "generation fixture snapshot: syntheticOnly must be true",
    )
    cases = fixture.get("cases")
    require(isinstance(cases, list), "generation fixture snapshot: cases must be an array")

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
    derived: dict[str, tuple[str, str, tuple[str, ...]]] = {}
    for case in cases:
        require(isinstance(case, dict), "generation fixture snapshot: case must be an object")
        case_id = case.get("id")
        require(isinstance(case_id, str), "generation fixture snapshot: case ID missing")
        require(case_id not in derived, "generation fixture snapshot: duplicate case ID")
        require(case_id in GENERATION_FIXTURE_ORACLE, "generation fixture snapshot: unknown case")
        expected_surface, expected_holdout = GENERATION_CASE_METADATA[case_id]
        require(case.get("surface") == expected_surface, "generation fixture snapshot: surface differs")
        require(
            case.get("holdout", False) is expected_holdout,
            "generation fixture snapshot: holdout flag differs",
        )
        payload = ["murmur-quality-case-payload-v2"] + [
            case.get(field, empty_defaults.get(field)) for field in payload_fields
        ]
        canonical = json.dumps(
            payload,
            ensure_ascii=False,
            separators=(",", ":"),
        )
        entities = case.get("syntheticRedactionEntities", [])
        require(
            isinstance(entities, list) and all(isinstance(item, str) for item in entities),
            "generation fixture snapshot: synthetic entity inventory differs",
        )
        derived[case_id] = (
            case.get("language"),
            framed_hash([canonical]),
            tuple(entities),
        )
    require(
        derived == GENERATION_FIXTURE_ORACLE,
        "generation fixture snapshot: payload or entity commitments differ",
    )
    scan_privacy(fixture_bytes, "generation fixture snapshot")


def retrieval_source_bindings() -> tuple[str, str]:
    fixture_bytes = read_fixed_source(RETRIEVAL_FIXTURE_SOURCE)
    try:
        fixture_text = fixture_bytes.decode("utf-8")
        fixture = json.loads(fixture_text)
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ArtifactError("retrieval fixture source: invalid UTF-8 JSON") from exc
    require(isinstance(fixture, list), "retrieval fixture source: root must be an array")
    derived: dict[str, tuple[str, str]] = {}
    for index, query in enumerate(fixture, start=1):
        require(isinstance(query, dict), "retrieval fixture source: query must be an object")
        require(
            set(query) == {"_comment", "query", "lang", "expected_meeting_ids"},
            "retrieval fixture source: query schema differs",
        )
        language = query.get("lang")
        text = query.get("query")
        expected_ids = query.get("expected_meeting_ids")
        require(language in {"en", "pl"}, "retrieval fixture source: unsupported language")
        require(isinstance(text, str), "retrieval fixture source: query text missing")
        require(
            isinstance(expected_ids, list)
            and expected_ids
            and all(isinstance(item, str) for item in expected_ids),
            "retrieval fixture source: expected IDs differ",
        )
        case_id = f"retrieval-{index:02d}"
        derived[case_id] = (
            language,
            framed_hash(
                ["murmur-retrieval-case-payload-v2", language, text, *expected_ids]
            ),
        )
    require(
        derived == RETRIEVAL_FIXTURE_ORACLE,
        "retrieval fixture source: query commitments differ",
    )
    scan_privacy(fixture_bytes, "retrieval fixture source")
    return framed_hash([fixture_text]), sha256(read_fixed_source(RETRIEVAL_CORPUS_SOURCE))


def validate_retrieval_source_bindings(
    reports: list[dict[str, Any]],
) -> None:
    fixture_sha256, corpus_sha256 = retrieval_source_bindings()
    for report in reports:
        retrieval = report.get("retrievalQuality")
        require(isinstance(retrieval, dict), "retrieval evidence: source binding missing")
        require(
            retrieval.get("fixtureSha256") == fixture_sha256,
            "retrieval evidence: fixture source commitment differs",
        )
        require(
            retrieval.get("corpusSourceSha256") == corpus_sha256,
            "retrieval evidence: corpus source commitment differs",
        )


def json_pointer(path: tuple[str, ...]) -> str:
    return "/" + "/".join(
        segment.replace("~", "~0").replace("/", "~1") for segment in path
    )


def content_inventory_entries(
    value: Any,
    repetition: str,
    path: tuple[str, ...] = (),
) -> list[dict[str, Any]]:
    entries: list[dict[str, Any]] = []
    if isinstance(value, dict):
        for key, child in value.items():
            child_path = (*path, key)
            if key in CONTENT_INVENTORY_FIELDS:
                if key in {"output", "rawModelOutput", "surfaceOutput", "error"}:
                    require(
                        child is None or isinstance(child, str),
                        "content inventory: output/error field type differs",
                    )
                elif key == "provenance":
                    require(
                        isinstance(child, list)
                        and all(item in PROVENANCE_ALLOWLIST for item in child),
                        "content inventory: provenance differs from synthetic allowlist",
                    )
                elif key == "toolSteps":
                    require(
                        isinstance(child, list)
                        and all(
                            item in {"search_meetings", "get_meeting"}
                            for item in child
                        ),
                        "content inventory: tool step differs from closed allowlist",
                    )
                entries.append(
                    {
                        "pointer": json_pointer(child_path),
                        "value": child,
                        "repetition": repetition,
                    }
                )
            entries.extend(content_inventory_entries(child, repetition, child_path))
    elif isinstance(value, list):
        for index, child in enumerate(value):
            entries.extend(
                content_inventory_entries(child, repetition, (*path, str(index)))
            )
    return entries


def validate_content_inventory(
    evidence: dict[str, Any],
    reports: dict[str, dict[str, Any]],
) -> None:
    entry = evidence.get("contentInventory")
    require(
        entry
        == {
            "path": FINAL_CONTENT_INVENTORY.as_posix(),
            "sha256": FINAL_CONTENT_INVENTORY_SHA256,
        },
        "content inventory: path or hash differs",
    )
    inventory_bytes = read_repository_file(FINAL_CONTENT_INVENTORY)
    require(
        sha256(inventory_bytes) == FINAL_CONTENT_INVENTORY_SHA256,
        "content inventory: SHA-256 differs",
    )
    inventory = load_object(inventory_bytes, "content inventory")
    string_values: list[str] = []
    string_counts: dict[str, int] = {}
    for repetition in ("1", "2"):
        report_inventory = text_inventory(reports[repetition])
        string_counts[repetition] = len(report_inventory)
        string_values.extend(value for _, value in report_inventory)
        # Keep the narrower typed-field checks as an additional semantic guard.
        content_inventory_entries(reports[repetition], repetition)
    expected = {
        "schemaVersion": 2,
        "kind": "murmur_synthetic_quality_all_string_inventory",
        "syntheticOnly": True,
        "logicalSha256ByRepetition": {
            repetition: FINAL_REPETITIONS[repetition]["logicalSha256"]
            for repetition in ("1", "2")
        },
        "pathAndOccurrenceCommitmentSha256ByRepetition": FINAL_REPORT_TEXT_SHA256,
        "stringLeafCountByRepetition": string_counts,
        "uniqueStringCount": len(set(string_values)),
        "uniqueStrings": sorted(set(string_values)),
    }
    require(
        inventory == expected,
        "content inventory: decoded archive content binding differs",
    )
    scan_privacy(inventory_bytes, "content inventory")


def validate_manifest_paths(evidence: dict[str, Any]) -> None:
    require(
        evidence.get("repetitions") == FINAL_REPETITIONS,
        "final evidence: exact repetition paths or hashes differ",
    )
    require(
        evidence.get("combined") == FINAL_COMBINED,
        "final evidence: exact combined path or hash differs",
    )


def validate_gzip(
    archive: bytes,
    archive_sha256: Any,
    logical_sha256: Any,
    label: str,
) -> tuple[bytes, dict[str, Any]]:
    require(is_sha256(archive_sha256), f"{label}: invalid archive SHA-256")
    require(is_sha256(logical_sha256), f"{label}: invalid logical SHA-256")
    require(sha256(archive) == archive_sha256, f"{label}: archive SHA-256 differs")
    require(len(archive) <= MAX_GZIP_ARCHIVE_BYTES, f"{label}: gzip archive exceeds size limit")
    require(len(archive) >= 10, f"{label}: truncated gzip header")
    require(
        archive[:10] == b"\x1f\x8b\x08\x00\x00\x00\x00\x00\x02\x03",
        f"{label}: gzip header must be deterministic",
    )
    decompressor = zlib.decompressobj(16 + zlib.MAX_WBITS)
    try:
        logical = decompressor.decompress(archive, MAX_GZIP_LOGICAL_BYTES + 1)
        require(
            len(logical) <= MAX_GZIP_LOGICAL_BYTES,
            f"{label}: logical JSON exceeds size limit",
        )
        logical += decompressor.flush(MAX_GZIP_LOGICAL_BYTES - len(logical) + 1)
    except zlib.error as exc:
        raise ArtifactError(f"{label}: gzip decompression failed") from exc
    require(len(logical) <= MAX_GZIP_LOGICAL_BYTES, f"{label}: logical JSON exceeds size limit")
    require(decompressor.eof, f"{label}: incomplete gzip stream")
    require(not decompressor.unused_data, f"{label}: concatenated/trailing gzip data forbidden")
    require(not decompressor.unconsumed_tail, f"{label}: unconsumed gzip data forbidden")
    require(sha256(logical) == logical_sha256, f"{label}: logical SHA-256 differs")
    report = load_object(logical, f"{label} logical JSON")
    require(report.get("syntheticOnly") is True, f"{label}: syntheticOnly must be true")
    scan_privacy(logical, f"{label} logical JSON")
    return logical, report


def validate_final() -> None:
    evidence_bytes = read_repository_file(FINAL_EVIDENCE)
    require(sha256(evidence_bytes) == FINAL_EVIDENCE_SHA256, "final evidence: SHA-256 differs")
    evidence = load_object(evidence_bytes, "final evidence")
    require(evidence.get("schemaVersion") == 1, "final evidence: schema differs")
    require(
        evidence.get("kind") == "murmur_local_cloud_quality_evidence",
        "final evidence: kind differs",
    )
    validate_closed_contract(
        evidence,
        "final evidence",
        FINAL_EVIDENCE_TOP_LEVEL_KEYS,
        FINAL_EVIDENCE_SCHEMA_SHA256,
        FINAL_EVIDENCE_TEXT_SHA256,
        set(),
    )
    validate_generation_fixture_snapshot(evidence)
    repetitions = evidence.get("repetitions")
    require(
        isinstance(repetitions, dict) and set(repetitions) == {"1", "2"},
        "final evidence: repetitions must be exactly 1 and 2",
    )
    validate_manifest_paths(evidence)

    logical_hashes: dict[str, str] = {}
    logical_paths: dict[str, str] = {}
    reports: dict[str, dict[str, Any]] = {}
    for repetition in ("1", "2"):
        entry = repetitions[repetition]
        require(
            isinstance(entry, dict)
            and set(entry)
            == {"archivePath", "archiveSha256", "logicalPath", "logicalSha256"},
            f"final R{repetition}: evidence entry differs",
        )
        archive_path = repository_path(entry["archivePath"], f"final R{repetition} archive")
        require(archive_path.suffix == ".gz", f"final R{repetition}: archive must be gzip")
        logical_path = Path(entry["logicalPath"])
        require(
            logical_path == Path(entry["archivePath"]).with_suffix(""),
            f"final R{repetition}: logical path must name decompressed archive",
        )
        try:
            archive = archive_path.read_bytes()
        except OSError as exc:
            raise ArtifactError(f"final R{repetition}: archive unreadable") from exc
        _, report = validate_gzip(
            archive,
            entry["archiveSha256"],
            entry["logicalSha256"],
            f"final R{repetition}",
        )
        require(report.get("schemaVersion") == 9, f"final R{repetition}: schema differs")
        require(
            case_ids(report) == FINAL_CASE_IDS,
            f"final R{repetition}: case IDs differ from invented fixture allowlist",
        )
        validate_closed_contract(
            report,
            f"final R{repetition}",
            FINAL_REPORT_TOP_LEVEL_KEYS,
            FINAL_REPORT_SCHEMA_SHA256[repetition],
            FINAL_REPORT_TEXT_SHA256[repetition],
            {"en", "pl"},
        )
        validate_fixture_commitments(report, f"final R{repetition}")
        logical_hashes[repetition] = entry["logicalSha256"]
        logical_paths[repetition] = entry["logicalPath"]
        reports[repetition] = report

    combined_entry = evidence.get("combined")
    require(
        isinstance(combined_entry, dict) and set(combined_entry) == {"path", "sha256"},
        "final evidence: combined entry differs",
    )
    combined_path = repository_path(combined_entry["path"], "final combined")
    try:
        combined_bytes = combined_path.read_bytes()
    except OSError as exc:
        raise ArtifactError("final combined: unreadable") from exc
    require(is_sha256(combined_entry["sha256"]), "final combined: invalid SHA-256")
    require(
        sha256(combined_bytes) == combined_entry["sha256"],
        "final combined: SHA-256 differs",
    )
    combined = load_object(combined_bytes, "final combined")
    require(combined.get("schemaVersion") == 5, "final combined: schema differs")
    validate_closed_contract(
        combined,
        "final combined",
        FINAL_COMBINED_TOP_LEVEL_KEYS,
        FINAL_COMBINED_SCHEMA_SHA256,
        FINAL_COMBINED_TEXT_SHA256,
        {"en", "pl"},
    )
    validate_fixture_commitments(
        combined,
        "final combined",
        set(GENERATION_FIXTURE_ORACLE) - {"live-bullets-pl-polaris"},
        set(RETRIEVAL_FIXTURE_ORACLE),
    )
    validate_retrieval_source_bindings([*reports.values(), combined])
    validate_content_inventory(evidence, reports)
    require(
        combined.get("inputResultSha256ByRepetition") == logical_hashes,
        "final combined: repetition logical hashes differ",
    )
    require(
        combined.get("repeatFilesByRepetition") == logical_paths,
        "final combined: repetition logical paths differ",
    )

    producer = evidence.get("producerSnapshot")
    require(isinstance(producer, dict), "final evidence: producer snapshot missing")
    measurement = combined.get("measurementFileSha256")
    require(isinstance(measurement, dict), "final combined: measurement hashes missing")
    for field in (
        "repositoryCommit",
        "sourceFingerprintSha256",
        "manifestSha256",
    ):
        require(
            combined.get(field) == producer.get(field),
            f"final combined: {field} differs from evidence",
        )
        valid_digest = (
            is_git_sha(producer.get(field))
            if field == "repositoryCommit"
            else is_sha256(producer.get(field))
        )
        require(valid_digest, f"final evidence: invalid {field}")
    for field in (
        "evaluatorFileSha256",
        "fixtureFileSha256",
        "repeatValidatorFileSha256",
    ):
        require(
            measurement.get(field) == producer.get(field),
            f"final combined: {field} differs from evidence",
        )
        require(is_sha256(producer.get(field)), f"final evidence: invalid {field}")
    require(is_sha256(producer.get("trackedDiffSha256")), "final evidence: invalid diff hash")
    for report in reports.values():
        for field in ("repositoryCommit", "sourceFingerprintSha256", "manifestSha256"):
            require(report.get(field) == producer.get(field), f"final report: {field} differs")

    runtime_identities = evidence.get("runtimeIdentities")
    require(
        isinstance(runtime_identities, dict) and set(runtime_identities) == {"local", "codex"},
        "final evidence: runtime identities differ",
    )
    for identity in runtime_identities.values():
        require(isinstance(identity, dict), "final evidence: runtime identity invalid")
        require(is_sha256(identity.get("sha256")), "final evidence: runtime SHA-256 invalid")
        require(isinstance(identity.get("version"), str), "final evidence: runtime version invalid")

    scan_privacy(evidence_bytes, "final evidence")
    scan_privacy(combined_bytes, "final combined")


def validate_history_case_inventory(
    report: dict[str, Any],
    expected_languages: dict[str, str],
    label: str,
) -> None:
    require(case_ids(report) == set(expected_languages), f"{label}: case IDs differ")
    for record in walk_objects(report):
        case_id = record.get("caseId")
        if case_id is None:
            continue
        require(case_id in expected_languages, f"{label}: unexpected case ID")
        if "language" in record:
            require(
                record["language"] == expected_languages[case_id],
                f"{label}: case language differs",
            )


def validate_history_envelope(
    bundle: dict[str, Any],
    bundle_name: str,
    expected_kind: str,
    derived_field: str,
    expected_derived: Any,
) -> dict[str, dict[str, Any]]:
    require(
        set(bundle) == {"schemaVersion", "kind", "encoding", "members", derived_field},
        f"{bundle_name}: outer schema differs",
    )
    require(bundle.get("schemaVersion") == 2, f"{bundle_name}: schema differs")
    require(bundle.get("kind") == expected_kind, f"{bundle_name}: kind differs")
    require(
        bundle.get("encoding")
        == "xz_lzma2_preset_9_over_compact_json_with_base64_gzip_members",
        f"{bundle_name}: encoding differs",
    )
    require(bundle.get(derived_field) == expected_derived, f"{bundle_name}: derived hash differs")
    members = bundle.get("members")
    require(
        isinstance(members, dict) and set(members) == {"r1", "r2"},
        f"{bundle_name}: members must be exactly r1 and r2",
    )
    reports: dict[str, dict[str, Any]] = {}
    sanitized = copy.deepcopy(bundle)
    for repetition in ("r1", "r2"):
        label = f"{bundle_name}/{repetition}"
        member = members[repetition]
        require(
            isinstance(member, dict)
            and set(member) == {"archiveSha256", "logicalSha256", "archiveBase64"},
            f"{label}: member schema differs",
        )
        expected_archive_sha256, expected_logical_sha256 = HISTORY_MEMBER_HASHES[label]
        require(
            member.get("archiveSha256") == expected_archive_sha256,
            f"{label}: fixed archive hash differs",
        )
        require(
            member.get("logicalSha256") == expected_logical_sha256,
            f"{label}: fixed logical hash differs",
        )
        encoded = member.get("archiveBase64")
        require(isinstance(encoded, str), f"{label}: archiveBase64 must be text")
        try:
            archive = base64.b64decode(encoded, validate=True)
        except (TypeError, ValueError) as exc:
            raise ArtifactError(f"{label}: invalid base64") from exc
        require(
            base64.b64encode(archive).decode("ascii") == encoded,
            f"{label}: base64 must be canonical",
        )
        _, report = validate_gzip(
            archive,
            expected_archive_sha256,
            expected_logical_sha256,
            label,
        )
        expected_schema = 3 if bundle_name == "baseline" else 8
        require(report.get("schemaVersion") == expected_schema, f"{label}: report schema differs")
        require(report.get("syntheticOnly") is True, f"{label}: syntheticOnly must be true")
        require(
            canonical_sha256(schema_inventory(report)) == HISTORY_SCHEMA_SHA256[label],
            f"{label}: closed schema commitment differs",
        )
        require(
            canonical_sha256(text_inventory(report)) == HISTORY_TEXT_SHA256[label],
            f"{label}: path/text commitment differs",
        )
        if bundle_name == "baseline":
            validate_history_case_inventory(report, BASELINE_CASE_LANGUAGES, label)
            require(
                not any(key == "casePayloadSha256" for key, _ in walk_json(report)),
                f"{label}: baseline unexpectedly claims case payload hashes",
            )
        else:
            decision_languages = {
                case_id: oracle[0]
                for case_id, oracle in GENERATION_FIXTURE_ORACLE.items()
            } | {
                case_id: oracle[0]
                for case_id, oracle in RETRIEVAL_FIXTURE_ORACLE.items()
            }
            validate_history_case_inventory(report, decision_languages, label)
            validate_fixture_commitments(report, label)
        reports[label] = report
        sanitized["members"][repetition]["archiveBase64"] = (
            "<validated exact synthetic gzip member>"
        )
    scan_privacy(
        json.dumps(sanitized, ensure_ascii=False, sort_keys=True).encode("utf-8"),
        f"{bundle_name} envelope",
    )
    return reports


def validate_history_bundle(
    relative: Path,
    expected_sha256: str,
    bundle_name: str,
    expected_kind: str,
    derived_field: str,
    expected_derived: Any,
) -> dict[str, dict[str, Any]]:
    archive = read_repository_file(relative)
    require(sha256(archive) == expected_sha256, f"{bundle_name}: XZ SHA-256 differs")
    require(len(archive) <= MAX_GZIP_ARCHIVE_BYTES, f"{bundle_name}: XZ archive too large")
    require(archive[:6] == b"\xfd7zXZ\x00", f"{bundle_name}: invalid XZ magic")
    try:
        logical = lzma.decompress(archive, format=lzma.FORMAT_XZ)
    except lzma.LZMAError as exc:
        raise ArtifactError(f"{bundle_name}: XZ decompression failed") from exc
    require(len(logical) <= MAX_GZIP_LOGICAL_BYTES, f"{bundle_name}: XZ logical JSON too large")
    require(
        lzma.compress(
            logical,
            format=lzma.FORMAT_XZ,
            check=lzma.CHECK_CRC64,
            preset=9,
        )
        == archive,
        f"{bundle_name}: XZ bytes are not deterministic Python LZMA2 preset 9",
    )
    bundle = load_object(logical, f"{bundle_name} envelope")
    return validate_history_envelope(
        bundle,
        bundle_name,
        expected_kind,
        derived_field,
        expected_derived,
    )


def validate_history_content_inventory(
    reports: dict[str, dict[str, Any]],
) -> None:
    inventory_bytes = read_repository_file(HISTORY_CONTENT_INVENTORY)
    require(
        sha256(inventory_bytes) == HISTORY_CONTENT_INVENTORY_SHA256,
        "history content inventory: SHA-256 differs",
    )
    inventory = load_object(inventory_bytes, "history content inventory")
    strings: list[str] = []
    counts: dict[str, int] = {}
    for label in sorted(reports):
        report_inventory = text_inventory(reports[label])
        counts[label] = len(report_inventory)
        strings.extend(value for _, value in report_inventory)
    expected = {
        "schemaVersion": 1,
        "kind": "murmur_synthetic_quality_history_all_string_inventory",
        "syntheticOnly": True,
        "memberLogicalSha256": {
            label: HISTORY_MEMBER_HASHES[label][1]
            for label in sorted(HISTORY_MEMBER_HASHES)
        },
        "pathAndOccurrenceCommitmentSha256ByMember": {
            label: HISTORY_TEXT_SHA256[label]
            for label in sorted(HISTORY_TEXT_SHA256)
        },
        "stringLeafCountByMember": counts,
        "uniqueStringCount": len(set(strings)),
        "uniqueStrings": sorted(set(strings)),
    }
    require(inventory == expected, "history content inventory: decoded binding differs")
    scan_privacy(inventory_bytes, "history content inventory")


def validate_history() -> None:
    reports = validate_history_bundle(
        BASELINE_BUNDLE,
        BASELINE_BUNDLE_SHA256,
        "baseline",
        "murmur_quality_initial_baseline_raw_archive_bundle",
        "removedDerivedRescoreSha256",
        BASELINE_RESCORE_SHA256,
    )
    reports.update(
        validate_history_bundle(
            DECISION_BUNDLE,
            DECISION_BUNDLE_SHA256,
            "decision",
            "murmur_quality_pre_routing_decision_raw_archive_bundle",
            "removedDerivedCombinedSha256",
            DECISION_COMBINED_SHA256,
        )
    )
    validate_history_content_inventory(reports)


def expect_failure(action: Callable[[], None], label: str) -> None:
    try:
        action()
    except ArtifactError:
        return
    raise ArtifactError(f"selftest did not reject {label}")


def run_selftests() -> None:
    evidence = load_object(read_repository_file(FINAL_EVIDENCE), "selftest evidence")
    first = evidence["repetitions"]["1"]
    archive = repository_path(first["archivePath"], "selftest archive").read_bytes()
    report = load_object(gzip.decompress(archive), "selftest final R1")
    mutated = bytearray(archive)
    mutated[-1] ^= 1
    expect_failure(
        lambda: validate_gzip(
            bytes(mutated),
            first["archiveSha256"],
            first["logicalSha256"],
            "selftest mutated gzip",
        ),
        "a mutated gzip member",
    )
    expect_failure(
        lambda: repository_path("../outside.json", "selftest path"),
        "parent path traversal",
    )
    alternate_path = copy.deepcopy(evidence)
    alternate_path["repetitions"]["1"]["archivePath"] = (
        "eval/results/renamed-final-r1.json.gz"
    )
    expect_failure(
        lambda: validate_manifest_paths(alternate_path),
        "a valid-looking alternate archive path",
    )
    alternate_combined = copy.deepcopy(evidence)
    alternate_combined["combined"]["path"] = "eval/results/renamed-combined.json"
    expect_failure(
        lambda: validate_manifest_paths(alternate_combined),
        "a valid-looking alternate combined path",
    )
    expect_failure(
        lambda: scan_privacy(b'{"path":"/Users/example/private.md"}', "selftest privacy"),
        "a user path",
    )
    for payload, label in (
        (b'{"contact":"alice@example.com"}', "an email address"),
        (b'{"phone":"+48 501 234 567"}', "a phone number"),
        (b'{"note":"access_token=abcd"}', "a secret assignment"),
        (b'{"authorization":"Basic abc"}', "authorization data"),
        (b'{"audioPath":"recording.wav"}', "an audio field"),
        (b'{"payload":"Alice Example, 123 Main Street"}', "a real-PII-shaped postal address"),
        (b'{"payload":"UklGRgAAAABXQVZFZm10IA=="}', "encoded WAV under an innocuous key"),
    ):
        expect_failure(
            lambda payload=payload: scan_privacy(payload, "selftest privacy"),
            label,
        )
    unsupported_language = copy.deepcopy(report)
    unsupported_language["retrievalQuality"]["cases"][0]["language"] = "fr"
    expect_failure(
        lambda: validate_closed_contract(
            unsupported_language,
            "selftest unsupported language",
            FINAL_REPORT_TOP_LEVEL_KEYS,
            FINAL_REPORT_SCHEMA_SHA256["1"],
            FINAL_REPORT_TEXT_SHA256["1"],
            {"en", "pl"},
        ),
        "unsupported-language fixture text",
    )
    real_pii_field = copy.deepcopy(report)
    real_pii_field["payload"] = {"fullName": "Alice Example", "address": "123 Main Street"}
    expect_failure(
        lambda: validate_closed_contract(
            real_pii_field,
            "selftest real PII field",
            FINAL_REPORT_TOP_LEVEL_KEYS,
            FINAL_REPORT_SCHEMA_SHA256["1"],
            FINAL_REPORT_TEXT_SHA256["1"],
            {"en", "pl"},
        ),
        "an unexpected real-PII-shaped fixture field",
    )
    encoded_audio_field = copy.deepcopy(report)
    encoded_audio_field["runLabel"] = "UklGRgAAAABXQVZFZm10IA=="
    expect_failure(
        lambda: scan_privacy(
            json.dumps(encoded_audio_field, ensure_ascii=False).encode("utf-8"),
            "selftest encoded audio",
        ),
        "encoded audio under an allowed text key",
    )
    arbitrary_archived_prose = copy.deepcopy(report)
    changed_output = False
    for record in walk_objects(arbitrary_archived_prose):
        if isinstance(record.get("output"), str):
            record["output"] = "Alice Example discussed a private vault note."
            changed_output = True
            break
    require(changed_output, "selftest fixture has no output field")
    committed_inventory = load_object(
        read_repository_file(FINAL_CONTENT_INVENTORY),
        "selftest content inventory",
    )
    expect_failure(
        lambda: require(
            {
                value
                for _, value in text_inventory(arbitrary_archived_prose)
            }
            <= set(committed_inventory["uniqueStrings"]),
            "selftest content inventory differs",
        ),
        "arbitrary prose under an allowed archived output field",
    )
    fixture_bytes = read_repository_file(FINAL_FIXTURE_SNAPSHOT)
    expect_failure(
        lambda: require(
            sha256(fixture_bytes + b" ") == FINAL_FIXTURE_SHA256,
            "selftest fixture source hash differs",
        ),
        "a mutated synthetic fixture source",
    )
    drifted = copy.deepcopy(evidence)
    drifted["combined"]["sha256"] = "0" * 64
    expect_failure(
        lambda: require(
            drifted["combined"]["sha256"]
            == sha256(repository_path(drifted["combined"]["path"], "selftest combined").read_bytes()),
            "selftest combined hash differs",
        ),
        "a forged combined hash",
    )


def run_history_selftests() -> None:
    baseline_outer = load_object(
        lzma.decompress(read_repository_file(BASELINE_BUNDLE), format=lzma.FORMAT_XZ),
        "selftest baseline envelope",
    )
    extra_member = copy.deepcopy(baseline_outer)
    extra_member["members"]["r3"] = copy.deepcopy(extra_member["members"]["r1"])
    expect_failure(
        lambda: validate_history_envelope(
            extra_member,
            "baseline",
            "murmur_quality_initial_baseline_raw_archive_bundle",
            "removedDerivedRescoreSha256",
            BASELINE_RESCORE_SHA256,
        ),
        "a third historical bundle member",
    )
    displaced_base64 = copy.deepcopy(baseline_outer)
    displaced_base64["payload"] = displaced_base64["members"]["r1"]["archiveBase64"]
    expect_failure(
        lambda: validate_history_envelope(
            displaced_base64,
            "baseline",
            "murmur_quality_initial_baseline_raw_archive_bundle",
            "removedDerivedRescoreSha256",
            BASELINE_RESCORE_SHA256,
        ),
        "archiveBase64 outside the two fixed member paths",
    )
    noncanonical_base64 = copy.deepcopy(baseline_outer)
    noncanonical_base64["members"]["r1"]["archiveBase64"] += "\n"
    expect_failure(
        lambda: validate_history_envelope(
            noncanonical_base64,
            "baseline",
            "murmur_quality_initial_baseline_raw_archive_bundle",
            "removedDerivedRescoreSha256",
            BASELINE_RESCORE_SHA256,
        ),
        "noncanonical historical base64",
    )
    original_archive = base64.b64decode(
        baseline_outer["members"]["r1"]["archiveBase64"],
        validate=True,
    )
    expect_failure(
        lambda: validate_gzip(
            original_archive + original_archive,
            sha256(original_archive + original_archive),
            HISTORY_MEMBER_HASHES["baseline/r1"][1],
            "selftest concatenated gzip",
        ),
        "concatenated gzip members",
    )
    expect_failure(
        lambda: scan_privacy(
            b'{"output":"private.person@example.com"}',
            "selftest decoded historical PII",
        ),
        "PII in a decoded historical member",
    )


def main() -> int:
    parser = argparse.ArgumentParser()
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--final", action="store_true")
    mode.add_argument("--history", action="store_true")
    parser.add_argument("--selftest", action="store_true")
    args = parser.parse_args()

    try:
        if args.final:
            validate_final()
            print(
                "final quality artifacts verified: deterministic gzip, archive/logical/combined "
                "hashes, closed PL/EN source/fixture/text/schema commitments, human-readable "
                "content inventory, repetition bindings, producer commitments, and privacy scan"
            )
        else:
            validate_history()
            print(
                "historical quality artifacts verified: deterministic XZ, exact two-member gzip "
                "archives, closed case/schema/text commitments, human-readable all-string "
                "inventory, derived hashes, and privacy scan"
            )
        if args.selftest:
            run_selftests()
            if args.history:
                run_history_selftests()
            print("quality artifact verifier selftests passed")
    except ArtifactError as exc:
        print(f"quality artifact verification failed: {exc}")
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
