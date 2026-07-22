#!/usr/bin/env python3
import json
import sys
from pathlib import Path


command, tree_sha, receipt_name = sys.argv[1:]
receipt = Path(receipt_name)
if command == "create":
    receipt.write_text(
        json.dumps({"verdict": "PASS", "tree_sha": tree_sha}, sort_keys=True),
        encoding="utf-8",
    )
    raise SystemExit(0)
if command == "verify":
    payload = json.loads(receipt.read_text(encoding="utf-8"))
    valid = payload.get("verdict") == "PASS" and payload.get("tree_sha") == tree_sha
    raise SystemExit(0 if valid else 1)
raise SystemExit(2)
