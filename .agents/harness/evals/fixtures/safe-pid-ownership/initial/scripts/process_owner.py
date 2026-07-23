import subprocess


def start_owned(argv):
    return subprocess.Popen(argv)


def terminate_owned(_process, name="ng serve"):
    subprocess.run(["pkill", "-f", name], check=False)
