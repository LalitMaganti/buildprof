from __future__ import annotations

from pathlib import Path
import subprocess

from .model import load_file_opens, load_perfetto


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
