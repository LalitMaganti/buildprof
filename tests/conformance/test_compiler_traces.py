# Copyright 2026 The Buildprof Authors.
# SPDX-License-Identifier: Apache-2.0

"""Compiler tracing follows the toolchain the build actually runs.

Rust self-profile data needs a nightly compiler. The build decides which
toolchain runs, so the wrapper must find out from the compiler it launches
rather than from whatever `rustc` is the default at startup.
"""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess

import pytest

from .model import count_compiler_events, load_perfetto, load_trace_attributes

# How a build can pick the nightly toolchain without changing the default.
NIGHTLY_SELECTIONS = {
    "cargo-plus-nightly": (["cargo", "+nightly", "build", "--offline"], {}, None),
    "rustup-toolchain-variable": (
        ["cargo", "build", "--offline"],
        {"RUSTUP_TOOLCHAIN": "nightly"},
        None,
    ),
    "rust-toolchain-file": (
        ["cargo", "build", "--offline"],
        {},
        "[toolchain]\nchannel = \"nightly\"\n",
    ),
}


def _require(tool: str) -> None:
    if shutil.which(tool) is None:
        message = f"{tool} is not installed in this environment"
        if os.environ.get("BUILDPROF_REQUIRE_TOOLS"):
            pytest.fail(message)
        pytest.skip(message)


def _rustc_version(*selector: str) -> str:
    result = subprocess.run(
        ["rustc", *selector, "--version"], text=True, capture_output=True
    )
    return result.stdout if result.returncode == 0 else ""


def _require_nightly() -> None:
    _require("cargo")
    if "nightly" not in _rustc_version("+nightly"):
        message = "the nightly Rust toolchain is not installed"
        if os.environ.get("BUILDPROF_REQUIRE_TOOLS"):
            pytest.fail(message)
        pytest.skip(message)


def _cargo_project(root: Path, toolchain_file: str | None) -> None:
    (root / "src").mkdir()
    (root / "src/main.rs").write_text("fn main() { println!(\"{}\", 3); }\n")
    (root / "Cargo.toml").write_text(
        "[package]\nname = \"bt\"\nversion = \"0.0.0\"\nedition = \"2021\"\n"
        "[dependencies]\n"
    )
    if toolchain_file is not None:
        (root / "rust-toolchain.toml").write_text(toolchain_file)


def _record(buildprof: Path, project: Path, command: list[str], extra_env: dict) -> Path:
    trace = project.parent / f"{project.name}.pftrace"
    env = dict(os.environ, LC_ALL="C", **extra_env)
    result = subprocess.run(
        [str(buildprof), "--compiler-traces", "-o", str(trace), "--", *command],
        cwd=project,
        text=True,
        capture_output=True,
        timeout=300,
        env=env,
    )
    assert result.returncode == 0, f"build failed:\n{result.stdout}{result.stderr}"
    assert trace.is_file(), "no trace was written"
    return trace


def _rustc_processes(trace: Path) -> list:
    return [
        process
        for process in load_perfetto(trace).values()
        if any(segment.name == "rustc" for segment in process.segments)
    ]


@pytest.mark.parametrize("selection", sorted(NIGHTLY_SELECTIONS))
def test_nightly_chosen_by_the_build_records_compiler_events(
    selection, buildprof: Path, tmp_path: Path
):
    _require_nightly()
    command, extra_env, toolchain_file = NIGHTLY_SELECTIONS[selection]
    project = tmp_path / selection
    project.mkdir()
    _cargo_project(project, toolchain_file)

    trace = _record(buildprof, project, command, extra_env)

    assert load_trace_attributes(trace)["buildprof.compiler_traces"] == 1
    assert _rustc_processes(trace), "the build compiled nothing"
    assert count_compiler_events(trace, "Rust") > 0, (
        f"nightly rustc selected by {selection} produced no compiler events"
    )


def test_default_toolchain_builds_and_traces_only_if_nightly(
    buildprof: Path, tmp_path: Path
):
    """A stable compiler must not be handed `-Z` flags; a nightly one must."""
    _require("cargo")
    project = tmp_path / "default"
    project.mkdir()
    _cargo_project(project, None)

    trace = _record(buildprof, project, ["cargo", "build", "--offline"], {})

    assert _rustc_processes(trace), "the build compiled nothing"
    default_is_nightly = "nightly" in _rustc_version()
    events = count_compiler_events(trace, "Rust")
    if default_is_nightly:
        assert events > 0, "the default nightly rustc produced no compiler events"
    else:
        assert events == 0, f"a stable rustc produced {events} compiler events"
