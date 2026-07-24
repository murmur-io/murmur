#!/usr/bin/env python3
"""Classify whether a shell command launches unwrapped Murmur Rust/full-CI work."""

import os
import json
import posixpath
import re
import shlex
import sys
from pathlib import Path


ASSIGNMENT = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*=.*$", re.DOTALL)
SHELLS = {"bash", "dash", "ksh", "sh", "zsh"}
LAUNCHERS = {"command", "env", "exec", "nice", "nohup", "sudo", "time"}
READ_ONLY_SEARCHES = {"grep", "rg"}
ALWAYS_ALLOWED_TOOLS = {"gh"}
SAFE_NONBUILD_TOOLS = READ_ONLY_SEARCHES | ALWAYS_ALLOWED_TOOLS
MAX_DEPTH = 12
REPO_ROOT = Path(__file__).resolve().parents[2]
RESOURCE_RUN = (REPO_ROOT / "scripts/agent-resource-run").resolve()
DEV_RUN = (REPO_ROOT / "scripts/agent-dev-run").resolve()
HEAVY_SCRIPTS = {
    (REPO_ROOT / "scripts/ci.sh").resolve(),
    (REPO_ROOT / "scripts/e2e-core.sh").resolve(),
    (REPO_ROOT / "scripts/e2e-mix.sh").resolve(),
    (REPO_ROOT / "scripts/measure-live-power.sh").resolve(),
    (REPO_ROOT / "scripts/release.sh").resolve(),
    (REPO_ROOT / "scripts/macos-sign-notarize.sh").resolve(),
}
HEAVY_PACKAGE_SCRIPTS = {"build", "bundle", "dev", "start", "tauri"}
SCRIPT_SCAN_STACK = set()
NODE_OPTIONS_WITH_VALUE = {
    "--cache",
    "--call",
    "--package",
    "--prefix",
    "--userconfig",
    "--workspace",
    "-c",
    "-p",
    "-w",
}
TEST_GUARDIAN_ENV = "MURMUR_AGENT_SELFTEST_GUARDIAN_RELEASE"
SAFE_SOURCE_TARGETS = {"$HOME/.cargo/env", "${HOME}/.cargo/env", "~/.cargo/env"}
DEV_SCRIPTS = {"dev", "start"}
DEV_PASSTHROUGH_FLAGS = {"--open", "--no-open", "--strict-port"}
DEV_PASSTHROUGH_VALUE_FLAGS = {"--host", "--port"}
RESERVED_RESOURCE_ENV = {
    "MURMUR_AGENT_RESOURCE_LANE_ACTIVE",
    "MURMUR_AGENT_DEV_PROXY_DIR",
    "MURMUR_AGENT_DEV_PROXY_TOKEN",
    "MURMUR_AGENT_DEV_REAL_CARGO",
    "MURMUR_AGENT_DEV_REAL_RUSTC",
}


def resolved_command_path(token):
    candidate = Path(token).expanduser()
    if not candidate.is_absolute():
        candidate = Path.cwd() / candidate
    return candidate.resolve()


def is_lane_runner(token):
    return resolved_command_path(token) == RESOURCE_RUN


def is_dev_runner(token):
    return resolved_command_path(token) == DEV_RUN


def assignment_name(token):
    if not ASSIGNMENT.match(token):
        return None
    return token.split("=", 1)[0]


def dev_passthrough_is_allowed(tokens):
    index = 0
    while index < len(tokens):
        token = tokens[index]
        if token in DEV_PASSTHROUGH_FLAGS:
            index += 1
            continue
        if token in DEV_PASSTHROUGH_VALUE_FLAGS:
            if index + 1 >= len(tokens):
                return False
            value = tokens[index + 1]
            index += 2
        elif token.startswith("--port="):
            value = token.split("=", 1)[1]
            token = "--port"
            index += 1
        elif token.startswith("--host="):
            value = token.split("=", 1)[1]
            token = "--host"
            index += 1
        else:
            return False
        if token == "--port":
            if not value.isdigit() or not 1 <= int(value) <= 65535:
                return False
        elif not re.fullmatch(r"[A-Za-z0-9_.:\[\]-]+", value):
            return False
    return True


