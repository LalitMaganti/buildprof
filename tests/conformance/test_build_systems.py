"""Diff stable tool and artifact-flow summaries for supported build systems.

Refresh expectations with:

    dev/test tests/conformance/test_build_systems.py --update-expectations
"""

from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path

import pytest

from .model import dependency_edges, load_perfetto

EXPECTED = Path(__file__).resolve().parent / "expected"


# Exercise parallel compiles, archiving, linking, and renamed outputs.

SOURCES = {
    "a.c": "int a(void) { return 1; }\n",
    "b.c": "int b(void) { return 2; }\n",
    "main.c": "int a(void); int b(void);\nint main(void) { return a() + b(); }\n",
}


def _write_sources(root: Path) -> None:
    for name, text in SOURCES.items():
        (root / name).write_text(text)


def _make_project(root: Path) -> list[str]:
    _write_sources(root)
    (root / "Makefile").write_text(
        "all: prog\n"
        "a.o: a.c\n\t$(CC) -c a.c -o a.o\n"
        "b.o: b.c\n\t$(CC) -c b.c -o b.o\n"
        "libab.a: a.o b.o\n\t$(AR) rcs libab.a a.o b.o\n"
        "prog: main.c libab.a\n\t$(CC) main.c libab.a -o prog\n"
    )
    return ["make", "-j2"]


def _cmake_project(root: Path) -> list[str]:
    _write_sources(root)
    (root / "CMakeLists.txt").write_text(
        "cmake_minimum_required(VERSION 3.16)\n"
        "project(bt C)\n"
        "add_library(ab STATIC a.c b.c)\n"
        "add_executable(prog main.c)\n"
        "target_link_libraries(prog ab)\n"
    )
    subprocess.run(
        ["cmake", "-G", "Ninja", "-B", "b", "-S", "."],
        cwd=root,
        check=True,
        capture_output=True,
    )
    return ["ninja", "-C", "b"]


def _meson_project(root: Path) -> list[str]:
    _write_sources(root)
    (root / "meson.build").write_text(
        "project('bt', 'c')\n"
        "ab = static_library('ab', 'a.c', 'b.c')\n"
        "executable('prog', 'main.c', link_with: ab)\n"
    )
    subprocess.run(
        ["meson", "setup", "b"], cwd=root, check=True, capture_output=True
    )
    return ["ninja", "-C", "b"]


def _cargo_project(root: Path) -> list[str]:
    (root / "src").mkdir()
    (root / "src/main.rs").write_text("fn main() { println!(\"{}\", 3); }\n")
    (root / "Cargo.toml").write_text(
        "[package]\nname = \"bt\"\nversion = \"0.0.0\"\nedition = \"2021\"\n"
        "[dependencies]\n"
    )
    return ["cargo", "build", "--offline"]


def _go_project(root: Path) -> list[str]:
    (root / "go.mod").write_text("module bt\n\ngo 1.21\n")
    (root / "main.go").write_text(
        "package main\n\nimport \"fmt\"\n\nfunc main() { fmt.Println(3) }\n"
    )
    return ["go", "build", "-o", "prog", "."]


CASES = {
    "make": (_make_project, ["make", "cc", "ar"]),
    "cmake-ninja": (_cmake_project, ["ninja", "cmake", "cc", "ar"]),
    "meson-ninja": (_meson_project, ["ninja", "meson", "cc", "ar"]),
    "cargo": (_cargo_project, ["cargo", "rustc"]),
    "go": (_go_project, ["go", "compile"]),
}

REQUIRED_TOOLS = {
    "make": ["make", "cc", "ar"],
    "cmake-ninja": ["cmake", "ninja", "cc"],
    "meson-ninja": ["meson", "ninja", "cc"],
    "cargo": ["cargo"],
    "go": ["go"],
}


def _summarise(trace: Path) -> str:
    """Describe stable tool and artifact-flow relationships in the trace."""
    everything = load_perfetto(trace)
    # Build actions only. The recorder now keeps forks that never exec'd, and
    # how many subshells a build system spawns is neither stable nor what
    # these tests are about.
    processes = {
        pid: p for pid, p in everything.items() if any(s.execed for s in p.segments)
    }
    tools = sorted({segment.name for p in processes.values() for segment in p.segments})

    def extension(path: str) -> str:
        base = path.rsplit("/", 1)[-1]
        return "." + base.rsplit(".", 1)[-1] if "." in base else "(none)"

    def tool_of(pid: int) -> str:
        return processes[pid].segments[0].name

    edges = sorted(
        {
            (tool_of(e.producer_pid), tool_of(e.consumer_pid), extension(e.path))
            for e in dependency_edges(trace)
            if e.producer_pid in processes and e.consumer_pid in processes
        }
    )
    lines = ["tools:"]
    lines += [f"  {tool}" for tool in tools]
    lines.append("edges:")
    lines += [f"  {producer} -> {consumer} [{ext}]" for producer, consumer, ext in edges]
    return "\n".join(lines) + "\n"


@pytest.mark.parametrize("case", sorted(CASES))
def test_build_system(case, buildprof: Path, tmp_path: Path, request):
    for tool in REQUIRED_TOOLS[case]:
        if shutil.which(tool) is None:
            message = f"{tool} is not installed in this environment"
            if os.environ.get("BUILDPROF_REQUIRE_TOOLS"):
                pytest.fail(message)
            pytest.skip(message)

    build, _ = CASES[case]
    project = tmp_path / case
    project.mkdir()
    command = build(project)

    trace = tmp_path / f"{case}.pftrace"
    env = dict(os.environ, LC_ALL="C")
    result = subprocess.run(
        [str(buildprof), "-o", str(trace), "--", *command],
        cwd=project,
        text=True,
        capture_output=True,
        timeout=300,
        env=env,
    )
    assert result.returncode == 0, f"{case} build failed:\n{result.stdout}{result.stderr}"
    assert trace.is_file(), f"{case} produced no trace"

    actual = _summarise(trace)
    expectation = EXPECTED / f"{case}.txt"
    if request.config.getoption("--update-expectations"):
        expectation.parent.mkdir(parents=True, exist_ok=True)
        expectation.write_text(actual)
        pytest.skip(f"updated {expectation.name}")
    assert expectation.is_file(), (
        f"no expectation for {case}; run with --update-expectations\n\n{actual}"
    )
    assert actual == expectation.read_text(), (
        f"{case} summary changed.\n\n--- expected ---\n{expectation.read_text()}"
        f"\n--- actual ---\n{actual}"
    )
