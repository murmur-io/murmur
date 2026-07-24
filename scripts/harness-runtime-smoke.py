#!/usr/bin/env python3
"""Exclusive, task-owned Tauri boot smoke for the development harness.

It never frees a port or kills an unknown process. The installed Murmur app therefore
wins over this smoke: if :1420 or :8765 is owned, the result is BLOCKED with evidence.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
from datetime import datetime, timezone
from pathlib import Path


DEV_DEK = "0123456789abcdef" * 4


def utc_now() -> str:
    return datetime.now(timezone.utc).replace(microsecond=0).isoformat().replace("+00:00", "Z")


def repo_root() -> Path:
    value = subprocess.check_output(
        ["git", "rev-parse", "--show-toplevel"], text=True, stderr=subprocess.DEVNULL
    ).strip()
    return Path(value).resolve()


def common_dir(root: Path) -> Path:
    value = subprocess.check_output(
        ["git", "rev-parse", "--path-format=absolute", "--git-common-dir"],
        cwd=str(root),
        text=True,
    ).strip()
    return Path(value).resolve()


def runtime_dir(root: Path) -> Path:
    """Keep harness-owned boot artifacts inside the check's writable runtime."""

    harness_runtime = os.environ.get("MURMUR_HARNESS_RUNTIME_DIR")
    if harness_runtime:
        path = Path(harness_runtime)
        if not path.is_absolute():
            raise ValueError("MURMUR_HARNESS_RUNTIME_DIR must be absolute")
        return path.resolve()
    return common_dir(root) / "agent-harness" / "runtime"


def listener(port: int) -> str | None:
    listening = False
    for family, host in ((socket.AF_INET, "127.0.0.1"), (socket.AF_INET6, "::1")):
        try:
            with socket.socket(family, socket.SOCK_STREAM) as sock:
                sock.settimeout(0.25)
                if sock.connect_ex((host, port)) == 0:
                    listening = True
                    break
        except OSError:
            continue
    if not listening:
        return None
    try:
        result = subprocess.run(
            ["lsof", "-nP", f"-iTCP:{port}", "-sTCP:LISTEN"],
            text=True,
            capture_output=True,
            timeout=3,
            check=False,
        )
        return result.stdout.strip() or f"listener on 127.0.0.1:{port}"
    except (FileNotFoundError, subprocess.TimeoutExpired):
        return f"listener on 127.0.0.1:{port}"


def angular_ready() -> bool:
    for url in ("http://127.0.0.1:1420", "http://localhost:1420"):
        try:
            with urllib.request.urlopen(url, timeout=1) as response:
                if 200 <= response.status < 500:
                    return True
        except Exception:
            continue
    return False


def emit(verdict: str, reason: str, log_path: Path | None, started_at: str) -> int:
    payload = {
        "schema_version": 1,
        "verdict": verdict,
        "reason": reason,
        "started_at": started_at,
        "finished_at": utc_now(),
        "log_path": str(log_path) if log_path else None,
    }
    print(json.dumps(payload, sort_keys=True))
    return 0 if verdict == "PASS" else 2


def main() -> int:
    parser = argparse.ArgumentParser(description="Safely prove that the real Tauri dev app boots")
    parser.add_argument("--timeout", type=int, default=240)
    parser.add_argument("--settle-seconds", type=int, default=10)
    parser.add_argument("--runtime-write-probe", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args()
    started_at = utc_now()

    root = repo_root()
    runtime_root = runtime_dir(root)
    runtime_root.mkdir(parents=True, exist_ok=True)
    run_id = f"boot-{int(time.time())}-{os.getpid()}"
    log_path = runtime_root / f"{run_id}.log"
    if args.runtime_write_probe:
        log_path.write_text("task-private runtime write probe\n", encoding="utf-8")
        return emit("PASS", "task-private runtime log is writable", log_path, started_at)

    for port in (1420, 8765):
        owner = listener(port)
        if owner:
            return emit(
                "BLOCKED",
                f"exclusive runtime port {port} is already owned; refusing to kill or reuse it: {owner}",
                None,
                started_at,
            )

    original_home = Path.home()
    temp_root = Path(tempfile.mkdtemp(prefix=f"murmur-{run_id}-"))
    proc: subprocess.Popen[bytes] | None = None
    try:
        isolated_home = temp_root / "home"
        isolated_home.mkdir(parents=True)
        env = os.environ.copy()
        env.update(
            {
                "HOME": str(isolated_home),
                "CARGO_HOME": env.get("CARGO_HOME", str(original_home / ".cargo")),
                "RUSTUP_HOME": env.get("RUSTUP_HOME", str(original_home / ".rustup")),
                "TMPDIR": str(temp_root / "tmp"),
                "npm_config_cache": str(temp_root / "npm-cache"),
                "MURMUR_DEV_DEK": DEV_DEK,
                "MISTRALRS_METAL_PRECOMPILE": "0",
            }
        )
        Path(env["TMPDIR"]).mkdir(parents=True)

        with log_path.open("wb") as log:
            proc = subprocess.Popen(
                ["npm", "run", "dev"],
                cwd=str(root),
                env=env,
                stdout=log,
                stderr=subprocess.STDOUT,
                start_new_session=True,
            )
            deadline = time.monotonic() + args.timeout
            while time.monotonic() < deadline:
                if proc.poll() is not None:
                    return emit("FAIL", f"dev process exited early with {proc.returncode}", log_path, started_at)
                if angular_ready() and listener(8765):
                    break
                time.sleep(1)
            else:
                return emit("FAIL", "timed out waiting for Angular and the real Rust MCP listener", log_path, started_at)

            time.sleep(max(0, args.settle_seconds))
            if proc.poll() is not None:
                return emit("FAIL", f"dev process exited during settle with {proc.returncode}", log_path, started_at)

        text = log_path.read_text(errors="replace")
        fatal_markers = ("thread 'main' panicked", "fatal runtime error", "Abort trap", "SIGABRT")
        hits = [marker for marker in fatal_markers if marker.lower() in text.lower()]
        if hits:
            return emit("FAIL", f"fatal marker(s) in boot log: {', '.join(hits)}", log_path, started_at)
        return emit("PASS", "Angular served and the real Rust MCP listener stayed alive", log_path, started_at)
    finally:
        if proc is not None and proc.poll() is None:
            try:
                os.killpg(proc.pid, signal.SIGTERM)
                proc.wait(timeout=12)
            except (ProcessLookupError, subprocess.TimeoutExpired):
                try:
                    os.killpg(proc.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    pass
        shutil.rmtree(temp_root, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