def dev_command_is_allowed(tokens):
    if len(tokens) < 3 or basename(tokens[0]) != "npm":
        return False
    if tokens[1] not in ("run", "run-script") or tokens[2] not in DEV_SCRIPTS:
        return False
    remainder = tokens[3:]
    if not remainder:
        return True
    return remainder[0] == "--" and dev_passthrough_is_allowed(remainder[1:])


def runner_command_index(tokens, index, value_options):
    index += 1
    while index < len(tokens):
        token = tokens[index]
        if token == "--":
            return index + 1
        if any(token.startswith(option + "=") for option in value_options):
            index += 1
            continue
        if token in value_options:
            if index + 1 >= len(tokens):
                return len(tokens)
            index += 2
            continue
        if token.startswith("-"):
            return len(tokens)
        return index
    return index


def dev_runner_invocation_is_allowed(tokens, index):
    nested = runner_command_index(
        tokens,
        index,
        {"--chdir", "--term-grace-seconds"},
    )
    return nested < len(tokens) and dev_command_is_allowed(tokens[nested:])


def segment_is_dev(tokens, depth):
    if not tokens or depth > MAX_DEPTH:
        return True
    index = skip_assignments(tokens, 0)
    if index >= len(tokens):
        return False
    executable = basename(tokens[index])

    if is_dev_runner(tokens[index]):
        return False
    if is_lane_runner(tokens[index]):
        nested = runner_command_index(
            tokens,
            index,
            {"--chdir", "--deadline-seconds", "--term-grace-seconds"},
        )
        return nested < len(tokens) and segment_is_dev(tokens[nested:], depth + 1)
    if executable in ("npm", "yarn", "pnpm", "bun"):
        cursor = first_non_option(tokens, index + 1)
        if cursor >= len(tokens):
            return False
        action = tokens[cursor]
        if action in ("run", "run-script"):
            cursor = first_non_option(tokens, cursor + 1)
            return cursor < len(tokens) and tokens[cursor] in DEV_SCRIPTS
        return action in DEV_SCRIPTS
    if executable in ("npx", "pnpx", "bunx"):
        cursor = first_non_option(tokens, index + 1)
        return (
            cursor + 1 < len(tokens)
            and basename(tokens[cursor]) == "tauri"
            and tokens[cursor + 1] == "dev"
        )
    if executable == "tauri":
        return index + 1 < len(tokens) and tokens[index + 1] == "dev"
    if executable in LAUNCHERS:
        nested = launcher_command_index(tokens, index, executable, depth)
        return segment_is_dev(tokens[nested:], depth + 1)
    if executable in SHELLS:
        cursor = index + 1
        while cursor < len(tokens):
            token = tokens[cursor]
            if token == "--":
                cursor += 1
                break
            if token.startswith("-") and token != "-":
                if "c" in token[1:] and cursor + 1 < len(tokens):
                    return command_is_dev(tokens[cursor + 1], depth + 1)
                cursor += 1
                continue
            break
    return False


def is_repo_script(path):
    try:
        path.relative_to(REPO_ROOT / "scripts")
    except ValueError:
        return False
    return path.is_file() and path not in (RESOURCE_RUN, Path(__file__).resolve())


def is_heavy_script(token, depth=0):
    """Recognize known and newly-added repo scripts that transitively launch heavy work."""
    path = resolved_command_path(token)
    if path in HEAVY_SCRIPTS:
        return True
    if not is_repo_script(path) or path in SCRIPT_SCAN_STACK:
        return False
    try:
        body = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError):
        # A repo script that cannot be inspected must not become a bypass.
        return True
    SCRIPT_SCAN_STACK.add(path)
    try:
        return command_is_heavy(body, depth + 1)
    finally:
        SCRIPT_SCAN_STACK.remove(path)


def package_json_for(start_dir):
    current = start_dir.resolve()
    while True:
        candidate = current / "package.json"
        if candidate.is_file():
            return candidate
        if current == current.parent:
            return None
        current = current.parent


def package_script_is_heavy(script_name, depth):
    """Inspect npm lifecycle bodies as well as their implicit pre/post scripts."""
    if script_name in HEAVY_PACKAGE_SCRIPTS:
        return True
    package_json = package_json_for(Path.cwd())
    if package_json is None:
        return False
    try:
        scripts = json.loads(package_json.read_text(encoding="utf-8")).get("scripts", {})
    except (OSError, UnicodeError, json.JSONDecodeError, AttributeError):
        return True
    if not isinstance(scripts, dict):
        return True
    bodies = [scripts.get(name) for name in ("pre" + script_name, script_name, "post" + script_name)]
    present_bodies = [body for body in bodies if isinstance(body, str)]
    if not present_bodies:
        return False
    return any(command_is_heavy(body, depth + 1) for body in present_bodies)


