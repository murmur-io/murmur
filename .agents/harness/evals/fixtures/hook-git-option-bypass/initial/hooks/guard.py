#!/usr/bin/env python3
import sys


def is_git_commit(argv):
    return len(argv) >= 2 and argv[0] == "git" and argv[1] == "commit"


raise SystemExit(0 if is_git_commit(sys.argv[1:]) else 1)
