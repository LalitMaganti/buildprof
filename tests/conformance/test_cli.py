# Copyright 2026 The Buildprof Authors.
# SPDX-License-Identifier: Apache-2.0

from __future__ import annotations

from pathlib import Path
import subprocess
import tomllib

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