def first_non_option(tokens, index):
    while index < len(tokens):
        token = tokens[index]
        if token == "--":
            return index + 1
        if not token.startswith("-") or token == "-":
            return index
        if token in NODE_OPTIONS_WITH_VALUE:
            index += 2
            continue
        index += 1
    return index


def node_tool_is_heavy(tokens, index, executable, depth):
    """Classify package-manager indirection without invoking package tooling."""
    cursor = index + 1
    if executable in ("npx", "pnpx", "bunx"):
        for option_index in range(cursor, len(tokens)):
            token = tokens[option_index]
            if token in ("-c", "--call"):
                if option_index + 1 >= len(tokens):
                    return True
                return command_is_heavy(tokens[option_index + 1], depth + 1)
            if token.startswith("--call="):
                return command_is_heavy(token.split("=", 1)[1], depth + 1)
        cursor = first_non_option(tokens, cursor)
        if cursor >= len(tokens):
            return False
        tool = basename(tokens[cursor])
        return tool == "tauri" or (
            tool == "ng" and cursor + 1 < len(tokens) and tokens[cursor + 1] == "build"
        )

    cursor = first_non_option(tokens, cursor)
    if cursor >= len(tokens):
        return False
    action = tokens[cursor]
    if action in ("exec", "x", "dlx"):
        cursor = first_non_option(tokens, cursor + 1)
        if cursor >= len(tokens):
            return False
        tool = basename(tokens[cursor])
        return tool == "tauri" or (
            tool == "ng" and cursor + 1 < len(tokens) and tokens[cursor + 1] == "build"
        )
    if action in ("run", "run-script"):
        cursor = first_non_option(tokens, cursor + 1)
        if cursor >= len(tokens):
            return False
        return package_script_is_heavy(tokens[cursor], depth)
    # npm/yarn/pnpm/bun all accept common lifecycle shorthand such as `npm start`.
    return package_script_is_heavy(action, depth)


def shell_text_with_newline_separators(command):
    """Turn executable newlines into `;` while preserving quoted newlines."""
    output = []
    quote = None
    escaped = False
    for character in command:
        if escaped:
            if character != "\n":
                output.append(character)
            escaped = False
            continue
        if character == "\\" and quote != "'":
            escaped = True
            continue
        if quote is not None:
            output.append(character)
            if character == quote:
                quote = None
            continue
        if character in ("'", '"'):
            quote = character
            output.append(character)
        elif character == "\n":
            output.append(";")
        else:
            output.append(character)
    if escaped:
        output.append("\\")
    return "".join(output)


def tokenize(command):
    lexer = shlex.shlex(
        shell_text_with_newline_separators(command),
        posix=True,
        punctuation_chars="();|&{}<>",
    )
    lexer.commenters = ""
    lexer.whitespace_split = True
    try:
        return list(lexer)
    except ValueError:
        # An unbalanced command is unsafe to classify as an allow when it names
        # a heavy tool. The caller will conservatively deny on this sentinel.
        return ["__MURMUR_UNPARSEABLE__", command]


def command_segments(tokens):
    segment = []
    for token in tokens:
        if token and all(character in ";|&(){}" for character in token):
            if segment:
                yield segment
                segment = []
        else:
            segment.append(token)
    if segment:
        yield segment


def basename(token):
    return os.path.basename(posixpath.normpath(token))


def is_dynamic_executable(token):
    return token == "$" or token.startswith("$") or token.startswith("`")


def skip_assignments(tokens, index):
    while index < len(tokens) and ASSIGNMENT.match(tokens[index]):
        index += 1
    return index


