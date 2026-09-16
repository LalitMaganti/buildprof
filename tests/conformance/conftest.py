# Copyright 2026 The Buildprof Authors.
# SPDX-License-Identifier: Apache-2.0

from __future__ import annotations

import os
from pathlib import Path
import subprocess

import pytest


ROOT = Path(__file__).resolve().parents[2]


# A compiler launcher in front of the toolchain runs as part of the build and
# lands in the recording, so a diff test would describe whichever launcher the
# machine happens to have installed. Tests that want one put it on PATH
# themselves.
LAUNCHERS = frozenset({"ccache", "distcc", "icecc", "sccache"})


@pytest.fixture
def plain_toolchain(monkeypatch: pytest.MonkeyPatch) -> None:
    """Takes any compiler launcher off `PATH` for the whole test.

    Not just for the recording: CMake and Meson resolve the compiler when they
    configure, and write the path they found into the build files, so a
    launcher they saw would be used however the recording is run.
    """
    directories = [
        directory
        for directory in os.environ.get("PATH", "").split(os.pathsep)
        if not LAUNCHERS & set(Path(directory).parts)
    ]
    monkeypatch.setenv("PATH", os.pathsep.join(directories))
    # Meson goes looking for a launcher binary rather than taking the one in
    # front of the compiler on PATH, and only leaves the compiler alone when
    # it is told which one to use.
    monkeypatch.setenv("CC", "cc")
    monkeypatch.setenv("CXX", "c++")


def pytest_addoption(parser: pytest.Parser) -> None:
    parser.addoption(
        "--update-expectations",
        action="store_true",
        help="rewrite the build-system diff-test expectations from this run",
    )


@pytest.fixture(scope="session")
def buildprof() -> Path:
    configured = os.environ.get("BUILDPROF_BIN")
    binary = Path(configured) if configured else ROOT / "target/debug/buildprof"
    binary = binary.resolve()
    if not binary.is_file():
        pytest.fail(
            f"buildprof binary not found at {binary}; run `cargo build` or set BUILDPROF_BIN"
        )
    return binary


@pytest.fixture(scope="session")
def process_fixture(tmp_path_factory: pytest.TempPathFactory) -> Path:
    output = tmp_path_factory.mktemp("fixture-bin") / "process-fixture"
    source = ROOT / "tests/fixtures/process_fixture.c"
    result = subprocess.run(
        ["cc", "-std=c11", "-O0", "-Wall", "-Wextra", "-o", str(output), str(source)],
        text=True,
        capture_output=True,
    )
    if result.returncode:
        pytest.fail(f"could not compile process fixture:\n{result.stdout}{result.stderr}")
    return output


@pytest.fixture
def run_trace(buildprof: Path, tmp_path: Path):
    def run(*command: str, name: str = "trace.pftrace"):
        trace = tmp_path / name
        result = subprocess.run(
            [str(buildprof), "-o", str(trace), "--", *map(str, command)],
            cwd=tmp_path,
            text=True,
            capture_output=True,
            timeout=10,
        )
        return result, trace

    return run
