# Copyright 2026 The Buildprof Authors.
# SPDX-License-Identifier: Apache-2.0

from __future__ import annotations

import os
from pathlib import Path
import subprocess
import tomllib

import pytest

from .model import load_file_opens, load_perfetto, load_trace_attributes, load_renames

ROOT = Path(__file__).resolve().parents[2]
ZSTD_MAGIC = bytes.fromhex("28b52ffd")


def test_no_command_prints_usage_and_exits_2(buildprof: Path, tmp_path: Path):
    result = subprocess.run(
        [str(buildprof)], cwd=tmp_path, text=True, capture_output=True, timeout=5
    )
    assert result.returncode == 2
    assert "usage:" in result.stderr.lower()


def test_help_shows_recording_and_subcommands_as_alternatives(
    buildprof: Path, tmp_path: Path
):
    result = subprocess.run(
        [str(buildprof), "--help"],
        cwd=tmp_path,
        text=True,
        capture_output=True,
        timeout=5,
    )
    assert result.returncode == 0
    assert "Usage: buildprof [FLAGS] -- <COMMAND>…" in result.stdout
    assert "buildprof [FLAGS] <SUBCOMMAND>" in result.stdout
    assert "Examples:" in result.stdout
    assert "$ buildprof -- cargo build --release" in result.stdout
    assert "$ buildprof open clean-build.buildprof" in result.stdout
    assert result.stdout.index("  record <COMMAND>…") < result.stdout.index("  open ")
    assert "--no-open" in result.stdout
    assert "      --open " not in result.stdout


def test_default_output_is_a_parseable_perfetto_trace(
    buildprof: Path, process_fixture: Path, tmp_path: Path
):
    result = subprocess.run(
        [str(buildprof), "--", str(process_fixture), "single"],
        cwd=tmp_path,
        text=True,
        capture_output=True,
        timeout=10,
    )
    assert result.returncode == 0
    assert load_perfetto(tmp_path / "output.buildprof")


def test_trace_is_zstd_compressed_and_records_its_provenance(
    run_trace, process_fixture: Path
):
    result, trace = run_trace(process_fixture, "single")
    assert result.returncode == 0
    assert trace.read_bytes()[:4] == ZSTD_MAGIC

    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
    attributes = load_trace_attributes(trace)
    assert attributes["buildprof.version"] == manifest["package"]["version"]
    assert attributes["buildprof.trace_format"] == 1
    assert attributes["buildprof.file_events"] == 1
    assert attributes["buildprof.compiler_traces"] == 0


def test_requested_output_path_is_used(run_trace, process_fixture: Path):
    result, trace = run_trace(process_fixture, "single", name="chosen.pftrace")
    assert result.returncode == 0
    assert load_perfetto(trace)


def test_command_exit_status_is_returned_and_trace_is_written(
    run_trace, process_fixture: Path
):
    result, trace = run_trace(process_fixture, "exit", "17")
    assert result.returncode == 17
    processes = load_perfetto(trace)
    assert len(processes) == 1
    assert processes[next(iter(processes))].segments[-1].exit_code == 17


def test_missing_command_is_reported_with_exit_127(run_trace):
    result, trace = run_trace("buildprof-no-such-command")
    assert result.returncode == 127
    assert "could not execute the command (errno 2: no such file or directory)" in result.stderr
    assert trace.exists()


def test_stdout_and_stderr_pass_through(run_trace, process_fixture: Path):
    result, trace = run_trace(process_fixture, "output")
    assert result.returncode == 0
    assert result.stdout == "fixture stdout\n"
    assert "fixture stderr\n" in result.stderr
    assert load_perfetto(trace)


def test_programs_that_hide_work_point_at_troubleshooting(buildprof: Path, tmp_path: Path):
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()
    for name in ("docker", "gradlew"):
        script = bin_dir / name
        script.write_text("#!/bin/sh\nexit 0\n")
        script.chmod(0o755)
    environment = dict(os.environ, PATH=f"{bin_dir}:{os.environ['PATH']}", NO_COLOR="1")

    def record(script: str) -> str:
        result = subprocess.run(
            [str(buildprof), "--no-open", "--", "sh", "-c", script],
            cwd=tmp_path,
            env=environment,
            text=True,
            capture_output=True,
            timeout=10,
        )
        assert result.returncode == 0, result.stderr
        return result.stderr

    stderr = record("docker run image; docker ps; gradlew --no-daemon build")
    assert stderr.count("ran during this build") == 1, stderr
    assert (
        "buildprof: docker ran during this build; "
        "work inside its containers is not recorded"
    ) in stderr
    assert (
        "buildprof: see https://buildprof.lalitm.com/diagnose/containers"
    ) in stderr

    assert "ran during this build" not in record("true")


