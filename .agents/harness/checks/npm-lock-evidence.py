#!/usr/bin/env python3
"""Emit compact, reviewable evidence for a package.json/package-lock.json diff.

The command is deliberately read-only and offline.  npm owns interpretation of
its logical lock tree; this wrapper only reduces that complete tree to the
direct and changed package names a reviewer must inspect.
"""

from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from typing import Any, Dict, Iterable, Mapping, Sequence


MAX_CHANGED_LOCK_ENTRIES = 512
MAX_OUTPUT_BYTES = 65_536
BASE_SHA_ENV = "MURMUR_HARNESS_BASE_SHA"
SHA1_RE = re.compile(r"^[0-9a-f]{40}$")
DIRECT_DEPENDENCY_KEYS = (
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
)


class EvidenceError(RuntimeError):
    pass


def _reject_duplicate_keys(pairs: Sequence[tuple[str, Any]]) -> Dict[str, Any]:
    result: Dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise EvidenceError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def _json_bytes(raw: bytes, label: str) -> Dict[str, Any]:
    try:
        value = json.loads(
            raw.decode("utf-8"),
            object_pairs_hook=_reject_duplicate_keys,
        )
    except (UnicodeDecodeError, json.JSONDecodeError, EvidenceError) as exc:
        raise EvidenceError(f"{label} is not strict JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise EvidenceError(f"{label} root must be an object")
    return value


def _read(path: Path) -> tuple[bytes, Dict[str, Any]]:
    try:
        raw = path.read_bytes()
    except OSError as exc:
        raise EvidenceError(f"cannot read {path}: {exc}") from exc
    return raw, _json_bytes(raw, str(path))


def _bound_base_sha() -> str:
    requested = os.environ.get(BASE_SHA_ENV, "")
    if not SHA1_RE.fullmatch(requested):
        raise EvidenceError(
            f"{BASE_SHA_ENV} must be the runner-bound 40-hex task base"
        )
    completed = subprocess.run(
        [
            "git",
            "rev-parse",
            "--verify",
            "--end-of-options",
            f"{requested}^{{commit}}",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    resolved = completed.stdout.decode("ascii", "replace").strip()
    if completed.returncode != 0 or resolved != requested:
        message = completed.stderr.decode("utf-8", "replace").strip()
        raise EvidenceError(
            f"runner-bound task base is unavailable: {message or requested}"
        )
    return requested


def _base(relative: str, base_sha: str) -> tuple[bytes, Dict[str, Any]]:
    completed = subprocess.run(
        [
            "git",
            "show",
            "--no-ext-diff",
            "--no-textconv",
            f"{base_sha}:{relative}",
        ],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )
    if completed.returncode != 0:
        message = completed.stderr.decode("utf-8", "replace").strip()
        raise EvidenceError(f"cannot read base {relative}: {message}")
    return completed.stdout, _json_bytes(
        completed.stdout, f"{base_sha}:{relative}"
    )


def _mapping(document: Mapping[str, Any], key: str, label: str) -> Dict[str, str]:
    value = document.get(key, {})
    if not isinstance(value, dict) or not all(
        isinstance(name, str) and isinstance(spec, str)
        for name, spec in value.items()
    ):
        raise EvidenceError(f"{label}.{key} must be a string map")
    return dict(sorted(value.items()))


def _peer_meta(
    document: Mapping[str, Any], label: str
) -> Dict[str, Dict[str, Any]]:
    value = document.get("peerDependenciesMeta", {})
    if not isinstance(value, dict):
        raise EvidenceError(f"{label}.peerDependenciesMeta must be an object map")
    result: Dict[str, Dict[str, Any]] = {}
    for name, metadata in value.items():
        if not isinstance(name, str) or not isinstance(metadata, dict):
            raise EvidenceError(
                f"{label}.peerDependenciesMeta entries must be objects"
            )
        optional = metadata.get("optional")
        if optional is not None and not isinstance(optional, bool):
            raise EvidenceError(
                f"{label}.peerDependenciesMeta.{name}.optional must be boolean"
            )
        result[name] = dict(sorted(metadata.items()))
    return dict(sorted(result.items()))


def _packages(lock: Mapping[str, Any], label: str) -> Dict[str, Mapping[str, Any]]:
    value = lock.get("packages")
    if not isinstance(value, dict) or not all(
        isinstance(path, str) and isinstance(item, dict)
        for path, item in value.items()
    ):
        raise EvidenceError(f"{label}.packages must be an object map")
    return {path: item for path, item in value.items()}


def _package_name(path: str, item: Mapping[str, Any]) -> str | None:
    declared = item.get("name")
    if isinstance(declared, str) and declared:
        return declared
    marker = "node_modules/"
    if marker not in path:
        return None
    candidate = path.rsplit(marker, 1)[-1]
    return candidate or None


def _changed_lock_entries(
    before: Mapping[str, Mapping[str, Any]],
    after: Mapping[str, Mapping[str, Any]],
) -> list[Dict[str, Any]]:
    rows: list[Dict[str, Any]] = []
    for path in sorted(set(before) | set(after)):
        old = before.get(path)
        new = after.get(path)
        if old == new:
            continue
        source = new if new is not None else old
        assert source is not None
        rows.append(
            {
                "path": path,
                "name": _package_name(path, source),
                "before_version": old.get("version") if old else None,
                "after_version": new.get("version") if new else None,
            }
        )
    if len(rows) > MAX_CHANGED_LOCK_ENTRIES:
        raise EvidenceError(
            "lock drift is too large for bounded review evidence: "
            f"{len(rows)} entries > {MAX_CHANGED_LOCK_ENTRIES}"
        )
    return rows


def _changed_mapping_names(
    before: Mapping[str, Any], after: Mapping[str, Any]
) -> set[str]:
    return {
        name
        for name in set(before) | set(after)
        if before.get(name) != after.get(name)
    }


def _logical_tree() -> Dict[str, Any]:
    environment = {
        **os.environ,
        "NPM_CONFIG_AUDIT": "false",
        "NPM_CONFIG_FUND": "false",
        "NPM_CONFIG_IGNORE_SCRIPTS": "true",
        "NPM_CONFIG_OFFLINE": "true",
        "NPM_CONFIG_UPDATE_NOTIFIER": "false",
    }
    completed = subprocess.run(
        ["npm", "ls", "--package-lock-only", "--all", "--json"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        env=environment,
        check=False,
    )
    if completed.returncode != 0:
        detail = completed.stderr.decode("utf-8", "replace").strip()
        if not detail:
            detail = completed.stdout.decode("utf-8", "replace")[-2000:]
        raise EvidenceError(
            f"npm lock-only logical tree failed with {completed.returncode}: {detail}"
        )
    return _json_bytes(completed.stdout, "npm ls lock-only output")


def _versions_by_name(
    logical: Mapping[str, Any], relevant: Iterable[str]
) -> tuple[Dict[str, list[str]], int]:
    wanted = set(relevant)
    found: Dict[str, set[str]] = {name: set() for name in wanted}
    count = 0
    root_dependencies = logical.get("dependencies", {})
    if not isinstance(root_dependencies, dict):
        raise EvidenceError("npm logical tree dependencies must be an object")
    stack: list[tuple[str, Mapping[str, Any]]] = [
        (str(name), item)
        for name, item in root_dependencies.items()
        if isinstance(item, dict)
    ]
    while stack:
        name, item = stack.pop()
        count += 1
        version = item.get("version")
        if name in wanted and isinstance(version, str) and version:
            found[name].add(version)
        children = item.get("dependencies", {})
        if children is None:
            continue
        if not isinstance(children, dict):
            raise EvidenceError(f"npm logical tree dependencies for {name} are malformed")
        stack.extend(
            (str(child_name), child)
            for child_name, child in children.items()
            if isinstance(child, dict)
        )
    result = {name: sorted(versions) for name, versions in sorted(found.items())}
    return result, count


def build_evidence(root: Path, base_sha: str) -> Dict[str, Any]:
    manifest_raw, manifest = _read(root / "package.json")
    lock_raw, lock = _read(root / "package-lock.json")
    base_manifest_raw, base_manifest = _base("package.json", base_sha)
    base_lock_raw, base_lock = _base("package-lock.json", base_sha)

    if lock.get("lockfileVersion") != 3:
        raise EvidenceError("package-lock.json lockfileVersion must be 3")
    current_packages = _packages(lock, "package-lock.json")
    base_packages = _packages(base_lock, f"{base_sha}:package-lock.json")
    lock_root = current_packages.get("")
    if not isinstance(lock_root, dict):
        raise EvidenceError("package-lock.json packages[''] is missing")

    current_direct = {
        key: _mapping(manifest, key, "package.json")
        for key in DIRECT_DEPENDENCY_KEYS
    }
    base_direct = {
        key: _mapping(
            base_manifest, key, f"{base_sha}:package.json"
        )
        for key in DIRECT_DEPENDENCY_KEYS
    }
    for key in DIRECT_DEPENDENCY_KEYS:
        lock_mapping = _mapping(
            lock_root, key, "package-lock.json packages['']"
        )
        if lock_mapping != current_direct[key]:
            raise EvidenceError(
                f"lock root {key} differ from package.json"
            )
    current_peer_meta = _peer_meta(manifest, "package.json")
    lock_peer_meta = _peer_meta(
        lock_root, "package-lock.json packages['']"
    )
    if lock_peer_meta != current_peer_meta:
        raise EvidenceError(
            "lock root peerDependenciesMeta differ from package.json"
        )
    base_peer_meta = _peer_meta(
        base_manifest, f"{base_sha}:package.json"
    )
    for key in ("name", "version"):
        expected = manifest.get(key)
        if lock.get(key) != expected or lock_root.get(key) != expected:
            raise EvidenceError(f"manifest/lock root {key} values differ")

    changed_entries = _changed_lock_entries(base_packages, current_packages)
    direct_names = {
        name
        for key in DIRECT_DEPENDENCY_KEYS
        for name in current_direct[key]
    }
    optional_names = set(current_direct["optionalDependencies"]) | {
        name
        for name, metadata in current_peer_meta.items()
        if metadata.get("optional") is True
    }
    changed_names = {
        str(row["name"]) for row in changed_entries if row.get("name")
    }
    changed_manifest_names = set().union(
        *(
            _changed_mapping_names(base_direct[key], current_direct[key])
            for key in DIRECT_DEPENDENCY_KEYS
        ),
        _changed_mapping_names(base_peer_meta, current_peer_meta),
    )
    relevant_names = sorted(
        direct_names | changed_names | changed_manifest_names
    )
    logical = _logical_tree()
    versions, logical_nodes = _versions_by_name(logical, relevant_names)
    missing_required_direct = sorted(
        name
        for name in direct_names - optional_names
        if not versions.get(name)
    )
    if missing_required_direct:
        raise EvidenceError(
            "direct packages missing from npm logical tree: "
            + ", ".join(missing_required_direct)
        )

    peer_metadata: list[Dict[str, Any]] = []
    for path, item in sorted(current_packages.items()):
        name = _package_name(path, item)
        peers = item.get("peerDependencies")
        if name in relevant_names and isinstance(peers, dict) and peers:
            peer_metadata.append(
                {
                    "path": path,
                    "name": name,
                    "peerDependencies": dict(sorted(peers.items())),
                }
            )

    return {
        "schema_version": 1,
        "mode": "offline-package-lock-only",
        "base_sha": base_sha,
        "package_json_sha256": hashlib.sha256(manifest_raw).hexdigest(),
        "package_lock_sha256": hashlib.sha256(lock_raw).hexdigest(),
        "base_package_json_sha256": hashlib.sha256(base_manifest_raw).hexdigest(),
        "base_package_lock_sha256": hashlib.sha256(base_lock_raw).hexdigest(),
        "project": {
            "name": manifest.get("name"),
            "version": manifest.get("version"),
            "lockfileVersion": lock.get("lockfileVersion"),
        },
        "direct": {
            **current_direct,
            "peerDependenciesMeta": current_peer_meta,
            "dependency_keys_unchanged": (
                sorted(current_direct["dependencies"])
                == sorted(base_direct["dependencies"])
            ),
            "dev_dependency_keys_unchanged": (
                sorted(current_direct["devDependencies"])
                == sorted(base_direct["devDependencies"])
            ),
            "optional_dependency_keys_unchanged": (
                sorted(current_direct["optionalDependencies"])
                == sorted(base_direct["optionalDependencies"])
            ),
            "peer_dependency_keys_unchanged": (
                sorted(current_direct["peerDependencies"])
                == sorted(base_direct["peerDependencies"])
            ),
            "peer_dependencies_meta_unchanged": (
                current_peer_meta == base_peer_meta
            ),
        },
        "missing_optional_versions": sorted(
            name for name in optional_names if not versions.get(name)
        ),
        "manifest_scripts_unchanged": (
            manifest.get("scripts") == base_manifest.get("scripts")
        ),
        "changed_manifest_dependency_names": sorted(
            changed_manifest_names
        ),
        "changed_lock_entries": changed_entries,
        "changed_lock_entry_count": len(changed_entries),
        "logical_tree_node_count": logical_nodes,
        "versions_by_name": versions,
        "peer_metadata": peer_metadata,
    }


def main() -> int:
    try:
        evidence = build_evidence(Path.cwd(), _bound_base_sha())
        encoded = (
            json.dumps(evidence, sort_keys=True, separators=(",", ":"))
            + "\n"
        ).encode("utf-8")
        if len(encoded) > MAX_OUTPUT_BYTES:
            raise EvidenceError(
                "structured lock evidence exceeds the bounded output limit: "
                f"{len(encoded)} > {MAX_OUTPUT_BYTES}"
            )
        sys.stdout.buffer.write(encoded)
        return 0
    except EvidenceError as exc:
        print(f"npm-lock-evidence: {exc}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
