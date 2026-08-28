from __future__ import annotations

import os
from pathlib import Path
import subprocess

import pytest


ROOT = Path(__file__).resolve().parents[2]


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
