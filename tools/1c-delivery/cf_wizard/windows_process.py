"""Launch installed 1C with its own libraries from the frozen Qt app."""

import ctypes
import os
import subprocess
import sys


def run_installed_platform(argv, *, timeout, **options):
    bundle = os.path.normcase(os.path.normpath(sys._MEIPASS))
    environment = os.environ.copy()
    paths = environment.get("PATH", "").split(os.pathsep)
    environment["PATH"] = os.pathsep.join(
        path for path in paths
        if path and (normalized := os.path.normcase(os.path.normpath(path.strip('"')))) != bundle
        and not normalized.startswith(bundle + os.sep)
    )
    set_directory = ctypes.WinDLL("kernel32", use_last_error=True).SetDllDirectoryW
    set_directory.argtypes = (ctypes.c_wchar_p,)
    set_directory.restype = ctypes.c_int
    if not set_directory(None):
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        process = subprocess.Popen(argv, env=environment, **options)
    finally:
        set_directory(sys._MEIPASS)

    with process:
        try:
            output, errors = process.communicate(timeout=timeout)
        except subprocess.TimeoutExpired as exc:
            process.kill()
            output, errors = process.communicate()
            raise subprocess.TimeoutExpired(argv, timeout, output=output, stderr=errors) from exc
    return subprocess.CompletedProcess(argv, process.returncode, output, errors)
