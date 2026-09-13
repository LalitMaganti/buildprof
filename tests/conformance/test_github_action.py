# Copyright 2026 The Buildprof Authors.
# SPDX-License-Identifier: Apache-2.0

from __future__ import annotations

import os
from pathlib import Path
import subprocess
from textwrap import dedent

import pytest


ROOT = Path(__file__).resolve().parents[2]
RECORD = ROOT / ".github/actions/record-build/record.sh"


@pytest.mark.parametrize(
    ("command", "status"),
    [
        (
            dedent("""\
                printf "generated input" > "input file"
                cp "input file" "output file"
            """),
            0,
        ),
        ("exit 7", 7),
        ("false | true", 1),
    ],
)
def test_action_records_build(
    buildprof: Path, tmp_path: Path, command: str, status: int
) -> None:
    trace = tmp_path / "recording with spaces.buildprof"
    outputs = tmp_path / "outputs"
    result = subprocess.run(
        ["bash", str(RECORD)],
        cwd=tmp_path,
        env={
            **os.environ,
            "BUILDPROF_BINARY": str(buildprof),
            "BUILDPROF_TRACE": str(trace),
            "BUILDPROF_COMMAND": command,
            "GITHUB_OUTPUT": str(outputs),
        },
        capture_output=True,
        text=True,
        timeout=30,
    )

    assert result.returncode == status, result.stderr
    values = dict(line.split("=", 1) for line in outputs.read_text().splitlines())
    assert values["status"] == str(status)
    assert int(values["elapsed"]) >= 0
    assert values["trace"] == str(trace)
    assert trace.stat().st_size > 0
    if status == 0:
        assert (tmp_path / "output file").read_text() == "generated input"


def test_action_reports_recorder_failure(tmp_path: Path) -> None:
    outputs = tmp_path / "outputs"
    result = subprocess.run(
        ["bash", str(RECORD)],
        env={
            **os.environ,
            "BUILDPROF_BINARY": str(tmp_path / "missing-buildprof"),
            "BUILDPROF_TRACE": str(tmp_path / "missing.buildprof"),
            "BUILDPROF_COMMAND": "true",
            "GITHUB_OUTPUT": str(outputs),
        },
        capture_output=True,
        text=True,
        timeout=10,
    )

    assert result.returncode == 127
    values = dict(line.split("=", 1) for line in outputs.read_text().splitlines())
    assert values["status"] == "127"
    assert "trace" not in values
