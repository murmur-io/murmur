import os
import signal
import subprocess


def start_owned(argv):
    return subprocess.Popen(argv)


def terminate_owned(process):
    if process.poll() is None:
        os.kill(process.pid, signal.SIGTERM)
