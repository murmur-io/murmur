#!/usr/bin/env python3
import re
import sys


TOKEN = re.compile(r"\bsk-[A-Za-z0-9]{20,}\b")
value = " ".join(sys.argv[1:])
raise SystemExit(2 if TOKEN.search(value) else 0)
