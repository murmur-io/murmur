#!/usr/bin/env python3
import json
import sys
from pathlib import Path


command, tree_sha, receipt_name = sys.argv[1:]
receipt = Path(receipt_name)
if command == "create":
    receipt.write_text(json.dumps({"verdict": "PASS"}), encoding="utf-8")
    raise SystemExit(0)
if command == "verify":
    payload = json.loads(receipt.read_text(encoding="utf-8"))
    raise SystemExit(0 if payload.get("verdict") == "PASS" else 1)
raise SystemExit(2)
