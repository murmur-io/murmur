#!/usr/bin/env python3
import re
import sys


TOKEN = re.compile(r"\bsk-proj-[A-Za-z0-9_-]{20,}\b")
value = " ".join(sys.argv[1:])
raise SystemExit(2 if TOKEN.search(value) else 0)
