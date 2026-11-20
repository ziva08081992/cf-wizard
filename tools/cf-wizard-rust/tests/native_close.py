"""Exercise the actual desktop WM_CLOSE on a synthetic local XML export."""
import argparse
import ctypes
from ctypes import wintypes
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import subprocess
import time


def wait_for(check, proc, label, timeout=60):
    start = time.perf_counter()
    while time.perf_counter() - start < timeout:
        value = check()
        if value:
            return value
        if proc.poll() is not None:
            raise RuntimeError(f"Worker exited {proc.returncode} before {label}")
        time.sleep(0.025)
    raise TimeoutError(label)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", type=Path, required=True)
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--profile", action="store_true")
    args = parser.parse_args()
    root = args.out.resolve()
    exe = args.exe.resolve(strict=True)
    fixture = args.fixture.resolve(strict=True)
    root.mkdir(parents=True, exist_ok=False)
    if not (fixture / "Configuration.xml").is_file():
        raise ValueError("Expected a synthetic configuration export")
    user32 = ctypes.WinDLL("user32", use_last_error=True)
    window_fn = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    user32.EnumWindows.argtypes = [window_fn, wintypes.LPARAM]
    user32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
    user32.GetWindowTextW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
    user32.IsWindowVisible.argtypes = [wintypes.HWND]
    user32.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
    user32.SendMessageTimeoutW.argtypes = [
        wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM,
        wintypes.UINT, wintypes.UINT, ctypes.POINTER(ctypes.c_size_t),
    ]
    user32.SendMessageTimeoutW.restype = ctypes.c_ssize_t
    env = dict(os.environ, CF_WIZARD_PROFILE="1" if args.profile else "0")
    proc = subprocess.Popen([str(exe), "--data-dir", str(root), "--export", str(fixture),
                             "--shutdown-smoke"], env=env)

    def find_window():
        handles = []

        @window_fn
        def visit(hwnd, _):
            pid = wintypes.DWORD()
            user32.GetWindowThreadProcessId(hwnd, ctypes.byref(pid))
            if pid.value == proc.pid:
                title = ctypes.create_unicode_buffer(256)
                user32.GetWindowTextW(hwnd, title, len(title))
                if title.value == "Мастер технической CF · Rust":
                    handles.append(hwnd)
            return True

        user32.EnumWindows(visit, 0)
        return handles[0] if len(handles) == 1 else None

    def post_close(hwnd):
        if not user32.PostMessageW(hwnd, 0x10, 0, 0):
            raise ctypes.WinError(ctypes.get_last_error())

    hwnd = None
    try:
        hwnd = wait_for(find_window, proc, "main window")
        wait_for((root / "close-ready").exists, proc, "ready preview")
        if (root / "close-ready").read_bytes() != b"retained-preview":
            raise RuntimeError("Missing prepared plan")
        before = time.perf_counter()
        post_close(hwnd)
        wait_for((root / "close-blocked").exists, proc, "blocked worker Drop")
        wait_for(lambda: not user32.IsWindowVisible(hwnd), proc, "hidden window")
        post_close(hwnd)
        wait_for((root / "close-repeated").exists, proc, "repeat CloseRequested")
        started = time.perf_counter()
        result = ctypes.c_size_t()
        if not user32.SendMessageTimeoutW(hwnd, 0, 0, 0, 2, 1500, ctypes.byref(result)):
            raise RuntimeError("UI did not acknowledge WM_NULL while Drop was blocked")
        heartbeat = time.perf_counter() - started
        (root / "close-release").write_bytes(b"ok")
        if proc.wait(timeout=25) != 0:
            raise RuntimeError(f"Unexpected exit code {proc.returncode}")
        exit_utc = datetime.now(timezone.utc).isoformat()
        close_seconds = time.perf_counter() - before
        settings = json.loads((root / "paths.json").read_text(encoding="utf-8"))
        if settings["values"].get("destination") != "close-smoke-destination":
            raise RuntimeError("Dirty setting did not survive close")
        logs = list((root / "logs").glob("*-shutdown-*.jsonl"))
        if len(logs) != int(args.profile):
            raise RuntimeError("Unexpected shutdown trace count")
        phases = []
        if logs:
            phases = [json.loads(line)["phase"] for line in logs[0].read_text("utf-8").splitlines()]
            for phase in ("requested", "flush-start", "handoff-start", "handoff-accepted",
                          "repeat-close", "worker-start", "plan-release-start", "plan-release-end",
                          "source-provider-refs-release-start", "source-provider-refs-release-end",
                          "worker-done", "loop-destroyed"):
                if phases.count(phase) != 1:
                    raise RuntimeError(f"Expected one {phase}: {phases}")
            if not ({"flush-clean", "flush-saved"} & set(phases)):
                raise RuntimeError(f"Missing settings save outcome: {phases}")
        evidence = dict(passed=True, pid=proc.pid, profile=args.profile,
                        ready_plan=True, close_requests=2, heartbeat_seconds=heartbeat,
                        exit_code=proc.returncode, exit_utc=exit_utc, close_seconds=close_seconds,
                        saved_destination=settings["values"]["destination"], phases=phases)
        (root / "receipt.json").write_text(json.dumps(evidence, indent=2), encoding="utf-8")
        print(json.dumps(evidence))
    finally:
        if proc.poll() is None:
            (root / "close-release").touch()
            if hwnd is not None:
                post_close(hwnd)


if __name__ == "__main__":
    main()
