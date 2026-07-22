#!/usr/bin/env python3
import json
import sys
from pathlib import Path


payload = json.loads(Path(sys.argv[1]).read_text(encoding="utf-8"))
raise SystemExit(0 if payload.get("verdict") == "PASS" else 1)
