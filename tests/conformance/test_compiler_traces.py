# Copyright 2026 The Buildprof Authors.
# SPDX-License-Identifier: Apache-2.0

"""Compiler tracing follows the toolchain the build actually runs.

Rust self-profile data needs a nightly compiler. The build decides which
toolchain runs, so the wrapper must find out from the compiler it launches
rather than from whatever `rustc` is the default at startup. The build also
decides what stands in for the compiler, which may be a launcher that goes
looking for the compiler itself.
"""

from __future__ import annotations

import os
from pathlib import Path
import shutil
import subprocess

import pytest

from .model import count_compiler_events, load_perfetto, load_trace_attributes

# Long enough for a cold ccache to compile a one-line program, short enough
# that a wrapper that never stops is reported rather than waited on.
CCACHE_TIMEOUT_SECONDS = 60

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


def _record(
    buildprof: Path,
    project: Path,
    command: list[str],
    extra_env: dict,
    timeout: int = 300,
) -> Path:
    trace = project.parent / f"{project.name}.pftrace"
    env = dict(os.environ, LC_ALL="C", **extra_env)
    result = subprocess.run(
        [str(buildprof), "--compiler-traces", "-o", str(trace), "--", *command],
        cwd=project,
        text=True,
        capture_output=True,
        timeout=timeout,
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


def test_ccache_wrappers_on_path_do_not_loop(buildprof: Path, tmp_path: Path):
    """A compiler launcher must not find the wrapper that launched it.

    ccache is normally used through a directory of symlinks named after the
    compilers, early on `PATH`. It works out which compiler to run by looking
    its own name up on `PATH` again, skipping only itself, so it finds the
    wrapper directory and runs the wrapper, which hands straight back to
    ccache. Left alone the two never stop.
    """
    _require("ccache")
    _require("clang")
    project = tmp_path / "ccache"
    project.mkdir()
    (project / "hello.c").write_text("int main(void) { return 0; }\n")
    launchers = tmp_path / "ccache-bin"
    launchers.mkdir()
    ccache = shutil.which("ccache")
    for name in ("clang", "clang++"):
        (launchers / name).symlink_to(ccache)

    trace = _record(
        buildprof,
        project,
        ["clang", "hello.c", "-o", "hello"],
        {
            "PATH": f"{launchers}{os.pathsep}{os.environ['PATH']}",
            "CCACHE_DIR": str(tmp_path / "cache"),
        },
        timeout=CCACHE_TIMEOUT_SECONDS,
    )

    assert (project / "hello").is_file(), "the build produced nothing"
    assert count_compiler_events(trace, "Clang") > 0, (
        "compiling through ccache recorded no Clang events"
    )


def _clang_project(root: Path) -> Path:
    project = root / "project"
    (project / "a").mkdir(parents=True)
    (project / "b").mkdir()
    (project / "a/hello.c").write_text("int main(void) { return 0; }\n")
    (project / "b/hello.c").write_text("int helper(void) { return 1; }\n")
    return project


def _clang_processes(trace: Path) -> list:
    return [
        process
        for process in load_perfetto(trace).values()
        if any(segment.name in ("clang", "clang++") for segment in process.segments)
    ]


def test_clang_invoked_by_relative_path_records_compiler_events(
    buildprof: Path, tmp_path: Path
):
    """GN and many CMake toolchains run Clang by path, which no wrapper on
    `PATH` can intercept."""
    _require("clang")
    project = _clang_project(tmp_path)
    toolchain = project / "toolchain/bin"
    toolchain.mkdir(parents=True)
    (toolchain / "clang").symlink_to(shutil.which("clang"))

    trace = _record(
        buildprof,
        project,
        ["toolchain/bin/clang", "-c", "a/hello.c", "-o", "a/hello.o"],
        {},
    )

    assert (project / "a/hello.o").is_file(), "the build produced nothing"
    assert count_compiler_events(trace, "Clang") > 0, (
        "Clang invoked by a relative path recorded no Clang events"
    )
    assert not (project / "a/hello.json").exists()


def test_clang_the_build_puts_on_path_is_traced(buildprof: Path, tmp_path: Path):
    """A build that puts its own toolchain ahead of everything on `PATH`,
    including any wrapper, runs that Clang and it is still traced."""
    _require("clang")
    project = _clang_project(tmp_path)
    toolchain = project / "toolchain"
    toolchain.mkdir()
    marker = tmp_path / "pinned-clang-ran"
    pinned = toolchain / "clang"
    pinned.write_text(
        f'#!/bin/sh\ntouch "{marker}"\nexec "{shutil.which("clang")}" "$@"\n'
    )
    pinned.chmod(0o755)

    trace = _record(
        buildprof,
        project,
        ["sh", "-c", f'PATH="{toolchain}:$PATH" exec clang -c a/hello.c -o a/hello.o'],
        {},
    )

    assert marker.exists(), "the build's own clang was replaced"
    assert count_compiler_events(trace, "Clang") > 0


def test_clang_traces_requested_by_the_build_are_left_alone(
    buildprof: Path, tmp_path: Path
):
    """A build's own `-ftime-trace` and `CCC_OVERRIDE_OPTIONS` keep working."""
    _require("clang")
    project = _clang_project(tmp_path)
    (project / "a/hello.c").write_text(
        "#ifndef FROM_OVERRIDE\n#error override lost\n#endif\n"
        "int main(void) { return 0; }\n"
    )

    trace = _record(
        buildprof,
        project,
        [
            "sh",
            "-c",
            "clang -ftime-trace -c a/hello.c -o a/hello.o"
            " && clang -c b/hello.c -o b/hello.o",
        ],
        {"CCC_OVERRIDE_OPTIONS": "# +-DFROM_OVERRIDE"},
    )

    assert (project / "a/hello.json").is_file(), "the build's own trace moved"
    assert count_compiler_events(trace, "Clang") > 0, "b/hello.c was not traced"
    assert len(_clang_processes(trace)) == 2
