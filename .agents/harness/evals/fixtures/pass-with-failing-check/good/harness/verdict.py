#!/usr/bin/env python3
import json
import sys
from pathlib import Path


payload = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
checks = payload.get("checks")
valid = (
    payload.get("verdict") == "PASS"
    and isinstance(checks, list)
    and bool(checks)
    and all(
        isinstance(check, dict)
        and type(check.get("exit_code")) is int
        and check["exit_code"] == 0
        for check in checks
    )
)
raise SystemExit(0 if valid else 1)