def launcher_command_index(tokens, index, executable, depth):
    index += 1
    while index < len(tokens):
        token = tokens[index]
        if token == "--":
            return index + 1
        if executable == "env" and ASSIGNMENT.match(token):
            index += 1
            continue
        if executable == "env" and token in ("-i", "--ignore-environment", "-0", "--null"):
            index += 1
            continue
        if executable == "env" and token in ("-S", "--split-string"):
            return len(tokens)
        if executable == "env" and token in ("-u", "--unset", "-C", "--chdir"):
            index += 2
            continue
        if executable == "env" and (
            token.startswith("--unset=") or token.startswith("--chdir=")
        ):
            index += 1
            continue
        if executable == "env" and token.startswith("--split-string="):
            nested = token.split("=", 1)[1]
            return index if command_is_heavy(nested, depth + 1) else len(tokens)
        if executable == "command" and token in ("-v", "-V"):
            return len(tokens)
        if executable == "exec" and token == "-a":
            index += 2
            continue
        if executable in ("exec", "nohup") and token in ("-c", "-l"):
            index += 1
            continue
        if executable == "nice" and token in ("-n", "--adjustment"):
            index += 2
            continue
        if executable == "time" and token in ("-f", "--format", "-o", "--output"):
            index += 2
            continue
        if executable == "sudo" and token in (
            "-C", "-D", "-g", "-h", "-p", "-R", "-r", "-t", "-T", "-u"
        ):
            index += 2
            continue
        if token.startswith("-"):
            index += 1
            continue
        break
    return index


def shell_nested_command(tokens, index, depth):
    for redirection in ("<", "<<", "<<<"):
        if redirection in tokens:
            redirection_index = tokens.index(redirection)
            if redirection_index + 1 < len(tokens):
                target = tokens[redirection_index + 1]
                if (
                    is_heavy_script(target, depth)
                    or is_dynamic_executable(target)
                    or command_is_heavy(target, depth + 1)
                ):
                    return True
    index += 1
    while index < len(tokens):
        token = tokens[index]
        if token == "--":
            index += 1
            break
        if token.startswith("-") and token != "-":
            if "c" in token[1:]:
                if index + 1 >= len(tokens):
                    return True
                return command_is_heavy(tokens[index + 1], depth + 1)
            index += 1
            continue
        break
    if index < len(tokens):
        if is_heavy_script(tokens[index], depth) or is_dynamic_executable(tokens[index]):
            return True
    return False


def segment_is_heavy(tokens, depth):
    if not tokens:
        return False
    if tokens[0] == "__MURMUR_UNPARSEABLE__":
        raw = tokens[1]
        direct_heavy = re.search(
            r"(^|[^A-Za-z0-9_-])(cargo|rustc|tauri)([^A-Za-z0-9_-]|$)", raw
        )
        package_heavy = re.search(
            r"\b(?:npm|yarn|pnpm|bun)\s+(?:run\s+)?(?:start|dev|build|bundle|tauri)\b",
            raw,
        )
        frontend_heavy = re.search(r"\b(?:npx|pnpx|bunx)\b.*\bng\s+build\b", raw)
        return bool(direct_heavy or package_heavy or frontend_heavy) or any(
            path.name in raw for path in HEAVY_SCRIPTS
        )

    if any(assignment_name(token) in RESERVED_RESOURCE_ENV for token in tokens):
        return True
    index = skip_assignments(tokens, 0)
    if index >= len(tokens):
        return False
    executable = basename(tokens[index])

    if is_lane_runner(tokens[index]):
        nested = runner_command_index(
            tokens,
            index,
            {"--chdir", "--deadline-seconds", "--term-grace-seconds"},
        )
        # A long-lived server may not own the flock. All other commands are
        # safe here because the exact canonical lane runner supervises them.
        return nested < len(tokens) and segment_is_dev(tokens[nested:], depth + 1)
    if executable == "agent-resource-run":
        # A same-named executable outside this repository is not the supervisor.
        return True
    if is_dev_runner(tokens[index]):
        return not dev_runner_invocation_is_allowed(tokens, index)
    if executable == "agent-dev-run":
        # A same-named executable outside this repository is not the dev supervisor.
        return True
    if is_dynamic_executable(tokens[index]):
        return True
    if executable in READ_ONLY_SEARCHES:
        return False
    if executable in ("cargo", "rustc"):
        return True
    if is_heavy_script(tokens[index], depth):
        return True

    if executable in ("npm", "npx", "yarn", "pnpm", "pnpx", "bun", "bunx"):
        return node_tool_is_heavy(tokens, index, executable, depth)
    if executable == "tauri":
        return True
    if executable == "ng":
        return index + 1 < len(tokens) and tokens[index + 1] == "build"

    if executable in LAUNCHERS:
        if executable == "env":
            for nested_index in range(index + 1, len(tokens)):
                token = tokens[nested_index]
                if token in ("-S", "--split-string") and nested_index + 1 < len(tokens):
                    return command_is_heavy(tokens[nested_index + 1], depth + 1)
                if token.startswith("--split-string="):
                    return command_is_heavy(token.split("=", 1)[1], depth + 1)
        nested = launcher_command_index(tokens, index, executable, depth)
        return segment_is_heavy(tokens[nested:], depth + 1)
    if executable in SHELLS:
        return shell_nested_command(tokens, index, depth)
    if executable in (".", "source"):
        if index + 1 >= len(tokens):
            return False
        source_target = tokens[index + 1]
        if source_target in SAFE_SOURCE_TARGETS:
            return False
        return is_heavy_script(source_target, depth) or is_dynamic_executable(source_target)
    if executable == "eval":
        return command_is_heavy(" ".join(tokens[index + 1 :]), depth + 1)
    if executable in ("rustup", "xcrun"):
        return any(basename(token) in ("cargo", "rustc") for token in tokens[index + 1 :])
    return False


