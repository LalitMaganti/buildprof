# Copyright 2026 The Buildprof Authors.
# SPDX-License-Identifier: Apache-2.0

"""Commands as CI runs them: leftover servers, sanitizers and sudo."""

from __future__ import annotations

import os
import shutil
import subprocess
import time
from pathlib import Path

import pytest

from .model import load_perfetto


def record(buildprof: Path, tmp_path: Path, *args: str, timeout: float = 20):
    trace = tmp_path / "trace.pftrace"
    started = time.monotonic()
    result = subprocess.run(
        [str(buildprof), "--no-open", "-o", str(trace), *args],
        cwd=tmp_path,
        text=True,
        capture_output=True,
        timeout=timeout,
    )
    return result, trace, time.monotonic() - started


def test_leftover_process_does_not_hold_the_recording(buildprof: Path, tmp_path: Path):
    marker = tmp_path / "written-later"
    # The leftover's own output is redirected so only buildprof can hold the
    # caller's pipes open.
    script = f"(sleep 2; echo later > {marker}.tmp; mv {marker}.tmp {marker}) >/dev/null 2>&1 & exit 3"
    result, trace, elapsed = record(
        buildprof, tmp_path, "--detach-after-exit", "0", "--", "sh", "-c", script
    )
    assert result.returncode == 3
    assert elapsed < 1.5
    assert "still running after the command exited" in result.stderr
    assert load_perfetto(trace)
    # The leftover is still traced, so its file calls keep working.
    deadline = time.monotonic() + 10
    while not marker.exists() and time.monotonic() < deadline:
        time.sleep(0.1)
    assert marker.read_text() == "later\n"


def asan_program(tmp_path: Path) -> Path:
    compiler = shutil.which("cc")
    if compiler is None:
        pytest.skip("no C compiler")
    source = tmp_path / "leak.c"
    source.write_text(
        "#include <stdlib.h>\n"
        "int main(int argc, char **argv) {\n"
        "  if (argc > 1) { void *volatile p = malloc(64); p = 0; }\n"
        "  return 0;\n"
        "}\n"
    )
    binary = tmp_path / "leak"
    built = subprocess.run(
        [compiler, "-fsanitize=address", "-o", str(binary), str(source)],
        capture_output=True,
        text=True,
    )
    if built.returncode != 0:
        pytest.skip(f"compiler lacks AddressSanitizer: {built.stderr}")
    return binary


@pytest.mark.parametrize("leak", [False, True])
def test_leak_sanitizer_runs_under_recording(buildprof: Path, tmp_path: Path, leak: bool):
    binary = asan_program(tmp_path)
    args = [str(binary)] + (["leak"] if leak else [])
    result, trace, _ = record(buildprof, tmp_path, "--", *args)
    assert "does not work under ptrace" not in result.stderr
    assert result.returncode == (1 if leak else 0)
    assert ("detected memory leaks" in result.stderr) == leak
    processes = load_perfetto(trace)
    root = next(process for process in processes.values() if process.parent_pid == 0)
    assert root.segments[-1].exit_code == (1 if leak else 0)


@pytest.mark.skipif(os.geteuid() != 0, reason="--run-as needs root")
def test_run_as_lets_setuid_programs_work(buildprof: Path, tmp_path: Path):
    user = os.environ.get("SUDO_USER") or os.environ.get("BUILDPROF_TEST_USER")
    if not user or shutil.which("sudo") is None:
        pytest.skip("set SUDO_USER or BUILDPROF_TEST_USER to a user with passwordless sudo")
    result, trace, _ = record(
        buildprof, tmp_path, "--run-as", user, "--", "sh", "-c", "id -un; sudo -n id -u"
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout.split() == [user, "0"]
    assert trace.stat().st_uid != 0


def test_allowing_any_tracer_does_not_stop_recording(buildprof: Path, tmp_path: Path):
    # Valgrind does this for its gdb server; only LeakSanitizer is let go.
    script = (
        "import ctypes; libc = ctypes.CDLL(None); "
        "libc.prctl(0x59616D61, ctypes.c_ulong(2**64 - 1), 0, 0, 0); "
        "open('after-prctl.txt', 'w').write('ok')"
    )
    result, trace, _ = record(buildprof, tmp_path, "--", "python3", "-c", script)
    assert result.returncode == 0, result.stderr
    assert "LeakSanitizer" not in result.stderr
    assert (tmp_path / "after-prctl.txt").read_text() == "ok"


def test_capabilities_let_setuid_programs_work(buildprof: Path, tmp_path: Path):
    if os.geteuid() == 0 or shutil.which("setcap") is None:
        pytest.skip("needs a non-root user and setcap")
    if subprocess.run(["sudo", "-n", "true"], capture_output=True).returncode != 0:
        pytest.skip("needs passwordless sudo")
    copy = tmp_path / "buildprof"
    shutil.copy(buildprof, copy)
    subprocess.run(["sudo", "-n", "setcap", "cap_sys_ptrace,cap_sys_admin+ep", str(copy)], check=True)
    env = {key: value for key, value in os.environ.items() if key != "TERM"}
    result = subprocess.run(
        [str(copy), "--no-open", "-o", str(tmp_path / "trace.pftrace"), "--",
         "sh", "-c", 'sudo -n id -u; id -u; echo "TERM=${TERM-unset}"'],
        cwd=tmp_path, env=env, text=True, capture_output=True, timeout=20,
    )
    assert result.returncode == 0, result.stderr
    assert result.stdout.split() == ["0", str(os.geteuid()), "TERM=unset"]
