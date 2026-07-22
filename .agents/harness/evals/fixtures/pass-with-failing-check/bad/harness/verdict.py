#!/usr/bin/env python3
import json
import sys
from pathlib import Path


payload = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
valid = payload.get("verdict") == "PASS" and isinstance(payload.get("checks"), list)
raise SystemExit(0 if valid else 1)
