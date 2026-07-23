#!/usr/bin/env python3
import sys


VALUE_OPTIONS = {
    "-c",
    "-C",
    "--config-env",
    "--exec-path",
    "--git-dir",
    "--namespace",
    "--super-prefix",
    "--work-tree",
}


def git_subcommand(argv):
    if not argv or argv[0] != "git":
        return None
    index = 1
    while index < len(argv):
        value = argv[index]
        if value == "--":
            index += 1
            break
        if not value.startswith("-") or value == "-":
            break
        option = value.split("=", 1)[0]
        if option in VALUE_OPTIONS and "=" not in value:
            index += 2
        else:
            index += 1
    return argv[index] if index < len(argv) else None


def is_git_commit(argv):
    return git_subcommand(argv) == "commit"


raise SystemExit(0 if is_git_commit(sys.argv[1:]) else 1)