def substitution_bodies(command):
    """Yield balanced `$()` bodies, including straightforward nested parentheses."""
    cursor = 0
    while True:
        start = command.find("$(", cursor)
        if start < 0:
            return
        depth = 1
        index = start + 2
        while index < len(command) and depth:
            if command.startswith("$(", index):
                depth += 1
                index += 2
                continue
            if command[index] == "(":
                depth += 1
            elif command[index] == ")":
                depth -= 1
            index += 1
        if depth:
            yield command[start + 2 :]
            return
        yield command[start + 2 : index - 1]
        cursor = index


def _segment_leading_tool(tokens):
    """Best-effort basename of the executable a segment launches, or None."""
    if not tokens:
        return None
    if tokens[0] == "__MURMUR_UNPARSEABLE__":
        raw = tokens[1].lstrip()
        match = re.match(r"(?:[A-Za-z_][A-Za-z0-9_]*=\S*\s+)*([^\s;|&]+)", raw)
        return basename(match.group(1)) if match else None
    index = skip_assignments(tokens, 0)
    if index >= len(tokens):
        return None
    return basename(tokens[index])


def command_is_heavy(command, depth=0):
    if depth > MAX_DEPTH:
        return True
    if TEST_GUARDIAN_ENV in command:
        return True
    # A command whose every segment leads with a safe non-build tool (gh, grep,
    # rg) cannot launch heavy work. Skip the substitution scan whose only job is
    # to catch build commands hidden in backticks/$() — those are exactly what a
    # gh PR/issue body legitimately contains as free-form text.
    segments = list(command_segments(tokenize(command)))
    if segments and all(
        _segment_leading_tool(segment) in SAFE_NONBUILD_TOOLS for segment in segments
    ):
        return False
    for backtick_body in re.findall(r"`([^`]*)`", command, flags=re.DOTALL):
        if command_is_heavy(backtick_body, depth + 1):
            return True
    # shlex keeps command substitutions inside a double-quoted token. Inspect
    # innermost `$()` bodies explicitly; nested substitutions are found on the
    # recursive pass. Conservatively checking a quoted literal is preferable to
    # allowing an executable indirection around the resource lane.
    for substitution_body in substitution_bodies(command):
        if command_is_heavy(substitution_body, depth + 1):
            return True
    return any(segment_is_heavy(segment, depth) for segment in command_segments(tokenize(command)))


def command_is_dev(command, depth=0):
    if depth > MAX_DEPTH:
        return True
    return any(segment_is_dev(segment, depth) for segment in command_segments(tokenize(command)))


def command_is_dev_in(command, cwd):
    """Classify long-lived dev commands using the hook payload execution directory."""
    previous = Path.cwd()
    try:
        os.chdir(cwd)
        return command_is_dev(command)
    finally:
        os.chdir(previous)


def command_is_heavy_in(command, cwd):
    """Classify using the exact execution directory carried by the hook payload."""
    previous = Path.cwd()
    try:
        os.chdir(cwd)
        return command_is_heavy(command)
    finally:
        os.chdir(previous)


def main(argv):
    if len(argv) != 1:
        print("usage: agent-resource-policy '<shell command>'", file=sys.stderr)
        return 64
    return 2 if command_is_heavy(argv[0]) else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