def test_file_opens_are_recorded_by_default(
    run_trace, process_fixture: Path, tmp_path: Path
):
    opened = tmp_path / "build-input.txt"
    opened.write_text("input")
    result, trace = run_trace(process_fixture, "open-file", str(opened))
    assert result.returncode == 0

    file_opens = load_file_opens(trace)
    matching = [event for event in file_opens if event.path == str(opened)]
    assert matching
    assert all(event.fd >= 0 for event in matching)


@pytest.mark.parametrize("file_events", [True, False])
@pytest.mark.parametrize("flags,exists,exit_code", [
    pytest.param(os.O_RDONLY, True, 0, id="read"),
    pytest.param(os.O_WRONLY | os.O_CREAT | os.O_TRUNC, False, 0, id="create"),
    pytest.param(os.O_RDONLY, False, 115, id="missing"),
])
def test_legacy_open_syscall(
    buildprof: Path, process_fixture: Path, tmp_path: Path,
    file_events: bool, flags: int, exists: bool, exit_code: int,
):
    opened = tmp_path / "legacy-open.txt"
    if exists:
        opened.write_text("input")
    trace = tmp_path / "legacy-open.pftrace"
    result = subprocess.run(
        [str(buildprof), "--no-open", "-o", str(trace),
         *([] if file_events else ["--no-file-events"]), "--",
         str(process_fixture), "legacy-open", opened.name, str(flags)],
        cwd=tmp_path, text=True, capture_output=True, timeout=10,
    )
    if result.returncode == 77:
        pytest.skip("this architecture has no legacy open syscall")
    assert result.returncode == exit_code, result.stderr
    assert opened.exists() == (exit_code == 0)

    events = load_file_opens(trace)
    matching = [event for event in events if event.path == str(opened)]
    if file_events and exit_code == 0:
        assert len(matching) == 1
        assert matching[0].flags == flags
        assert matching[0].fd >= 0
    else:
        assert matching == []
    if not file_events:
        assert events == []


def test_no_file_events_preserves_process_tree_and_exit_status(
    buildprof: Path, process_fixture: Path, tmp_path: Path
):
    from .test_process_tree import assert_structurally_valid, root_of

    opened = tmp_path / "input.txt"
    opened.write_text("input")
    trace = tmp_path / "process-only.buildprof"
    result = subprocess.run(
        [str(buildprof), "--no-file-events", "--no-open", "-o", str(trace), "--",
         "sh", "-c", '"$1" fork-exec && "$1" open-file "$2" && mv "$2" "$2.moved"; exit 17',
         "fixture", str(process_fixture), str(opened)],
        cwd=tmp_path, text=True, capture_output=True, timeout=10,
    )
    assert result.returncode == 17
    assert opened.with_suffix(".txt.moved").read_text() == "input"
    processes = load_perfetto(trace)
    assert_structurally_valid(processes)
    assert root_of(processes).segments[-1].exit_code == 17
    assert any("leaf" in seg.command for proc in processes.values() for seg in proc.segments)
    assert load_file_opens(trace) == []
    assert load_renames(trace) == []
    assert load_trace_attributes(trace)["buildprof.file_events"] == 0


def test_no_file_events_skips_installing_a_seccomp_filter(buildprof: Path, tmp_path: Path):
    def filter_count(command):
        result = subprocess.run(command, text=True, capture_output=True, timeout=10, check=True)
        line = next(line for line in result.stdout.splitlines() if line.startswith("Seccomp_filters:"))
        return int(line.split(":", 1)[1])

    baseline = filter_count(["cat", "/proc/self/status"])
    for flags, expected in [([], baseline + 1), (["--no-file-events"], baseline)]:
        assert filter_count([
            str(buildprof), "--no-open", *flags, "-o", str(tmp_path / "filters.buildprof"),
            "--", "cat", "/proc/self/status",
        ]) == expected
