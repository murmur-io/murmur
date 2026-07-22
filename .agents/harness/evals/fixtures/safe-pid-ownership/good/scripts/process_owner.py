import os
import signal
import subprocess


def start_owned(argv):
    return subprocess.Popen(argv, start_new_session=True)


def terminate_owned(process):
    if process.poll() is not None:
        return
    os.killpg(os.getpgid(process.pid), signal.SIGTERM)
